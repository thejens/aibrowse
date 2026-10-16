//! Fan-out (docs/design.md §5.5): open several links in background tabs and pursue the
//! goal in each, the way a person who can't tell which link is right opens them all
//! and looks.
//!
//! Branches **think in parallel and act one at a time**. Pages loading and Jev deciding
//! are what take the time, and they overlap freely. Input doesn't: before a branch
//! clicks or scrolls, its tab comes to the front, so no page ever gets input while
//! hidden and there is only ever one cursor at work.

use std::time::Duration;

use aibrowse_browser::{Browser, Tab, World};
use aibrowse_input::Pace;
use futures_util::future::join_all;

use crate::jev::Jev;
use crate::pursue::{Decision, Limits, Outcome, Step, Workspace};
use crate::pursuit::{Link, Next, Opening, Pursuit, failed_step};

/// How long a background tab may take to load before its branch starts looking anyway:
/// a page that hasn't settled by then is judged on what it shows.
const LOAD_WAIT: Duration = Duration::from_secs(5);
/// How long Chrome may take to report a tab a modifier click opened.
const OPEN_WAIT: Duration = Duration::from_secs(3);

/// The addresses a page's pagination leads to: the links of a pagination bar the page
/// names one (class, id or label `pagination`, `pager`), `rel=next`/`prev` links, and
/// short links to this very page that change only a query parameter some link numbered
/// by its text pages through (`2` → `?sida=2`, then `Nästa ›` → `?sida=2` too). A
/// category link (`Meat` → `?cat=3`) isn't one. Read in the isolated world and never
/// written: the page is left exactly as the site made it.
const PAGING_LINKS: &str = include_str!("paging_links.js");

/// How far past its time budget a branch's step may run before it is cut off.
const STEP_GRACE: Duration = Duration::from_secs(15);

/// Which branches a fan-out waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collect {
    /// The first branch to reach the goal ends the fan-out (navigating somewhere).
    First,
    /// Every branch runs to its end (finding all pages that qualify). Each branch has
    /// a step or two to show its result is a page that qualifies, so it opens items
    /// rather than paging through a list: its pages' pagination is hidden from it (a
    /// second page of recall notices is older ones, not a better result).
    All,
}

/// How one branch ended.
#[derive(Debug, Clone)]
pub struct Branch {
    /// The link it started from.
    pub link: Link,
    pub outcome: Outcome,
    pub steps: Vec<Step>,
    pub trail: Vec<Decision>,
    /// Element identities clicked in the branch.
    pub path: Vec<String>,
    /// Where it ended.
    pub url: String,
    pub title: String,
    /// How long it ran, until it ended rather than until the whole fan-out did.
    pub elapsed: Duration,
}

/// A branch that reached the goal, with its tab, still open.
pub struct Found {
    pub branch: Branch,
    pub tab: Tab,
}

pub struct FanOut {
    pub reached: Vec<Found>,
    /// Branches that stopped where the agent or the user must act (text to type, a
    /// sign-in): not the goal, but where control goes back, so their tabs stay open
    /// for the caller to adopt or close.
    pub waiting: Vec<Found>,
    /// Branches that gave up (their tabs are closed), and why, in `outcome`.
    pub gave_up: Vec<Branch>,
    /// With `Collect::All`: branches that ended short of the goal without handing back
    /// (out of steps or time, unsure, stuck), tabs still open. What they show may still
    /// be what was looked for, and the caller can judge it rather than throw it away.
    pub unfinished: Vec<Found>,
    pub decisions: usize,
    pub notes: Vec<String>,
}

/// One branch's tabs: the one its link opened, plus any its own clicks opened.
struct BranchTabs<'b> {
    browser: &'b Browser,
    tabs: Vec<Tab>,
    pace: Pace,
}

impl Workspace for BranchTabs<'_> {
    fn tab(&mut self) -> &mut Tab {
        self.tabs.last_mut().expect("a branch has a tab")
    }

    async fn follow_new_tabs(&mut self) -> Vec<String> {
        let opener = self.tab().target_id().to_owned();
        let popups = self.browser.take_popups(&opener).await.unwrap_or_default();
        let notes = vec!["the page opened a new tab".to_owned(); popups.len()];
        for mut popup in popups {
            popup.set_pace(self.pace);
            self.tabs.push(popup);
        }
        notes
    }

    async fn back_to(&mut self, opener: &str) -> bool {
        self.back(opener).await
    }
}

impl BranchTabs<'_> {
    /// Closes the tab in front if it isn't the branch's first, and returns to `opener`.
    async fn back(&mut self, opener: &str) -> bool {
        if self.tabs.len() < 2 || self.tabs[self.tabs.len() - 2].target_id() != opener {
            return false;
        }
        let front = self.tabs.pop().expect("checked above");
        let _ = self.browser.close_tab(front.target_id()).await;
        let _ = self.browser.activate_tab(opener).await;
        true
    }

    async fn close(self) {
        for tab in self.tabs {
            let _ = self.browser.close_tab(tab.target_id()).await;
        }
    }

    /// Keeps the tab in front and closes the rest.
    async fn keep_front(mut self) -> Tab {
        let front = self.tabs.pop().expect("a branch has a tab");
        self.close().await;
        front
    }
}

struct Running<'b, 'g> {
    link: Link,
    tabs: BranchTabs<'b>,
    pursuit: Pursuit<'g>,
    loaded: bool,
    end: Option<Outcome>,
    /// How long the branch had run when it ended: the fan-out goes on for the others.
    ended_after: Option<Duration>,
}

impl Running<'_, '_> {
    fn finish(&mut self, outcome: Outcome) {
        self.end = Some(outcome);
        self.ended_after = Some(self.pursuit.elapsed());
    }
}

/// Opens each of `links` (on the page in the workspace's tab in front) in a background
/// tab and pursues `goal` in each. The page in front stays where it was, and is in
/// front again afterwards.
pub async fn fan_out<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    goal: &str,
    texts: &[String],
    links: &[Link],
    collect: Collect,
    limits: &Limits,
) -> FanOut {
    let mut out = FanOut {
        reached: Vec::new(),
        waiting: Vec::new(),
        gave_up: Vec::new(),
        unfinished: Vec::new(),
        decisions: 0,
        notes: Vec::new(),
    };
    let pace = workspace.tab().pace();
    let home = workspace.tab().target_id().to_owned();
    // Go down the list Cmd-clicking, as a person would, each new tab claimed as it
    // appears.
    let mut claimed: Vec<(Link, Tab)> = Vec::new();
    for link in links {
        let Some(mark) = workspace.browser().map(Browser::opened_mark) else {
            out.gave_up
                .push(unopened(link, "no browser to open tabs in"));
            continue;
        };
        let reference = match &link.open {
            Opening::Click(reference) => *reference,
            Opening::Address(address) => {
                let Some(browser) = workspace.browser() else {
                    continue;
                };
                match browser.new_background_tab(address).await {
                    Ok(mut tab) => {
                        tab.set_pace(pace);
                        claimed.push((link.clone(), tab));
                    }
                    Err(err) => out
                        .gave_up
                        .push(unopened(link, &format!("could not open: {err}"))),
                }
                continue;
            }
        };
        let opening = std::time::Instant::now();
        let url = match workspace.tab().open_in_background(reference).await {
            Ok(opened) => opened.url,
            Err(err) => {
                out.gave_up
                    .push(unopened(link, &format!("could not open: {err}")));
                continue;
            }
        };
        let Some(browser) = workspace.browser() else {
            continue;
        };
        let claimed_tab = browser.take_opened(mark, &url, OPEN_WAIT).await;
        tracing::debug!(link = %link.label, elapsed = ?opening.elapsed(), "opened in a background tab");
        match claimed_tab {
            Ok(Some(mut tab)) => {
                tab.set_pace(pace);
                claimed.push((link.clone(), tab));
            }
            Ok(None) => out.gave_up.push(unopened(link, "no tab opened")),
            Err(err) => out
                .gave_up
                .push(unopened(link, &format!("could not attach: {err}"))),
        }
    }
    let Some(browser) = workspace.browser() else {
        return out;
    };
    let mut running: Vec<Running> = claimed
        .into_iter()
        .map(|(link, tab)| Running {
            tabs: BranchTabs {
                browser,
                tabs: vec![tab],
                pace,
            },
            pursuit: {
                let mut pursuit = Pursuit::new(goal, texts, limits, Vec::new(), true);
                if link.named_for_goal {
                    pursuit.verify_first();
                }
                pursuit
            },
            link,
            loaded: false,
            end: None,
            ended_after: None,
        })
        .collect();
    if !running.is_empty() {
        out.notes.push(format!(
            "explored {} links side by side in background tabs",
            running.len()
        ));
    }

    while running.iter().any(|r| r.end.is_none()) {
        // Think: every live branch at once.
        let thoughts = join_all(running.iter_mut().filter(|r| r.end.is_none()).map(
            |r| async move {
                if !r.loaded {
                    let loading = std::time::Instant::now();
                    r.tabs.tab().wait_settled(LOAD_WAIT).await;
                    r.loaded = true;
                    tracing::debug!(link = %r.link.label, elapsed = ?loading.elapsed(), "branch loaded");
                }
                // Again on every page: each step may have loaded another.
                // A search branch has a step or two to show its page qualifies: paging
                // through a list spends them on older items (återkallat.se's `?sida=2`).
                if collect == Collect::All {
                    let paging = match r.tabs.tab().evaluate(PAGING_LINKS, World::Isolated).await {
                        Ok(found) => serde_json::from_value(found).unwrap_or_default(),
                        Err(err) => {
                            tracing::debug!(link = %r.link.label, error = %err, "could not find pagination");
                            Vec::new()
                        }
                    };
                    r.pursuit.avoid_links(paging);
                }
                // Bounded within the step too (a page read that hangs): out of budget.
                let left = limits.max_time.saturating_sub(r.pursuit.elapsed()) + STEP_GRACE;
                let next = match tokio::time::timeout(left, r.pursuit.think(r.tabs.tab(), jev)).await {
                    Ok(next) => next,
                    Err(_) => Ok(Next::Stop(Outcome::OutOfBudget)),
                };
                (r, next)
            },
        ))
        .await;
        let mut acts = Vec::new();
        let mut any_reached = false;
        for (r, next) in thoughts {
            match next {
                Ok(Next::Stop(outcome)) => {
                    any_reached |= matches!(outcome, Outcome::Reached);
                    r.finish(outcome);
                }
                Ok(next) => acts.push((r, next)),
                Err(err) => {
                    let (url, _) = r.pursuit.page();
                    let url = url.to_owned();
                    r.pursuit
                        .steps
                        .push(failed_step("decide", &url, &err.to_string()));
                    r.finish(Outcome::Stuck {
                        options: Vec::new(),
                    });
                }
            }
        }
        if collect == Collect::First && any_reached {
            break;
        }
        // Act: one tab at a time (the browser's one hand brings each to the front).
        for (r, next) in acts {
            if let Some(outcome) = r.pursuit.act(&mut r.tabs, next).await {
                r.finish(outcome);
            }
        }
    }
    let _ = browser.activate_tab(&home).await;

    for r in running {
        out.decisions += r.pursuit.decisions;
        let (url, title) = r.pursuit.page();
        let (url, title) = (url.to_owned(), title.to_owned());
        let outcome = r.end.unwrap_or(Outcome::OutOfBudget);
        let branch = Branch {
            link: r.link,
            elapsed: r.ended_after.unwrap_or_else(|| r.pursuit.elapsed()),
            steps: r.pursuit.steps,
            trail: r.pursuit.trail,
            path: r.pursuit.path,
            outcome,
            url,
            title,
        };
        tracing::debug!(
            link = %branch.link.label,
            outcome = %branch.outcome.why(),
            steps = ?branch.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(),
            url = %branch.url,
            "branch ended"
        );
        let hands_back = matches!(
            branch.outcome,
            Outcome::NeedsAgent { .. } | Outcome::NeedsUser { .. }
        );
        if matches!(branch.outcome, Outcome::Reached) {
            let tab = r.tabs.keep_front().await;
            out.reached.push(Found { branch, tab });
        } else if hands_back {
            let tab = r.tabs.keep_front().await;
            out.waiting.push(Found { branch, tab });
        } else if collect == Collect::All {
            let tab = r.tabs.keep_front().await;
            out.unfinished.push(Found { branch, tab });
        } else {
            r.tabs.close().await;
            out.gave_up.push(branch);
        }
    }
    out
}

/// A branch that never started.
fn unopened(link: &Link, why: &str) -> Branch {
    Branch {
        link: link.clone(),
        outcome: Outcome::Stuck {
            options: Vec::new(),
        },
        steps: vec![failed_step("open in a new tab", "", why)],
        trail: Vec::new(),
        path: Vec::new(),
        url: String::new(),
        title: String::new(),
        elapsed: Duration::ZERO,
    }
}
