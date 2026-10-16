//! Durable sessions (docs/design.md §5.1): the browser is put away when
//! nobody needs it and comes back where it was.
//!
//! - After every call that can move the session (navigate, act, tabs...) its place is
//!   written to the profile's journal (`session::journal`), off the call's path.
//! - After `AIBROWSE_IDLE_MINUTES` (default 15, 0 = never) without tool calls, and when
//!   no human step or download is under way, Chrome is closed cleanly; the server stays.
//!   `session_close(hibernate=true)` does the same on request.
//! - The next call that needs the browser starts it again from the journal and tells
//!   the agent so in one line ahead of its result.
//! - `AIBROWSE_RESUME=1` resumes the default profile's journal across a process restart
//!   (the reload front sets it for the worker that replaces a retired one).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aibrowse_browser::DownloadState;

use crate::server::Aibrowse;
use crate::session::journal::{self, Journal, Writer};
use crate::session::{Live, Restored};

/// How `session_close(hibernate=true)` starts its answer; the reload front reads it to
/// know there is a session to resume in the next worker.
pub const HIBERNATED: &str = "Session hibernated";

/// Idle hibernation has no agent waiting on it, so Chrome gets far longer to exit by
/// itself than at a tool call (5s). Headless Chrome 154 usually takes ~10.5s and then
/// ends in its own teardown watchdog; storage is on disk well before that (measured:
/// cookies and localStorage written just before closing survive even a kill at 5s), so
/// this is margin, not a requirement.
const IDLE_GRACE: Duration = Duration::from_secs(30);

/// A requested hibernation (an agent, or the reload front, is waiting for it) closes
/// Chrome like `session_close` does.
const REQUESTED_GRACE: Duration = Duration::from_secs(5);

/// How often a blocked hibernation (a handoff, a download) is reconsidered.
const RECHECK: Duration = Duration::from_secs(30);

/// Tools after which the session's place may have changed, so the journal is updated.
fn moves_session(tool: &str) -> bool {
    matches!(
        tool,
        "navigate" | "back" | "act" | "tabs" | "session_open" | "evaluate" | "handoff_status"
    )
}

/// Why the next session start resumes from the journal.
#[derive(Debug, Clone, Copy)]
enum Why {
    Idle(Duration),
    Requested,
    Restart,
}

#[derive(Debug)]
struct Resume {
    profile: String,
    why: Why,
}

/// The server's side of durable sessions: activity, the pending resume and the notice.
pub struct Lifecycle {
    /// `None` when idle hibernation is off.
    idle: Option<Duration>,
    last_call: std::sync::Mutex<Instant>,
    in_flight: AtomicUsize,
    resume: std::sync::Mutex<Option<Resume>>,
    /// Told to the agent ahead of the result of the call that resumed the session.
    notice: std::sync::Mutex<Option<String>>,
    changes: AtomicU64,
    writer: Arc<Writer>,
}

impl Lifecycle {
    pub fn from_env(default_profile: &str) -> Self {
        let seconds = |name: &str, unit: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(|n| n * unit)
        };
        // AIBROWSE_IDLE_SECONDS exists for tests, which can't wait minutes.
        let idle = seconds("AIBROWSE_IDLE_SECONDS", 1)
            .or_else(|| seconds("AIBROWSE_IDLE_MINUTES", 60))
            .unwrap_or(15 * 60);
        let resume = std::env::var("AIBROWSE_RESUME")
            .is_ok_and(|v| v == "1" || v == "true")
            .then(|| Resume {
                profile: default_profile.to_owned(),
                why: Why::Restart,
            });
        Self {
            idle: (idle > 0).then(|| Duration::from_secs(idle)),
            last_call: std::sync::Mutex::new(Instant::now()),
            in_flight: AtomicUsize::new(0),
            resume: std::sync::Mutex::new(resume),
            notice: std::sync::Mutex::default(),
            changes: AtomicU64::new(0),
            writer: Arc::default(),
        }
    }

    fn idle_for(&self) -> Duration {
        lock(&self.last_call).elapsed()
    }

    fn touch(&self) {
        *lock(&self.last_call) = Instant::now();
    }

    /// The number of the next journal change. Taken under the session lock, so changes
    /// are numbered in the order they describe.
    fn next_change(&self) -> u64 {
        self.changes.fetch_add(1, Ordering::AcqRel) + 1
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Aibrowse {
    /// Marks a tool call as started: the session is not idle while it runs.
    pub(crate) fn call_started(&self) {
        self.lifecycle.in_flight.fetch_add(1, Ordering::AcqRel);
        self.lifecycle.touch();
    }

    /// Marks a tool call as finished, and journals the session if the call may have
    /// moved it. Returns the notice to put ahead of its result if it resumed the session.
    pub(crate) fn call_finished(&self, tool: &str) -> Option<String> {
        self.lifecycle.touch();
        self.lifecycle.in_flight.fetch_sub(1, Ordering::AcqRel);
        if moves_session(tool) {
            let this = self.clone();
            tokio::spawn(async move { this.journal_now().await });
        }
        lock(&self.lifecycle.notice).take()
    }

    /// Writes the session's place to its journal, holding the session only while
    /// reading it.
    async fn journal_now(&self) {
        let guard = self.live.lock().await;
        let Some(snapshot) = self.snapshot(guard.as_ref()).await else {
            return;
        };
        drop(guard);
        self.write_journal(snapshot).await;
    }

    /// Writes `live`'s journal and waits for it to be on disk: the last record before
    /// Chrome closes.
    async fn journal_final(&self, live: &Live) {
        if let Some(snapshot) = self.snapshot(Some(live)).await {
            self.write_journal(snapshot).await;
        }
    }

    /// The session's place, numbered. Called under the session lock, so a later
    /// snapshot always has a higher number.
    async fn snapshot(&self, live: Option<&Live>) -> Option<(u64, PathBuf, Journal)> {
        let live = live.filter(|l| l.has_tabs())?;
        let seq = self.lifecycle.next_change();
        Some((seq, live.profile_root().to_owned(), live.snapshot().await))
    }

    async fn write_journal(&self, (seq, root, journal): (u64, PathBuf, Journal)) {
        let writer = self.lifecycle.writer.clone();
        let _ = tokio::task::spawn_blocking(move || writer.write(seq, &root, &journal)).await;
    }

    /// Starts a session for `profile`: from its journal when the session was put away
    /// (hibernated, or retired by a restart), otherwise fresh. `headless` overrides the
    /// mode; by default a resumed session keeps the journal's and a fresh one the
    /// server's.
    pub(crate) async fn start_session(
        &self,
        profile: &str,
        headless: Option<bool>,
        attended: Option<bool>,
    ) -> Result<Live, String> {
        // Before the profile is opened, which makes its directory: a profile this
        // machine doesn't have comes from the profile store, when a build has one.
        crate::hooks::before_launch(profile).await;
        let resume = {
            let mut pending = lock(&self.lifecycle.resume);
            pending.take_if(|r| r.profile == profile)
        };
        let root = aibrowse_browser::Profile::open(profile)
            .map(|p| p.root)
            .map_err(|e| e.to_string())?;
        if let Some(resume) = resume
            && let Some(journal) = journal::load(&root)
        {
            let headless = headless.unwrap_or(journal.headless);
            let (live, restored) = Live::resume(profile, headless, &journal)
                .await
                .map_err(|e| e.to_string())?;
            let doing = journal.doing().summary();
            let mut note = notice(resume.why, &restored);
            if !doing.is_empty() {
                note.push(' ');
                note.push_str(&doing);
            }
            *lock(&self.lifecycle.notice) = Some(note);
            return Ok(live);
        }
        let d = &self.defaults;
        Live::start(
            profile,
            headless.unwrap_or(d.headless),
            d.pace,
            attended.unwrap_or(d.attended),
            false,
        )
        .await
        .map_err(|e| e.to_string())
    }

    /// Ends the session for good (`session_close`): closes Chrome and forgets the tabs.
    pub(crate) async fn close_session(&self) -> Result<bool, String> {
        let mut guard = self.live.lock().await;
        let resumable = lock(&self.lifecycle.resume).take();
        let root = match (guard.as_ref(), &resumable) {
            (Some(live), _) => Some(live.profile_root().to_owned()),
            (None, Some(resume)) => aibrowse_browser::Profile::open(&resume.profile)
                .ok()
                .map(|p| p.root),
            (None, None) => None,
        };
        if let Some(root) = root {
            // Numbered like a snapshot, so a snapshot still on its way can't bring the
            // journal back.
            let seq = self.lifecycle.next_change();
            let writer = self.lifecycle.writer.clone();
            let _ = tokio::task::spawn_blocking(move || writer.remove(seq, &root)).await;
        }
        let Some(live) = guard.take() else {
            return Ok(resumable.is_some());
        };
        // Still holding the session lock: a call meanwhile waits rather than launch a
        // second Chrome on a profile that is still closing.
        let profile = live.browser.profile().clone();
        live.browser.close().await.map_err(|e| e.to_string())?;
        crate::hooks::after_close(&profile.name, profile.root).await;
        Ok(true)
    }

    /// Puts the session away: its place to the journal, then Chrome closed patiently.
    /// The next call that needs the browser resumes it. Returns the number of tabs put
    /// away, or `None` if there was no session (already hibernated counts as one).
    async fn hibernate(&self, why: Why) -> Option<usize> {
        let mut guard = self.live.lock().await;
        let Some(live) = guard.take() else {
            return lock(&self.lifecycle.resume).is_some().then_some(0);
        };
        if let Some(server) = self.handoff.get() {
            server.set_tab(None);
        }
        self.journal_final(&live).await;
        let tabs = live.tab_count();
        let profile = live.browser.profile().name.clone();
        let started = Instant::now();
        let grace = match why {
            Why::Idle(_) => IDLE_GRACE,
            Why::Requested | Why::Restart => REQUESTED_GRACE,
        };
        let root = live.browser.profile().root.clone();
        if let Err(err) = live.browser.close_within(grace).await {
            tracing::warn!(%err, "browser did not close cleanly on hibernation");
        }
        crate::hooks::after_close(&profile, root).await;
        tracing::info!(
            tabs,
            close_ms = started.elapsed().as_millis() as u64,
            "session hibernated"
        );
        *lock(&self.lifecycle.resume) = Some(Resume { profile, why });
        drop(guard);
        Some(tabs)
    }

    /// `session_close(hibernate=true)`.
    pub(crate) async fn hibernate_now(&self) -> String {
        match self.hibernate(Why::Requested).await {
            Some(_) => format!(
                "{HIBERNATED}: the browser is closed and the next call reopens its tabs where they were."
            ),
            None => "No session was open.".into(),
        }
    }

    /// Ends the server's session (the host went away): the journal first, so a restart
    /// with `AIBROWSE_RESUME=1` can pick up from here, then Chrome (which saves the
    /// knowledge base before it is told to close).
    pub(crate) async fn shutdown_session(&self) {
        if let Some(live) = self.live.lock().await.take() {
            self.journal_final(&live).await;
            let profile = live.browser.profile().clone();
            if let Err(err) = live.browser.close().await {
                tracing::warn!(%err, "browser did not close cleanly");
            }
            crate::hooks::before_exit(&profile.name, profile.root).await;
        }
    }

    /// Hibernates the session whenever it has been idle long enough. Runs for the life
    /// of the server.
    pub fn watch_idle(&self) {
        let Some(idle) = self.lifecycle.idle else {
            return;
        };
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                let waited = this.lifecycle.idle_for();
                if waited < idle {
                    tokio::time::sleep(idle - waited).await;
                    continue;
                }
                if this.lifecycle.in_flight.load(Ordering::Acquire) == 0
                    && this.may_hibernate().await
                {
                    this.hibernate(Why::Idle(idle)).await;
                }
                // Idle again only after the next call; until then, check now and then
                // whether whatever blocked hibernation is over.
                tokio::time::sleep(RECHECK.min(idle)).await;
            }
        });
    }

    /// Whether nothing needs the browser to stay up: a live session, no human step
    /// pending or watched, and no download still arriving.
    async fn may_hibernate(&self) -> bool {
        if self.pending.lock().await.is_some() {
            return false;
        }
        if let Some(server) = self.handoff.get()
            && (server.pending().is_some() || server.watchers() > 0)
        {
            return false;
        }
        let guard = self.live.lock().await;
        guard.as_ref().is_some_and(|live| {
            !live
                .browser
                .downloads()
                .list()
                .iter()
                .any(|d| d.state == DownloadState::InProgress)
        })
    }
}

/// The line put ahead of the result of the call that resumed the session.
fn notice(why: Why, restored: &Restored) -> String {
    let cause = match why {
        Why::Idle(idle) => format!("was hibernated after {} idle", minutes(idle)),
        Why::Requested => "was hibernated on request".into(),
        Why::Restart => "was restarted with the server".into(),
    };
    let tabs = match restored.tabs {
        1 => "1 tab restored".to_owned(),
        n => format!("{n} tabs restored"),
    };
    let failed = if restored.failed.is_empty() {
        String::new()
    } else {
        format!(
            " ({} could not be reloaded: {})",
            restored.failed.len(),
            restored.failed.join("; ")
        )
    };
    format!("Note: the browser {cause} and resumed: {tabs}{failed}; refs are new, observe again.")
}

fn minutes(idle: Duration) -> String {
    match idle.as_secs() {
        s if s < 60 => format!("{s}s"),
        s => format!("{} min", s / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_says_why_and_what_came_back() {
        let restored = Restored {
            tabs: 3,
            failed: Vec::new(),
        };
        assert_eq!(
            notice(Why::Idle(Duration::from_secs(15 * 60)), &restored),
            "Note: the browser was hibernated after 15 min idle and resumed: 3 tabs restored; refs are new, observe again."
        );
        let partial = Restored {
            tabs: 1,
            failed: vec!["https://a.test/: timed out".into()],
        };
        assert_eq!(
            notice(Why::Restart, &partial),
            "Note: the browser was restarted with the server and resumed: 1 tab restored (1 could not be reloaded: https://a.test/: timed out); refs are new, observe again."
        );
    }
}
