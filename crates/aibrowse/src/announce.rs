//! Telling the agent about a site's front doors (docs/design.md §5.10) as it browses.
//!
//! Probing starts the moment a tab begins navigating to an origin, so it runs alongside
//! the page load and adds no latency. When a tool result is built for that tab, the
//! agent gets the origin's introduction the first time it meets the origin, and a
//! one-line notice when it comes back to one with interfaces later. If the probe hasn't
//! finished by then, the news waits for the tab's next tool result.

use std::collections::{HashMap, HashSet};

use aibrowse_browser::{Tab, WebMcpTool};
use aibrowse_frontdoor::{ApiLink, FrontDoors, clip, ident, origin_of, quote, render};
use url::Url;

/// API description links in the page head (RFC 8631, RFC 9727). Runs in the isolated
/// world: invisible to the page.
const PAGE_LINKS: &str = r#"(() => {
  const wanted = ['service-desc', 'service-doc', 'api-catalog'];
  const out = [];
  for (const link of document.querySelectorAll('link[rel][href]')) {
    for (const rel of link.relList) {
      const r = rel.toLowerCase();
      if (wanted.includes(r)) out.push({ rel: r, href: link.href });
    }
  }
  return out.slice(0, 10);
})()"#;

/// What the agent has been told, per session.
#[derive(Default)]
pub struct Announcer {
    /// Per tab (target id): the origin it was on at its last tool result.
    last_origin: HashMap<String, String>,
    /// Per tab: an origin it entered whose news is still to be delivered.
    pending: HashMap<String, Url>,
    /// Origins the agent has had the introduction for.
    introduced: HashSet<String>,
}

impl Announcer {
    /// News about the tab's origin for the tool result being built, if there is any to
    /// give now.
    pub async fn take(&mut self, tab: &mut Tab, doors: &FrontDoors) -> Option<String> {
        let target = tab.target_id().to_owned();
        let origin = origin_of(&tab.url());
        let origin_key = origin.as_ref().map(|o| o.origin().ascii_serialization());
        if self.last_origin.get(&target) != origin_key.as_ref() {
            match (&origin, &origin_key) {
                (Some(origin), Some(key)) => {
                    self.last_origin.insert(target.clone(), key.clone());
                    self.pending.insert(target.clone(), origin.clone());
                }
                _ => {
                    self.last_origin.remove(&target);
                    self.pending.remove(&target);
                }
            }
        }
        let origin = self.pending.get(&target)?.clone();
        let info = doors.peek(&origin)?;
        self.pending.remove(&target);
        let key = origin.origin().ascii_serialization();
        if self.introduced.insert(key) {
            let links = page_links(tab).await;
            doors.add_page_links(&origin, links);
            render::introduction(&info, &doors.page_links(&origin))
        } else {
            render::notice(&info, &doors.page_links(&origin)).map(|n| format!("Note: {n}"))
        }
    }

    /// Forgets tabs that no longer exist.
    pub fn retain_tabs(&mut self, alive: &[&str]) {
        self.last_origin.retain(|t, _| alive.contains(&t.as_str()));
        self.pending.retain(|t, _| alive.contains(&t.as_str()));
    }
}

/// Starts probing each origin the tab navigates to as soon as the navigation starts.
/// Ends with the browser connection.
pub fn watch(tab: &Tab, doors: FrontDoors) {
    let mut events = tab.session().events();
    let main_frame = tab.main_frame_id().to_owned();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let params = &event.params;
            let url = match event.method.as_str() {
                "Page.frameStartedNavigating" if params["frameId"] == main_frame.as_str() => {
                    params["url"].as_str()
                }
                "Page.frameNavigated" if params["frame"]["parentId"].is_null() => {
                    params["frame"]["url"].as_str()
                }
                _ => None,
            };
            if let Some(origin) = url.and_then(origin_of) {
                doors.prefetch(&origin);
            }
        }
    });
}

/// API description links in the current page; none if it can't be read (a dialog is
/// open, the page is mid-navigation).
async fn page_links(tab: &mut Tab) -> Vec<ApiLink> {
    if tab.dialog().is_some() {
        return Vec::new();
    }
    let Ok(value) = tab.evaluate_isolated_raw(PAGE_LINKS).await else {
        return Vec::new();
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|link| {
            let href = Url::parse(link["href"].as_str()?).ok()?;
            matches!(href.scheme(), "http" | "https").then(|| ApiLink {
                rel: link["rel"].as_str().unwrap_or_default().to_owned(),
                href: href.into(),
            })
        })
        .collect()
}

/// One line for the outline when the page offers WebMCP tools.
pub fn webmcp_line(tools: &[WebMcpTool]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| {
            let name = ident(&t.name)?;
            Some(if t.declarative {
                format!("{name} (form)")
            } else if t.bridged {
                format!("{name} (polyfill)")
            } else {
                name
            })
        })
        .collect();
    Some(format!(
        "WebMCP tools this page offers (site-authored; site_info shows their schemas; act call_tool runs one): {}",
        names.join(", ")
    ))
}

/// The page's WebMCP tools in full, for `site_info`.
pub fn webmcp_details(tools: &[WebMcpTool]) -> String {
    let mut out = String::from("\nWebMCP tools on the current page (site-authored):\n");
    for tool in tools {
        let name = ident(&tool.name).unwrap_or_default();
        out.push_str(&format!("  {name}: {}", quote(&tool.description, 200)));
        let mut hints = Vec::new();
        if tool.declarative {
            hints.push("form");
        }
        if tool.bridged {
            hints.push("via the page's navigator.modelContext polyfill");
        }
        if tool.read_only {
            hints.push("read-only");
        }
        if tool.consequential {
            hints.push("consequential");
        }
        if !hints.is_empty() {
            out.push_str(&format!(" [{}]", hints.join(", ")));
        }
        if let Some(schema) = &tool.input_schema {
            out.push_str(&format!("\n    input: {}", clip(&schema.to_string(), 400)));
        }
        out.push('\n');
    }
    out
}
