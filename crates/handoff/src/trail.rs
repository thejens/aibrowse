//! What the user did during a handoff, so the agent can pick up where they left off:
//! pages visited (URL and title), elements clicked (by their text), and how much was
//! typed. Typed text itself is never recorded: it may be a password.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibrowse_cdp::Session;
use serde_json::json;
use tokio::sync::watch;

use crate::Status;
use crate::input::ClientEvent;

/// Entries are short human-readable lines, oldest first.
pub(crate) type Trail = Arc<Mutex<Vec<String>>>;

const MAX_ENTRIES: usize = 60;

pub(crate) fn push(trail: &Trail, entry: String) {
    let mut trail = trail
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Collapse repeats (several keystrokes in a row become one entry).
    if let Some(last) = trail.last_mut()
        && let (Some(prev), Some(now)) = (typed_count(last), typed_count(&entry))
    {
        *last = format!("typed {} characters", prev + now);
        return;
    }
    // The same page again (a reload, a redirect through itself) says nothing new.
    if trail.last() == Some(&entry) {
        return;
    }
    if trail.len() < MAX_ENTRIES {
        trail.push(entry);
    }
}

/// A visited page as the trail names it: its address without query or fragment. Sign-in
/// pages carry kilobytes of tokens in their queries (Okta's, Microsoft's), which made a
/// handoff's trail, and every resume note quoting it, mostly noise.
fn page_of(url: &str) -> String {
    let page = url.split(['?', '#']).next().unwrap_or(url);
    page.chars().take(MAX_PAGE_CHARS).collect()
}

const MAX_PAGE_CHARS: usize = 120;

fn typed_count(entry: &str) -> Option<usize> {
    entry
        .strip_prefix("typed ")?
        .strip_suffix(" characters")?
        .parse()
        .ok()
}

/// Records main-frame navigations until the handoff resolves.
pub(crate) fn record_navigation(
    session: Session,
    trail: Trail,
    mut status: watch::Receiver<Status>,
) {
    tokio::spawn(async move {
        let mut events = session.events();
        loop {
            let event = tokio::select! {
                event = events.recv() => event,
                _ = status.wait_for(|s| *s != Status::Pending) => return,
            };
            let Some(event) = event else { return };
            let url = match event.method.as_str() {
                "Page.frameNavigated" if event.params["frame"]["parentId"].is_null() => {
                    event.params["frame"]["url"].as_str().map(str::to_owned)
                }
                "Page.navigatedWithinDocument" => event.params["url"].as_str().map(str::to_owned),
                _ => None,
            };
            if let Some(url) = url {
                // The title arrives after the document starts loading.
                tokio::time::sleep(Duration::from_millis(800)).await;
                let title = current_title(&session).await.unwrap_or_default();
                push(&trail, format!("visited {} \"{title}\"", page_of(&url)));
            }
        }
    });
}

async fn current_title(session: &Session) -> Option<String> {
    let history = session
        .call("Page.getNavigationHistory", json!({}))
        .await
        .ok()?;
    let index = usize::try_from(history["currentIndex"].as_u64()?).ok()?;
    history["entries"][index]["title"]
        .as_str()
        .map(str::to_owned)
}

/// Notes a user input event: clicks by what was clicked, typing by length only.
pub(crate) async fn note_input(session: &Session, trail: &Trail, event: &ClientEvent) {
    match event {
        ClientEvent::Mouse { kind, x, y, .. } if kind == "mousePressed" => {
            let target = clicked(session, *x, *y)
                .await
                .unwrap_or_else(|| "the page".into());
            push(trail, format!("clicked {target}"));
        }
        ClientEvent::Text { text } => {
            push(trail, format!("typed {} characters", text.chars().count()))
        }
        ClientEvent::Key { kind, key, .. } if kind == "keyDown" => {
            if key.chars().count() == 1 {
                push(trail, "typed 1 characters".into());
            } else if matches!(key.as_str(), "Enter" | "Tab" | "Escape") {
                push(trail, format!("pressed {key}"));
            }
        }
        _ => {}
    }
}

/// A short description of the element at a viewport point, e.g. `button "Sign in"`.
async fn clicked(session: &Session, x: f64, y: f64) -> Option<String> {
    let metrics = session
        .call("Page.getLayoutMetrics", json!({}))
        .await
        .ok()?;
    let viewport = &metrics["cssVisualViewport"];
    let hit = session
        .call(
            "DOM.getNodeForLocation",
            json!({
                "x": (x + viewport["pageX"].as_f64().unwrap_or(0.0)).round() as i64,
                "y": (y + viewport["pageY"].as_f64().unwrap_or(0.0)).round() as i64,
            }),
        )
        .await
        .ok()?;
    let html = session
        .call(
            "DOM.getOuterHTML",
            json!({ "backendNodeId": hit["backendNodeId"] }),
        )
        .await
        .ok()?;
    Some(describe(html["outerHTML"].as_str()?))
}

/// `<button class="x">Sign <b>in</b></button>` → `button "Sign in"`.
fn describe(html: &str) -> String {
    let tag: String = html
        .trim_start_matches('<')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text.chars().take(60).collect();
    let label = attribute(html, "aria-label").filter(|_| text.is_empty());
    format!("{tag} \"{}\"", label.unwrap_or(text))
}

fn attribute(html: &str, name: &str) -> Option<String> {
    let start = html.find(&format!("{name}=\""))? + name.len() + 2;
    let end = html[start..].find('"')? + start;
    Some(html[start..end].to_owned())
}

/// The trail as a plain list.
pub(crate) fn entries(trail: &Trail) -> Vec<String> {
    trail
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_clicked_elements_by_text() {
        assert_eq!(
            describe(r#"<button class="x">Sign <b>in</b></button>"#),
            r#"button "Sign in""#
        );
        assert_eq!(
            describe(r#"<a aria-label="Home" href="/"><svg></svg></a>"#),
            r#"a "Home""#
        );
    }

    #[test]
    fn typing_collapses_and_never_keeps_text() {
        let trail: Trail = Arc::default();
        push(&trail, "typed 3 characters".into());
        push(&trail, "typed 1 characters".into());
        push(&trail, "pressed Enter".into());
        assert_eq!(entries(&trail), vec!["typed 4 characters", "pressed Enter"]);
    }

    #[test]
    fn visits_name_the_page_and_repeats_collapse() {
        let trail = Trail::default();
        let sign_in = "https://login.example.com/sso/wsfed/passive?wctx=AAAABBBB&username=x";
        for _ in 0..3 {
            push(&trail, format!("visited {} \"Sign in\"", page_of(sign_in)));
        }
        assert_eq!(
            entries(&trail),
            vec!["visited https://login.example.com/sso/wsfed/passive \"Sign in\""]
        );
    }
}
