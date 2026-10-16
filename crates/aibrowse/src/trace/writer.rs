//! Writing traces to disk on a thread of their own, so tool calls never wait for the
//! disk, and keeping the trace folder bounded.
//!
//! Layout: `<root>/<profile>/<session start>/` holds `events.jsonl` (one event per tool
//! call), `screens/<seq>.jpg` and `outlines/<seq>.txt`. Traces hold page content and
//! screenshots of the user's accounts, so directories are created 0700 and files 0600.

use std::fs::{self, File, OpenOptions};

use crate::private_fs::{Private, private_dir};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel};
use std::time::{Duration, SystemTime};

use base64::Engine;

use super::event::{Event, dir_stamp};

/// Trace sessions older than this (by last write) are deleted.
pub const MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// Trace sessions kept per profile, newest first.
pub const MAX_SESSIONS: usize = 50;
/// Bytes kept per profile; the oldest sessions go first.
pub const MAX_BYTES: u64 = 1 << 30;

pub enum Message {
    Event {
        profile: String,
        event: Box<Event>,
        /// Base64 JPEG, as Chrome returns it; decoded here, off the tool's path.
        screenshot: Option<String>,
        outline: Option<String>,
    },
    /// The browser session ended: the next event starts a new trace directory.
    Rotate,
    /// Answered once everything sent before it is on disk.
    Flush(SyncSender<()>),
}

/// Starts the writer thread for traces under `root`.
pub fn spawn(root: PathBuf) -> Sender<Message> {
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("aibrowse-trace".into())
        .spawn(move || Writer { root, open: None }.run(rx));
    if let Err(err) = spawned {
        tracing::warn!(%err, "could not start the trace writer; tracing is off");
    }
    tx
}

struct Writer {
    root: PathBuf,
    open: Option<Open>,
}

/// The trace directory being written.
struct Open {
    profile: String,
    dir: PathBuf,
    events: File,
}

impl Writer {
    fn run(mut self, rx: Receiver<Message>) {
        for message in rx {
            match message {
                Message::Event {
                    profile,
                    event,
                    screenshot,
                    outline,
                } => {
                    if let Err(err) = self.write(&profile, *event, screenshot, outline) {
                        tracing::warn!(%err, "could not write a trace event");
                    }
                }
                Message::Rotate => self.open = None,
                Message::Flush(done) => {
                    let _ = done.send(());
                }
            }
        }
    }

    fn write(
        &mut self,
        profile: &str,
        mut event: Event,
        screenshot: Option<String>,
        outline: Option<String>,
    ) -> std::io::Result<()> {
        if self.open.as_ref().is_none_or(|o| o.profile != profile) {
            self.open = Some(self.start(profile)?);
        }
        let open = self.open.as_mut().expect("opened above");
        let seq = event.seq;
        if let Some(jpeg) =
            screenshot.and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
        {
            let path = format!("screens/{seq:04}.jpg");
            write_private(&open.dir.join(&path), &jpeg)?;
            event.screenshot = Some(path);
        }
        if let Some(text) = outline {
            let path = format!("outlines/{seq:04}.txt");
            write_private(&open.dir.join(&path), text.as_bytes())?;
            event.outline = Some(path);
        }
        let mut line = serde_json::to_vec(&event).map_err(std::io::Error::other)?;
        line.push(b'\n');
        open.events.write_all(&line)
    }

    /// Opens a new trace directory for `profile`, pruning old ones first.
    fn start(&self, profile: &str) -> std::io::Result<Open> {
        let profile_dir = self.root.join(safe_name(profile));
        private_dir(&self.root)?;
        private_dir(&profile_dir)?;
        prune(&profile_dir, SystemTime::now());
        let stamp = dir_stamp(SystemTime::now());
        // Two sessions can start within a second (a restart, a reload).
        let dir = (1..)
            .map(|n| match n {
                1 => profile_dir.join(&stamp),
                n => profile_dir.join(format!("{stamp}-{n}")),
            })
            .find(|dir| !dir.exists())
            .expect("an unused name exists");
        private_dir(&dir)?;
        private_dir(&dir.join("screens"))?;
        private_dir(&dir.join("outlines"))?;
        let events = OpenOptions::new()
            .create(true)
            .append(true)
            .private()
            .open(dir.join("events.jsonl"))?;
        tracing::info!(dir = %dir.display(), "recording a trace");
        Ok(Open {
            profile: profile.to_owned(),
            dir,
            events,
        })
    }
}

/// Profile names become directory names; keep them to one plain path component.
fn safe_name(profile: &str) -> String {
    let name: String = profile
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect();
    match name.trim_matches('.') {
        "" => "_".into(),
        _ => name,
    }
}

/// Writes a file only the user can read.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .private()
        .open(path)?
        .write_all(bytes)
}

/// Deletes trace sessions in `profile_dir` that are older than [`MAX_AGE`], beyond the
/// newest [`MAX_SESSIONS`], or beyond [`MAX_BYTES`] in total (oldest first). Runs before
/// a new session is created, so the one being written is never touched.
pub fn prune(profile_dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(profile_dir) else {
        return;
    };
    let mut sessions: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let path = e.path();
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, size(&path), path)
        })
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.0)); // newest first
    let mut kept_bytes = 0;
    for (i, (modified, bytes, path)) in sessions.into_iter().enumerate() {
        let old = now.duration_since(modified).unwrap_or_default() > MAX_AGE;
        kept_bytes += bytes;
        if old || i >= MAX_SESSIONS || kept_bytes > MAX_BYTES {
            if let Err(err) = fs::remove_dir_all(&path) {
                tracing::warn!(%err, dir = %path.display(), "could not delete an old trace");
            }
            kept_bytes -= bytes;
        }
    }
}

/// Bytes in the files directly in `dir` and one level down (the trace layout).
fn size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => size(&e.path()),
            _ => e.metadata().map_or(0, |m| m.len()),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;

    use super::*;

    fn session(profile_dir: &Path, name: &str, age_days: u64, now: SystemTime) {
        let dir = profile_dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), "{}\n").unwrap();
        let when = now - Duration::from_secs(age_days * 24 * 60 * 60);
        File::open(&dir)
            .unwrap()
            .set_times(FileTimes::new().set_modified(when))
            .unwrap();
    }

    #[test]
    fn old_and_surplus_sessions_are_pruned() {
        let root = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        session(root.path(), "stale", 20, now);
        for i in 0..MAX_SESSIONS + 2 {
            session(root.path(), &format!("s{i:03}"), i as u64 % 10, now);
        }
        prune(root.path(), now);
        let left: Vec<String> = fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), MAX_SESSIONS);
        assert!(!left.contains(&"stale".to_owned()));
    }

    #[test]
    #[cfg(unix)]
    fn files_are_private_and_events_reference_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = Writer {
            root: root.path().join("traces"),
            open: None,
        };
        let jpeg = base64::engine::general_purpose::STANDARD.encode(b"\xff\xd8jpeg");
        let event = Event {
            seq: 7,
            tool: "act".into(),
            ..Event::default()
        };
        writer
            .write("a/../b", event, Some(jpeg), Some("[e1] link".into()))
            .unwrap();
        let dir = writer.open.as_ref().unwrap().dir.clone();
        assert!(dir.starts_with(root.path().join("traces").join("a_.._b")));
        let line = fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert!(
            line.contains(r#""screenshot":"screens/0007.jpg""#),
            "{line}"
        );
        assert!(line.contains(r#""outline":"outlines/0007.txt""#), "{line}");
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.join("screens/0007.jpg")), 0o600);
        assert_eq!(mode(&dir.join("events.jsonl")), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&root.path().join("traces")), 0o700);
    }
}
