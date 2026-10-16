//! Tabs the page opens by itself (target=_blank links, window.open), tabs opened in the
//! background by a modifier click, and tabs that close.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibrowse_cdp::Connection;
use serde_json::json;

use crate::error::Result;
use crate::launch::Browser;
use crate::tab::Tab;

#[derive(Default)]
pub(crate) struct TargetWatch {
    /// (opener target id, new target id), in creation order.
    popups: Vec<(String, String)>,
    /// (sequence number, target id, first URL) of pages that appeared with no opener
    /// and that no one has claimed: a link opened in the background with Cmd/Ctrl-click
    /// is one (Chrome gives such tabs no opener), and so are tabs we create ourselves.
    unclaimed: Vec<(u64, String, String)>,
    /// The sequence number the next unclaimed page gets.
    next_unclaimed: u64,
    destroyed: HashSet<String>,
}

/// Unclaimed tabs remembered, at most; the oldest are forgotten first.
const MAX_UNCLAIMED: usize = 32;

pub(crate) type SharedTargets = Arc<Mutex<TargetWatch>>;

fn lock(targets: &SharedTargets) -> std::sync::MutexGuard<'_, TargetWatch> {
    targets
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Turns on target discovery and records page targets as they come and go.
pub(crate) async fn watch_targets(conn: &Connection) -> Result<SharedTargets> {
    let targets: SharedTargets = Arc::default();
    let mut events = conn.browser().events();
    conn.browser()
        .call("Target.setDiscoverTargets", json!({ "discover": true }))
        .await?;
    let shared = Arc::clone(&targets);
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let info = &event.params["targetInfo"];
            match event.method.as_str() {
                "Target.targetCreated" if info["type"] == "page" => {
                    let id = info["targetId"].as_str().unwrap_or_default().to_owned();
                    let mut watch = lock(&shared);
                    match info["openerId"].as_str() {
                        Some(opener) => watch.popups.push((opener.to_owned(), id)),
                        None => {
                            let url = info["url"].as_str().unwrap_or_default().to_owned();
                            let seq = watch.next_unclaimed;
                            watch.next_unclaimed += 1;
                            watch.unclaimed.push((seq, id, url));
                            let excess = watch.unclaimed.len().saturating_sub(MAX_UNCLAIMED);
                            watch.unclaimed.drain(..excess);
                        }
                    }
                }
                "Target.targetDestroyed" => {
                    if let Some(id) = event.params["targetId"].as_str() {
                        let mut watch = lock(&shared);
                        watch.unclaimed.retain(|(_, t, _)| t != id);
                        watch.destroyed.insert(id.to_owned());
                    }
                }
                _ => {}
            }
        }
    });
    Ok(targets)
}

/// A tab as listed to the agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TabInfo {
    pub target_id: String,
    pub title: String,
    pub url: String,
}

impl Browser {
    /// Tabs opened by `opener` since the last call, attached and ready to drive. When
    /// headless, their identity was already applied as they started (early_identity.rs).
    pub async fn take_popups(&self, opener: &str) -> Result<Vec<Tab>> {
        let opened: Vec<String> = {
            let mut watch = lock(&self.targets);
            let (mine, rest): (Vec<_>, Vec<_>) =
                watch.popups.drain(..).partition(|(o, _)| o == opener);
            watch.popups = rest;
            let destroyed = &watch.destroyed;
            mine.into_iter()
                .map(|(_, id)| id)
                .filter(|id| !destroyed.contains(id))
                .collect()
        };
        let mut tabs = Vec::with_capacity(opened.len());
        for target_id in opened {
            tabs.push(self.attach_target(&target_id).await?);
        }
        Ok(tabs)
    }

    /// Marks the moment before a background open, for `take_opened`.
    pub fn opened_mark(&self) -> u64 {
        lock(&self.targets).next_unclaimed
    }

    /// The tab a background open of `url` created (`Tab::open_in_background`), attached
    /// and ready to drive: a page without an opener that appeared since `mark`, the one
    /// at `url` if there are several. The address can't be required: the link may
    /// redirect before Chrome reports the tab (search engines route results through
    /// their own redirects). Chrome reports it a moment after the click, so this waits
    /// up to `wait` for it.
    pub async fn take_opened(&self, mark: u64, url: &str, wait: Duration) -> Result<Option<Tab>> {
        let deadline = Instant::now() + wait;
        loop {
            let claimed = {
                let mut watch = lock(&self.targets);
                let since = |seq: &u64| *seq >= mark;
                let found = watch
                    .unclaimed
                    .iter()
                    .position(|(seq, _, u)| since(seq) && same_document(u, url))
                    .or_else(|| watch.unclaimed.iter().position(|(seq, ..)| since(seq)));
                found.map(|i| watch.unclaimed.remove(i).1)
            };
            if let Some(target_id) = claimed {
                return self.attach_target(&target_id).await.map(Some);
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Whether the page closed itself (window.close()) or crashed away.
    pub fn is_tab_closed(&self, target_id: &str) -> bool {
        lock(&self.targets).destroyed.contains(target_id)
    }

    /// Opens another blank tab next to the existing ones (unlike [`Browser::open_tab`],
    /// which starts the session over with a single tab), attached and ready to drive.
    pub async fn new_tab(&self) -> Result<Tab> {
        let created = self
            .connection()
            .browser()
            .call("Target.createTarget", json!({ "url": "about:blank" }))
            .await?;
        let target_id = created["targetId"].as_str().unwrap_or_default().to_owned();
        self.attach_target(&target_id).await
    }

    /// Opens `url` in a new tab behind the current one, attached and ready to drive.
    pub async fn new_background_tab(&self, url: &str) -> Result<Tab> {
        let created = self
            .connection()
            .browser()
            .call(
                "Target.createTarget",
                json!({ "url": url, "background": true }),
            )
            .await?;
        let target_id = created["targetId"].as_str().unwrap_or_default().to_owned();
        self.attach_target(&target_id).await
    }

    pub async fn close_tab(&self, target_id: &str) -> Result<()> {
        self.connection()
            .browser()
            .call("Target.closeTarget", json!({ "targetId": target_id }))
            .await?;
        Ok(())
    }

    /// Makes a tab the visible one in its window, as clicking its tab strip entry would.
    pub async fn activate_tab(&self, target_id: &str) -> Result<()> {
        self.connection()
            .browser()
            .call("Target.activateTarget", json!({ "targetId": target_id }))
            .await?;
        Ok(())
    }

    pub async fn tab_info(&self, target_id: &str) -> Result<TabInfo> {
        let info = self
            .connection()
            .browser()
            .call("Target.getTargetInfo", json!({ "targetId": target_id }))
            .await?;
        let info = &info["targetInfo"];
        Ok(TabInfo {
            target_id: target_id.to_owned(),
            title: info["title"].as_str().unwrap_or_default().to_owned(),
            url: info["url"].as_str().unwrap_or_default().to_owned(),
        })
    }
}

/// Whether two URLs name the same document (fragments aside).
fn same_document(a: &str, b: &str) -> bool {
    let strip = |u: &str| u.split('#').next().unwrap_or(u).to_owned();
    strip(a) == strip(b)
}
