//! The Page Model (docs/design.md §5.3): a compact, token-budgeted view of a page that
//! tells an agent what is there, what it can interact with, and where things are.
//!
//! It is built entirely from CDP-side data (the accessibility tree fused with a DOM
//! snapshot), so observing a page runs no JavaScript in it and is invisible to it.

mod ax;
mod compact;
mod control;
mod effects;
mod field_state;
mod formless;
mod forms;
mod frame;
mod fuse;
mod geometry;
mod model;
mod page;
mod page_words;
mod pdf;
mod read;
mod regions;
mod render;
mod roles;
mod snapshot;
mod text;
mod widgets;

use std::time::Duration;

use aibrowse_browser::{FrameHandle, Result, Tab};
use serde_json::json;

pub use ax::AxNode;
pub use fuse::fuse;
pub use model::{Detail, Field, FormPart, Mark, Node, NodeKind, PageModel, PageType, Position};
pub use read::{
    DEFAULT_PAGE_CHARS, FRAGMENT_MARK, main_text, paginate_text, read, settled_main_text,
    without_mark,
};
pub use snapshot::DomSnapshot;

/// Raw CDP inputs for one observation; persisted as fixtures for offline development.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RawObservation {
    pub url: String,
    pub viewport: aibrowse_browser::Viewport,
    /// One DOM snapshot per target: the page (target 0) and each out-of-process iframe.
    pub snapshots: Vec<TargetSnapshot>,
    /// Every observed frame, main frame first.
    pub frames: Vec<FrameCapture>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TargetSnapshot {
    pub target: u32,
    pub snapshot: DomSnapshot,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameCapture {
    pub target: u32,
    pub frame_id: String,
    /// Parent frame id and the backend id of the `<iframe>` element in it.
    pub owner: Option<(String, i64)>,
    /// Content box in main-viewport coordinates: [x, y, width, height].
    pub rect: Option<[f64; 4]>,
    pub ax: Vec<AxNode>,
}

/// Computed styles requested from the DOM snapshot, in this order (see `snapshot.rs`).
pub(crate) const STYLES: [&str; 4] = ["display", "visibility", "opacity", "cursor"];

/// Large SPAs produce multi-megabyte trees; give the browser time to serialize them.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);

/// Frames smaller than this (in CSS px²) are tracking pixels and ad beacons, not content.
const MIN_FRAME_AREA: f64 = 400.0;

/// Lays out what `content-visibility: auto` skips, for the length of a capture. Skipped
/// content far from the viewport has no boxes and no accessibility nodes at all
/// (Vercel's whole footer, "Help" included, was missing until scrolled to), though it is
/// on the page for anyone who scrolls. An adopted stylesheet rather than a `<style>`
/// element: it fires no mutation the page could observe. Removed again right after, so
/// the page renders as it chose; sections keep the size they were laid out at when
/// they use `contain-intrinsic-size: auto`, as most do.
const UNSKIP: &str = r#"(() => {
  if (!document.adoptedStyleSheets) return false;
  const sheet = globalThis.__aibrowseUnskip || new CSSStyleSheet();
  if (!globalThis.__aibrowseUnskip) {
    sheet.replaceSync('* { content-visibility: visible !important; }');
    globalThis.__aibrowseUnskip = sheet;
  }
  if (!document.adoptedStyleSheets.includes(sheet))
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  return true;
})()"#;
const RESKIP: &str = r#"(() => {
  const sheet = globalThis.__aibrowseUnskip;
  if (sheet && document.adoptedStyleSheets.includes(sheet))
    document.adoptedStyleSheets = document.adoptedStyleSheets.filter((s) => s !== sheet);
})()"#;

pub async fn capture(tab: &mut Tab) -> Result<RawObservation> {
    // Tools a page registered through a `navigator.modelContext` polyfill are bridged
    // into Chrome's registry, so the outline lists them like any other page tool.
    tab.sync_polyfill_tools().await;
    // A page that can't take the sheet is captured as it renders.
    let unskipped = tab
        .evaluate_isolated_raw(UNSKIP)
        .await
        .is_ok_and(|v| v == true);
    let raw = capture_rendered(tab).await;
    if unskipped {
        let _ = tab.evaluate_isolated_raw(RESKIP).await;
    }
    raw
}

async fn capture_rendered(tab: &Tab) -> Result<RawObservation> {
    let viewport = tab.viewport().await?;
    let frames: Vec<FrameHandle> = tab
        .frames()
        .await?
        .into_iter()
        .filter(|f| f.owner.is_none() || f.rect.is_some_and(|[_, _, w, h]| w * h >= MIN_FRAME_AREA))
        .collect();

    let mut snapshots = Vec::new();
    for frame in &frames {
        if snapshots
            .iter()
            .any(|s: &TargetSnapshot| s.target == frame.target)
        {
            continue;
        }
        let value = frame
            .session
            .call_with_timeout(
                "DOMSnapshot.captureSnapshot",
                json!({ "computedStyles": STYLES, "includeDOMRects": false }),
                CAPTURE_TIMEOUT,
            )
            .await;
        let Ok(value) = value else { continue }; // iframe went away meanwhile
        let snapshot = serde_json::from_value(value).map_err(|e| shape_error("DOM snapshot", e))?;
        snapshots.push(TargetSnapshot {
            target: frame.target,
            snapshot,
        });
    }

    let mut captures = Vec::new();
    for frame in frames {
        let ax = frame
            .session
            .call_with_timeout(
                "Accessibility.getFullAXTree",
                json!({ "frameId": frame.frame_id }),
                CAPTURE_TIMEOUT,
            )
            .await;
        let Ok(mut ax) = ax else { continue };
        let ax =
            serde_json::from_value(ax["nodes"].take()).map_err(|e| shape_error("AX tree", e))?;
        captures.push(FrameCapture {
            target: frame.target,
            frame_id: frame.frame_id,
            owner: frame.owner,
            rect: frame.rect,
            ax,
        });
    }
    Ok(RawObservation {
        url: tab.url(),
        viewport,
        snapshots,
        frames: captures,
    })
}

fn shape_error(what: &str, err: serde_json::Error) -> aibrowse_browser::BrowserError {
    aibrowse_browser::BrowserError::Element(format!("unexpected {what} shape: {err}"))
}

/// Captures the page and builds its Page Model; `region` narrows it to one landmark
/// (see `fuse`).
pub async fn observe(tab: &mut Tab, detail: Detail, region: Option<&str>) -> Result<PageModel> {
    let raw = capture(tab).await?;
    Ok(fuse(&raw, detail, region))
}
