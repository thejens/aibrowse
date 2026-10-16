//! Downloads: where files the page downloads land, and what the agent is told about them.
//!
//! Every download of a profile lands in `~/.aibrowse/downloads/<profile>/`, never the
//! user's own Downloads folder: what an agent fetches stays apart from what the user
//! saved. The folder is set in the profile's preferences before Chrome starts, and
//! Chrome's own download flow does the rest, as for a person: it sanitizes the
//! site-suggested name (no path traversal, no hidden files), never overwrites (it
//! numbers duplicates), and writes `<name>.crdownload` until the file is complete.
//!
//! Deliberately not used: `Browser.setDownloadBehavior`. In Chrome 154 it crashes the
//! browser on the next download whenever the profile already has downloads in its
//! history (from any earlier session), whatever the behavior or events setting. So
//! downloads are tracked by watching the folder instead of through CDP events.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{Instant, sleep};

use crate::error::Result;

/// Chrome's suffix for a file still being downloaded.
const IN_PROGRESS: &str = ".crdownload";
/// A download that hasn't appeared yet when the action settles usually does within this:
/// the response that turns into a download can arrive after the page went quiet.
const APPEAR_WAIT: Duration = Duration::from_millis(600);
const POLL: Duration = Duration::from_millis(100);

/// Where a download stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadState {
    InProgress,
    Completed,
    /// The partial file went away without a finished file: canceled by the page, the
    /// server, or a network error.
    Failed,
}

/// One file the session downloaded.
#[derive(Debug, Clone)]
pub struct Download {
    /// The file name Chrome chose (sanitized and made unique by Chrome).
    pub name: String,
    /// The finished file, or the partial `.crdownload` file while in progress.
    pub path: PathBuf,
    pub bytes: u64,
    pub state: DownloadState,
    /// The state the agent was last told about, if any.
    reported: Option<DownloadState>,
}

impl Download {
    /// A line for the agent, e.g. `downloaded "report.pdf" (12.3 KB) to /…/report.pdf`.
    pub fn describe(&self) -> String {
        match self.state {
            DownloadState::Completed => format!(
                "downloaded {:?} ({}) to {}",
                self.name,
                human_size(self.bytes),
                self.path.display()
            ),
            DownloadState::InProgress => format!(
                "download of {:?} in progress ({} so far), saving into {}",
                self.name,
                human_size(self.bytes),
                self.path.parent().unwrap_or(&self.path).display()
            ),
            DownloadState::Failed => {
                format!("download of {:?} failed or was canceled", self.name)
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    /// Finished files in the folder that are accounted for: there before the session,
    /// or already recorded as downloads.
    known: HashSet<OsString>,
    downloads: Vec<Download>,
}

/// The downloads of one browser session, shared by its tabs.
#[derive(Clone)]
pub struct Downloads {
    dir: PathBuf,
    registry: Arc<Mutex<Registry>>,
}

impl Downloads {
    /// Watches `dir`, which the profile's preferences point downloads at
    /// (`profile_prefs::prepare`).
    pub(crate) fn prepare(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let downloads = Self {
            dir,
            registry: Arc::default(),
        };
        // What is already there belongs to earlier sessions.
        let existing = downloads.listing();
        downloads.lock().known = existing.into_iter().map(|(name, _)| name).collect();
        Ok(downloads)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every download of this session, oldest first.
    pub fn list(&self) -> Vec<Download> {
        self.scan();
        self.lock().downloads.clone()
    }

    /// A marker for "downloads that begin from now on" (see [`Downloads::report`]).
    pub(crate) fn mark(&self) -> usize {
        self.scan();
        self.lock().downloads.len()
    }

    /// Waits up to `wait` for downloads begun since `mark` to finish (small files usually
    /// do), then returns a note for every download the agent hasn't been told about in
    /// its current state: those this action started, and older ones that finished or
    /// failed since.
    pub(crate) async fn report(&self, mark: usize, wait: Duration) -> Vec<String> {
        let started = Instant::now();
        loop {
            self.scan();
            let (begun, pending) = {
                let registry = self.lock();
                let mine = registry.downloads.get(mark..).unwrap_or_default();
                (
                    !mine.is_empty(),
                    mine.iter().any(|d| d.state == DownloadState::InProgress),
                )
            };
            let elapsed = started.elapsed();
            let appearing = !begun && elapsed < APPEAR_WAIT && self.has_unnamed_partial();
            if elapsed >= wait || !(pending || appearing) {
                break;
            }
            sleep(POLL).await;
        }
        let mut registry = self.lock();
        registry
            .downloads
            .iter_mut()
            .filter(|d| d.reported != Some(d.state))
            .map(|d| {
                d.reported = Some(d.state);
                match d.state {
                    DownloadState::InProgress => {
                        format!("{}; call downloads to wait for it", d.describe())
                    }
                    _ => d.describe(),
                }
            })
            .collect()
    }

    /// Waits up to `wait` until no download is in progress.
    pub async fn wait_idle(&self, wait: Duration) {
        let deadline = Instant::now() + wait;
        loop {
            self.scan();
            let busy = self
                .lock()
                .downloads
                .iter()
                .any(|d| d.state == DownloadState::InProgress);
            if !busy || Instant::now() >= deadline {
                return;
            }
            sleep(POLL).await;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Regular files in the folder, with sizes.
    fn listing(&self) -> Vec<(OsString, u64)> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let meta = entry.metadata().ok()?;
                meta.is_file().then(|| (entry.file_name(), meta.len()))
            })
            .collect()
    }

    /// Chrome's placeholder ("Unconfirmed 123.crdownload") for a download whose name
    /// it hasn't settled on yet; it is renamed moments later.
    fn has_unnamed_partial(&self) -> bool {
        self.listing()
            .iter()
            .any(|(name, _)| unnamed_partial(&name.to_string_lossy()))
    }

    /// Brings the registry up to date with the folder: new partial files are downloads
    /// in progress, new finished files are completed downloads (finishing the partial
    /// one of the same name, if any), and partial files that vanished without a
    /// finished file are failures.
    fn scan(&self) {
        let listing = self.listing();
        let mut registry = self.lock();
        for (file, bytes) in &listing {
            let text = file.to_string_lossy();
            if unnamed_partial(&text) {
                continue;
            }
            if let Some(name) = text.strip_suffix(IN_PROGRESS) {
                match registry
                    .downloads
                    .iter_mut()
                    .find(|d| d.state == DownloadState::InProgress && d.name == name)
                {
                    Some(download) => download.bytes = *bytes,
                    None => registry.downloads.push(Download {
                        name: name.to_owned(),
                        path: self.dir.join(file),
                        bytes: *bytes,
                        state: DownloadState::InProgress,
                        reported: None,
                    }),
                }
            } else if registry.known.insert(file.clone()) {
                let name = text.into_owned();
                let path = self.dir.join(file);
                match registry
                    .downloads
                    .iter_mut()
                    .find(|d| d.state == DownloadState::InProgress && d.name == name)
                {
                    // Keeps `reported`, so the agent hears that it finished.
                    Some(download) => {
                        download.path = path;
                        download.bytes = *bytes;
                        download.state = DownloadState::Completed;
                    }
                    None => registry.downloads.push(Download {
                        name,
                        path,
                        bytes: *bytes,
                        state: DownloadState::Completed,
                        reported: None,
                    }),
                }
            }
        }
        let present: HashSet<&OsString> = listing.iter().map(|(name, _)| name).collect();
        for download in &mut registry.downloads {
            let gone = download
                .path
                .file_name()
                .is_none_or(|name| !present.contains(&name.to_os_string()));
            if download.state == DownloadState::InProgress && gone {
                download.state = DownloadState::Failed;
            }
        }
    }
}

fn unnamed_partial(file: &str) -> bool {
    file.starts_with("Unconfirmed ") && file.ends_with(IN_PROGRESS)
}

/// "512 B", "12.3 KB", "4.0 MB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("aibrowse-dl-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("downloads")).unwrap();
            Self(root)
        }

        fn downloads(&self) -> Downloads {
            Downloads::prepare(self.0.join("downloads")).unwrap()
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join("downloads").join(name)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn earlier_files_are_not_this_sessions_downloads() {
        let folder = Folder::new("earlier");
        std::fs::write(folder.file("old.pdf"), "old").unwrap();
        let downloads = folder.downloads();
        let mark = downloads.mark();
        assert!(block_on(downloads.report(mark, Duration::ZERO)).is_empty());
    }

    #[test]
    fn a_download_is_followed_from_partial_to_finished() {
        let folder = Folder::new("follow");
        let downloads = folder.downloads();
        let mark = downloads.mark();
        std::fs::write(folder.file("report.csv.crdownload"), [0u8; 100]).unwrap();
        let notes = block_on(downloads.report(mark, Duration::ZERO));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("in progress (100 B so far)"), "{notes:?}");

        std::fs::rename(
            folder.file("report.csv.crdownload"),
            folder.file("report.csv"),
        )
        .unwrap();
        let notes = block_on(downloads.report(downloads.mark(), Duration::ZERO));
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].starts_with("downloaded \"report.csv\" (100 B)"),
            "{notes:?}"
        );
        assert_eq!(downloads.list().len(), 1);
    }

    #[test]
    fn a_vanished_partial_file_is_a_failure() {
        let folder = Folder::new("failed");
        let downloads = folder.downloads();
        let mark = downloads.mark();
        std::fs::write(folder.file("big.iso.crdownload"), [0u8; 5]).unwrap();
        block_on(downloads.report(mark, Duration::ZERO));
        std::fs::remove_file(folder.file("big.iso.crdownload")).unwrap();
        let notes = block_on(downloads.report(downloads.mark(), Duration::ZERO));
        assert!(notes[0].contains("failed"), "{notes:?}");
    }

    #[test]
    fn sizes_read_like_a_person_writes_them() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12_600), "12.3 KB");
        assert_eq!(human_size(4 * 1024 * 1024), "4.0 MB");
    }
}
