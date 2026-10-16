//! The navigator eval's browser: a home tab plus the tabs a run's clicks open, as a
//! [`Workspace`] the navigator works in.

use std::path::PathBuf;

use aibrowse_browser::{Browser, Tab};
use aibrowse_frontdoor::FrontDoors;
use aibrowse_input::Pace;
use aibrowse_navigator::Workspace;

/// The eval's browser: one home tab, plus whatever tabs a run's clicks open, which the
/// navigator follows and which are closed again after the run.
pub(super) struct EvalTabs<'a> {
    pub(super) browser: &'a Browser,
    pub(super) home: Tab,
    pub(super) opened: Vec<Tab>,
    /// Where learned paths live; the eval keeps its own, so a second run measures what
    /// learning saves.
    pub(super) kb: Option<PathBuf>,
    /// Sitemaps, read and cached as the server reads them.
    pub(super) doors: FrontDoors,
}

impl Workspace for EvalTabs<'_> {
    fn tab(&mut self) -> &mut Tab {
        self.opened.last_mut().unwrap_or(&mut self.home)
    }

    fn browser(&self) -> Option<&Browser> {
        Some(self.browser)
    }

    async fn adopt(&mut self, tab: Tab) -> Vec<String> {
        self.opened.push(tab);
        Vec::new()
    }

    async fn back_to(&mut self, opener: &str) -> bool {
        let Some(front) = self.opened.pop() else {
            return false;
        };
        let _ = self.browser.close_tab(front.target_id()).await;
        let _ = self.browser.activate_tab(opener).await;
        self.tab().target_id() == opener
    }

    async fn follow_new_tabs(&mut self) -> Vec<String> {
        let opener = self.tab().target_id().to_owned();
        let popups = self.browser.take_popups(&opener).await.unwrap_or_default();
        let notes = vec!["the page opened a new tab".to_owned(); popups.len()];
        for mut popup in popups {
            popup.set_pace(Pace::Fast);
            self.opened.push(popup);
        }
        notes
    }

    async fn site_pages(&mut self, url: &str, words: &[String]) -> Vec<String> {
        self.doors.site_pages(url, words).await.urls
    }

    async fn recall_path(&mut self, site: &str, goal: &str) -> Option<Vec<String>> {
        aibrowse_network::recall_path(self.kb.as_deref()?, site, goal)
    }

    async fn forget_path(&mut self, site: &str, goal: &str) {
        if let Some(kb) = &self.kb {
            aibrowse_network::forget_path(kb, site, goal);
        }
    }

    async fn learn_path(&mut self, site: &str, goal: &str, steps: &[String]) {
        if let Some(kb) = &self.kb {
            aibrowse_network::learn_path(kb, site, goal, steps);
        }
    }
}

impl EvalTabs<'_> {
    pub(super) async fn close_opened(&mut self) {
        for tab in self.opened.drain(..) {
            let _ = self.browser.close_tab(tab.target_id()).await;
        }
    }
}
