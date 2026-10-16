//! The raw event logger: a local page that records every input event while aibrowse
//! performs a scripted interaction through the same humanized paths agents use (`click`,
//! `fill`, `type`, `scroll`), then analyzes what the page saw. Needs no internet.

use aibrowse_browser::{Browser, ElementRef, Tab};
use aibrowse_perception::{Detail, PageModel};
use anyhow::{Context, bail};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::analysis::{Analysis, Event, WindowContext, analyze};
use super::persona_match;

const PAGE: &str = include_str!("event_log.html");

/// Asks the page (from the isolated world, which can't see page globals) to write its
/// log into the DOM, where the isolated world can read it. The custom event isn't one of
/// the logged types, so the request leaves no trace in the log.
const DUMP: &str = r#"(() => {
  document.dispatchEvent(new CustomEvent('aibrowse-dump'));
  const node = document.getElementById('aibrowse-log');
  return node ? node.textContent : null;
})()"#;

const DUMP_LISTENER: &str = r#"<script>
  document.addEventListener('aibrowse-dump', () => {
    let node = document.getElementById('aibrowse-log');
    if (!node) {
      node = document.createElement('script');
      node.type = 'application/json';
      node.id = 'aibrowse-log';
      document.body.appendChild(node);
    }
    node.textContent = JSON.stringify({ events: window.__events, window: window.__context() });
  });
</script>"#;

/// What the event logger saw.
#[derive(Debug, Serialize)]
pub struct EventLogReport {
    /// The profile persona's pointing device, which the wheel deltas should match.
    pub persona_device: String,
    /// The persona that produced the input, and what the page could measure of it with
    /// the extractor used on the real user's handoff input.
    pub persona: aibrowse_input::Persona,
    pub kinematics: aibrowse_input::kinematics::Kinematics,
    /// The interaction performed, in order.
    pub steps: Vec<String>,
    pub window: WindowContext,
    pub analysis: Analysis,
    /// Every recorded event, for comparing against recorded human traces later.
    pub events: Vec<Event>,
}

/// Serves the logger page on an ephemeral localhost port (a secure context, unlike a
/// data: URL), returning the port and the server task.
async fn serve() -> anyhow::Result<(u16, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let body = PAGE.replace("</body>", &format!("{DUMP_LISTENER}\n</body>"));
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let (status, body) = if buf.starts_with(b"GET / ") {
                    ("200 OK", body)
                } else {
                    ("404 Not Found", String::new())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    Ok((port, server))
}

/// Runs the scripted interaction in a fresh tab of `browser` and analyzes the result.
pub async fn run(browser: &Browser) -> anyhow::Result<EventLogReport> {
    let (port, server) = serve().await?;
    let mut tab = browser.open_tab().await?;
    let result = interact(&mut tab, port).await;
    let _ = browser.close_tab(tab.target_id()).await;
    server.abort();
    let (steps, events, window) = result?;
    let analysis = analyze(&events, &window);
    let persona = browser.profile().persona.clone();
    Ok(EventLogReport {
        persona_device: format!("{:?}", persona.device).to_lowercase(),
        kinematics: persona_match::measure(&events),
        persona,
        steps,
        window,
        analysis,
        events,
    })
}

type Recorded = (Vec<String>, Vec<Event>, WindowContext);

async fn interact(tab: &mut Tab, port: u16) -> anyhow::Result<Recorded> {
    tab.navigate(&format!("http://localhost:{port}/")).await?;
    let model = aibrowse_perception::observe(tab, Detail::All, None).await?;
    let mut steps = Vec::new();

    // Moves in several directions and distances, a field fill (click, select-all,
    // typing), free typing with capitals and punctuation, wheel scrolling both ways,
    // and a click on an element that must first be scrolled into view.
    tab.click(find(&model, "button", "Alpha")?).await?;
    steps.push("click Alpha".to_owned());
    tab.click(find(&model, "button", "Bravo")?).await?;
    steps.push("click Bravo".to_owned());
    tab.fill(find(&model, "textbox", "Name")?, "Ada Lovelace")
        .await?;
    steps.push("fill Name".to_owned());
    tab.click(find(&model, "textbox", "Message")?).await?;
    tab.type_text("Hello there, this is a typing test.").await?;
    steps.push("click Message, type a sentence".to_owned());
    tab.click(find(&model, "button", "Charlie")?).await?;
    steps.push("click Charlie".to_owned());
    tab.scroll(0.0, 700.0).await?;
    tab.scroll(0.0, -350.0).await?;
    steps.push("scroll down 700, up 350".to_owned());
    tab.click(find(&model, "button", "Far away")?).await?;
    steps.push("click Far away (scrolled into view)".to_owned());

    let dump = tab.evaluate_isolated_raw(DUMP).await?;
    let text = dump
        .as_str()
        .context("the logger page did not produce its log")?;
    #[derive(serde::Deserialize)]
    struct Log {
        events: Vec<Event>,
        window: WindowContext,
    }
    let log: Log = serde_json::from_str(text).context("parsing the event log")?;
    Ok((steps, log.events, log.window))
}

/// The element with `role` and exactly `name` in the Page Model.
fn find(model: &PageModel, role: &str, name: &str) -> anyhow::Result<ElementRef> {
    let Some(node) = model
        .nodes
        .iter()
        .find(|n| n.role == role && n.name == name)
    else {
        bail!("the logger page has no {role} {name:?}");
    };
    node.reference
        .with_context(|| format!("{role} {name:?} has no ref"))
}

#[cfg(test)]
mod tests {
    use aibrowse_browser::LaunchOptions;

    /// Runs the logger against a real Chrome and requires no tells: everything here is
    /// produced by our own input paths, so a tell is a regression. Ignored by default;
    /// run with `make test-live`.
    #[tokio::test]
    #[ignore = "needs Chrome; run with `make test-live`"]
    async fn humanized_input_has_no_tells() {
        let headless = std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1");
        let browser = aibrowse_browser::Browser::launch(LaunchOptions {
            profile: "test-detectors".into(),
            headless,
            mute: true,
            ..LaunchOptions::default()
        })
        .await
        .expect("launch");
        let report = super::run(&browser).await;
        browser.close().await.expect("close");
        let report = report.expect("event logger run");
        let analysis = &report.analysis;
        assert!(analysis.pointer.clicks >= 5, "{:?}", analysis.pointer);
        assert!(
            analysis.keyboard.keystrokes >= 40,
            "{:?}",
            analysis.keyboard
        );
        assert!(analysis.wheel.ticks > 0, "{:?}", analysis.wheel);
        let tells: Vec<_> = analysis
            .findings
            .iter()
            .filter(|f| f.severity == super::super::analysis::Severity::Tell)
            .collect();
        assert!(tells.is_empty(), "tells: {tells:#?}");
    }
}
