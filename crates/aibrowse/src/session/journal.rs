//! The session journal: a small durable record of where a session was, so it can be
//! put away (hibernation, a reload, a server restart) and resumed with the same tabs.
//!
//! One JSON file per profile, `<profile>/session.json`, replaced atomically (write a
//! temporary file, sync it, rename it over the old one). JSON rather than SQLite: the
//! record is a few hundred bytes that is always written and read whole, by one writer
//! (a profile has at most one live session), so a database would add a connection,
//! WAL files and a schema for no gain; a rename can never leave a half-written record,
//! and the file stays readable by hand. A hosted deployment (B2) keeps the same record
//! in its own database.
//!
//! What it holds is deliberately thin: where each tab was (URL, title, scroll offset),
//! which tab was active, the session's settings, and what the agent was doing: its last
//! goal for the navigator and how each part ended, and how the last few handoffs went
//! (what the user did, never what they typed). Never page content or form values: a
//! resumed tab loads its page afresh, as Chrome's own session restore does for forms.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use aibrowse_input::Pace;
use serde::{Deserialize, Serialize};

/// Bumped when the format changes incompatibly; older records are then ignored.
const VERSION: u32 = 1;

/// The file name inside the profile directory.
const FILE: &str = "session.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub version: u32,
    /// Unix seconds when the session first started; kept across resumes, since a
    /// resumed session is the same session.
    pub started_at: u64,
    /// Unix seconds of this record.
    pub updated_at: u64,
    pub headless: bool,
    pub pace: Pace,
    pub attended: bool,
    /// Index into `tabs` of the tab the agent works in.
    pub active: usize,
    /// In tab-strip order.
    pub tabs: Vec<TabRecord>,
    /// The agent's last goal for the navigator. Absent in records from before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalRecord>,
    /// The last few handoffs, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoffs: Vec<HandoffRecord>,
}

/// A goal as `navigate(goal, then)` took it, and how each part pursued ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalRecord {
    /// The goal and its later parts, in order.
    pub parts: Vec<String>,
    /// How each part pursued ended ("reached", "needs agent"...); parts after the first
    /// one not reached weren't pursued and have none.
    pub outcomes: Vec<String>,
    pub at: u64,
}

/// How a handoff went: its outcome and the user's trail (pages visited, what they
/// clicked, how many characters they typed; never the text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffRecord {
    pub id: String,
    /// "done", "cancelled" or "timed out".
    pub outcome: String,
    pub trail: Vec<String>,
    pub at: u64,
}

/// Handoffs kept, and trail entries per handoff.
pub const HANDOFFS_KEPT: usize = 3;
pub const TRAIL_KEPT: usize = 12;

/// What the session was doing, carried from one record to the next and across resumes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doing {
    pub goal: Option<GoalRecord>,
    pub handoffs: Vec<HandoffRecord>,
}

impl Doing {
    pub fn goal(&mut self, parts: Vec<String>, outcomes: Vec<String>) {
        self.goal = Some(GoalRecord {
            parts,
            outcomes,
            at: now(),
        });
    }

    pub fn handoff(&mut self, id: &str, outcome: &str, mut trail: Vec<String>) {
        trail.truncate(TRAIL_KEPT);
        self.handoffs.push(HandoffRecord {
            id: id.to_owned(),
            outcome: outcome.to_owned(),
            trail,
            at: now(),
        });
        let extra = self.handoffs.len().saturating_sub(HANDOFFS_KEPT);
        self.handoffs.drain(..extra);
    }

    /// For the agent, after a resume: what it was doing before. Empty when nothing.
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        if let Some(goal) = &self.goal {
            let parts: Vec<String> = goal
                .parts
                .iter()
                .enumerate()
                .map(|(n, part)| {
                    let ended = goal.outcomes.get(n).map_or("not started", String::as_str);
                    format!("{part:?} ({ended})")
                })
                .collect();
            lines.push(format!(
                "Your last navigator goal: {}.",
                parts.join(", then ")
            ));
        }
        if let Some(handoff) = self.handoffs.last() {
            let mut line = format!("The last handoff ({}) was {}", handoff.id, handoff.outcome);
            if !handoff.trail.is_empty() {
                line.push_str(&format!("; the user: {}", handoff.trail.join("; ")));
            }
            line.push('.');
            lines.push(line);
        }
        lines.join(" ")
    }
}

impl Journal {
    pub fn doing(&self) -> Doing {
        Doing {
            goal: self.goal.clone(),
            handoffs: self.handoffs.clone(),
        }
    }

    pub fn with_doing(mut self, doing: &Doing) -> Self {
        self.goal = doing.goal.clone();
        self.handoffs = doing.handoffs.clone();
        self
    }
}

/// Where one tab was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabRecord {
    pub url: String,
    pub title: String,
    pub scroll_x: f64,
    pub scroll_y: f64,
}

impl Journal {
    pub fn new(
        started_at: u64,
        headless: bool,
        pace: Pace,
        attended: bool,
        active: usize,
        tabs: Vec<TabRecord>,
    ) -> Self {
        let tabs: Vec<TabRecord> = tabs.into_iter().map(TabRecord::sanitized).collect();
        Self {
            version: VERSION,
            started_at,
            updated_at: now(),
            headless,
            pace,
            attended,
            active: active.min(tabs.len().saturating_sub(1)),
            tabs,
            goal: None,
            handoffs: Vec::new(),
        }
    }
}

impl TabRecord {
    /// Drops what a URL may carry that is not a location: credentials embedded in it
    /// (`https://user:pass@host/`). Query strings stay, since they are often the page.
    fn sanitized(mut self) -> Self {
        if let Ok(mut url) = url::Url::parse(&self.url)
            && (!url.username().is_empty() || url.password().is_some())
        {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            self.url = url.into();
        }
        self.scroll_x = finite(self.scroll_x);
        self.scroll_y = finite(self.scroll_y);
        self
    }

    /// Whether resuming should load this URL. Browser-internal pages (`chrome://`,
    /// `devtools://`, error pages) are left blank rather than reopened.
    pub fn restorable(&self) -> bool {
        ["http://", "https://", "file://"]
            .iter()
            .any(|scheme| self.url.starts_with(scheme))
    }
}

fn finite(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn path(profile_root: &Path) -> PathBuf {
    profile_root.join(FILE)
}

/// The profile's journal, if it has a readable one of this version.
pub fn load(profile_root: &Path) -> Option<Journal> {
    let bytes = std::fs::read(path(profile_root)).ok()?;
    let journal: Journal = serde_json::from_slice(&bytes).ok()?;
    (journal.version == VERSION).then_some(journal)
}

/// Forgets the profile's session (it was closed on purpose).
pub fn remove(profile_root: &Path) {
    match std::fs::remove_file(path(profile_root)) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(%err, "could not remove the session journal"),
    }
}

/// Replaces the journal atomically: readers see the old record or the new one, never a
/// mix, even if the process dies mid-write.
pub fn save(profile_root: &Path, journal: &Journal) -> std::io::Result<()> {
    let target = path(profile_root);
    let temporary = profile_root.join(format!(".{FILE}.tmp"));
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(journal).map_err(std::io::Error::other)?)?;
    // On disk before the rename, or a crash could leave the new name on empty data.
    file.sync_data()?;
    drop(file);
    std::fs::rename(&temporary, &target)
}

/// Serializes journal writes that are made off the tool call's path. Changes are
/// numbered when taken (under the session lock, so in order), and one that arrives
/// after a newer change to the same profile's journal is dropped: a slow background
/// write never overwrites the final record written at hibernation, nor brings back a
/// journal that `session_close` removed.
#[derive(Debug, Default)]
pub struct Writer {
    /// Per profile directory, the number of the last change applied.
    applied: Mutex<HashMap<PathBuf, u64>>,
}

impl Writer {
    /// Writes snapshot number `seq` unless a newer change is already applied.
    pub fn write(&self, seq: u64, profile_root: &Path, journal: &Journal) {
        self.apply(seq, profile_root, || save(profile_root, journal));
    }

    /// Removes the journal as change number `seq`.
    pub fn remove(&self, seq: u64, profile_root: &Path) {
        self.apply(seq, profile_root, || {
            remove(profile_root);
            Ok(())
        });
    }

    fn apply(&self, seq: u64, profile_root: &Path, change: impl FnOnce() -> std::io::Result<()>) {
        let mut applied = self.applied.lock().unwrap_or_else(|p| p.into_inner());
        let last = applied.entry(profile_root.to_owned()).or_default();
        if seq <= *last {
            return;
        }
        match change() {
            Ok(()) => *last = seq,
            Err(err) => tracing::warn!(%err, "could not write the session journal"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(url: &str) -> TabRecord {
        TabRecord {
            url: url.into(),
            title: "Title".into(),
            scroll_x: 0.0,
            scroll_y: 640.0,
        }
    }

    fn sample() -> Journal {
        Journal::new(
            1_700_000_000,
            true,
            Pace::Fast,
            false,
            1,
            vec![tab("https://a.test/x?q=1"), tab("https://b.test/")],
        )
    }

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let journal = sample();
        save(dir.path(), &journal).unwrap();
        assert_eq!(load(dir.path()), Some(journal));
    }

    #[test]
    fn write_replaces_atomically_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &sample()).unwrap();
        let mut second = sample();
        second.tabs.truncate(1);
        second.active = 0;
        save(dir.path(), &second).unwrap();
        assert_eq!(load(dir.path()), Some(second));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [FILE]);
    }

    #[test]
    fn unreadable_or_foreign_records_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), None);
        std::fs::write(path(dir.path()), b"{\"version\": 1, \"tabs\": [").unwrap();
        assert_eq!(load(dir.path()), None);
        let mut future = serde_json::to_value(sample()).unwrap();
        future["version"] = 99.into();
        std::fs::write(path(dir.path()), future.to_string()).unwrap();
        assert_eq!(load(dir.path()), None);
        remove(dir.path());
        remove(dir.path());
        assert!(!path(dir.path()).exists());
    }

    /// The record holds locations and settings only: any new field must be added here
    /// on purpose, after checking it can't carry what a page shows or a user types.
    #[test]
    fn records_no_page_content() {
        let value = serde_json::to_value(sample()).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "active",
                "attended",
                "headless",
                "pace",
                "started_at",
                "tabs",
                "updated_at",
                "version"
            ]
        );
        let mut tab_keys: Vec<&str> = value["tabs"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        tab_keys.sort_unstable();
        assert_eq!(tab_keys, ["scroll_x", "scroll_y", "title", "url"]);
    }

    #[test]
    fn urls_lose_embedded_credentials() {
        let journal = Journal::new(
            0,
            false,
            Pace::Human,
            true,
            0,
            vec![tab("https://me:secret@example.test/inbox?folder=2")],
        );
        assert_eq!(journal.tabs[0].url, "https://example.test/inbox?folder=2");
        assert!(!serde_json::to_string(&journal).unwrap().contains("secret"));
    }

    #[test]
    fn active_index_and_scroll_are_clamped() {
        let mut odd = tab("https://a.test/");
        odd.scroll_y = f64::NAN;
        odd.scroll_x = -5.0;
        let journal = Journal::new(0, false, Pace::Human, true, 7, vec![odd]);
        assert_eq!(journal.active, 0);
        assert_eq!(
            (journal.tabs[0].scroll_x, journal.tabs[0].scroll_y),
            (0.0, 0.0)
        );
    }

    #[test]
    fn only_web_pages_are_reopened() {
        assert!(tab("https://a.test/").restorable());
        assert!(tab("file:///tmp/a.html").restorable());
        assert!(!tab("about:blank").restorable());
        assert!(!tab("chrome://settings").restorable());
    }

    #[test]
    fn writer_drops_stale_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::default();
        let mut newer = sample();
        newer.active = 0;
        writer.write(2, dir.path(), &newer);
        writer.write(1, dir.path(), &sample());
        assert_eq!(load(dir.path()), Some(newer));
        // Each profile's journal only compares against its own last change.
        let other = tempfile::tempdir().unwrap();
        let first = sample();
        writer.write(1, other.path(), &first);
        assert_eq!(load(other.path()), Some(first));
    }

    #[test]
    fn a_removed_journal_is_not_brought_back_by_a_late_write() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::default();
        writer.write(1, dir.path(), &sample());
        writer.remove(3, dir.path());
        writer.write(2, dir.path(), &sample());
        assert_eq!(load(dir.path()), None);
    }

    #[test]
    fn what_the_agent_was_doing_comes_back() {
        let mut doing = Doing::default();
        doing.goal(
            vec![
                "Enter =A1+1 in A2".into(),
                "Select A2 to A10".into(),
                "Fill down".into(),
            ],
            vec!["reached".into(), "needs agent".into()],
        );
        for n in 0..5 {
            doing.handoff(&format!("h{n}"), "done", vec!["visited Sign in".into(); 20]);
        }
        assert_eq!(doing.handoffs.len(), HANDOFFS_KEPT);
        assert_eq!(doing.handoffs[0].id, "h2");
        assert_eq!(doing.handoffs[2].trail.len(), TRAIL_KEPT);

        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &sample().with_doing(&doing)).unwrap();
        let back = load(dir.path()).unwrap().doing();
        assert_eq!(back, doing);
        let summary = back.summary();
        assert!(
            summary.starts_with("Your last navigator goal: \"Enter =A1+1 in A2\" (reached), then \"Select A2 to A10\" (needs agent), then \"Fill down\" (not started)."),
            "{summary}"
        );
        assert!(
            summary.contains("The last handoff (h4) was done; the user: visited Sign in"),
            "{summary}"
        );
    }

    #[test]
    fn records_from_before_goals_were_kept_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut old = serde_json::to_value(sample()).unwrap();
        old.as_object_mut().unwrap().remove("goal");
        old.as_object_mut().unwrap().remove("handoffs");
        std::fs::write(path(dir.path()), old.to_string()).unwrap();
        let back = load(dir.path()).expect("an older record");
        assert_eq!(back.doing(), Doing::default());
        assert_eq!(back.doing().summary(), "");
    }
}
