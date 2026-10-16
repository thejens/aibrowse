//! Putting a session away and bringing it back: the journal record of a live session,
//! and a session started where a journal left off.

use std::path::Path;
use std::time::Duration;

use aibrowse_browser::{Tab, World};

use super::Live;
use super::journal::{Journal, TabRecord};
use crate::announce;

/// A tab whose layout can't be read this quickly (a stuck renderer) is recorded without
/// its scroll offset rather than hold up the session.
const LAYOUT_TIMEOUT: Duration = Duration::from_secs(1);

/// What resuming brought back.
#[derive(Debug, Default)]
pub struct Restored {
    pub tabs: usize,
    /// Tabs whose page could not be loaded again, as "url: error".
    pub failed: Vec<String>,
}

impl Live {
    /// Where the session is now. Cheap enough to take after every acting call: per tab,
    /// one target lookup and one layout query, and no page script (a page frozen by a
    /// dialog still answers both).
    pub async fn snapshot(&self) -> Journal {
        let mut tabs = Vec::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            let info = self.browser.tab_info(tab.target_id()).await.ok();
            let viewport = tokio::time::timeout(LAYOUT_TIMEOUT, tab.viewport())
                .await
                .ok()
                .and_then(Result::ok);
            let (url, title) = match info {
                Some(info) => (info.url, info.title),
                None => (tab.url(), String::new()),
            };
            tabs.push(TabRecord {
                url,
                title,
                scroll_x: viewport.as_ref().map_or(0.0, |v| v.scroll_x),
                scroll_y: viewport.as_ref().map_or(0.0, |v| v.scroll_y),
            });
        }
        Journal::new(
            self.started_at,
            self.headless,
            self.pace,
            self.attended,
            self.current,
            tabs,
        )
        .with_doing(&self.doing)
    }

    pub fn profile_root(&self) -> &Path {
        &self.browser.profile().root
    }

    /// Starts a session where `journal` left off: the same tabs in the same order, each
    /// scrolled back to where it was, the same one active, with the journal's pace and
    /// presence. Pages load afresh (in parallel); nothing typed into them comes back.
    pub async fn resume(
        profile: &str,
        headless: bool,
        journal: &Journal,
    ) -> aibrowse_browser::Result<(Self, Restored)> {
        let mut live =
            Self::start(profile, headless, journal.pace, journal.attended, false).await?;
        live.started_at = journal.started_at;
        live.doing = journal.doing();
        for _ in 1..journal.tabs.len() {
            let mut tab = live.browser.new_tab().await?;
            tab.set_pace(live.pace);
            announce::watch(&tab, live.doors.clone());
            live.tabs.push(tab);
        }
        let loads: Vec<_> = std::mem::take(&mut live.tabs)
            .into_iter()
            .zip(journal.tabs.iter().cloned())
            .map(|(mut tab, record)| {
                tokio::spawn(async move {
                    let outcome = reopen(&mut tab, &record).await;
                    (tab, record.url, outcome)
                })
            })
            .collect();
        let mut restored = Restored::default();
        for load in loads {
            // A load task only fails to join if it panicked; its tab is lost with it.
            let (tab, url, outcome) = load.await.map_err(std::io::Error::other)?;
            match outcome {
                Ok(()) => restored.tabs += 1,
                Err(err) => restored.failed.push(format!("{url}: {err}")),
            }
            live.tabs.push(tab);
        }
        live.current = journal.active.min(live.tabs.len() - 1);
        let active = live.tabs[live.current].target_id().to_owned();
        live.browser.activate_tab(&active).await?;
        Ok((live, restored))
    }
}

/// Loads a journaled tab's page again and scrolls back to where it was. A tab that was
/// on a browser page (blank, settings) stays blank.
async fn reopen(tab: &mut Tab, record: &TabRecord) -> Result<(), String> {
    if !record.restorable() {
        return Ok(());
    }
    tab.navigate(&record.url).await.map_err(|e| e.to_string())?;
    if record.scroll_x > 0.0 || record.scroll_y > 0.0 {
        // Programmatic, as Chrome's own session restore is: no wheel input to humanize.
        let scroll = format!(
            "window.scrollTo({}, {})",
            record.scroll_x.round(),
            record.scroll_y.round()
        );
        tab.evaluate(&scroll, World::Isolated)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
