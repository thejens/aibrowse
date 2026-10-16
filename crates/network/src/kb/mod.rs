//! The Site Knowledge Base (docs/design.md §5.4): the structure of each site's API,
//! persisted across sessions in SQLite, so a new session already knows the endpoints
//! earlier ones discovered.
//!
//! SQLite is synchronous, so the store lives on its own thread. The catalog talks to it
//! through a channel and is never locked across I/O: the thread takes a snapshot of the
//! changed endpoints under the lock, then writes without it.

mod paths;
mod record;
mod store;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::oneshot;

use crate::catalog::WeakCatalog;
pub use paths::{forget_path, learn_path, recall_path};
pub(crate) use record::Record;
use store::Store;

/// Changes are batched: a chatty page makes dozens of calls a second, and each write is a
/// transaction.
const WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// Where the knowledge base lives: `AIBROWSE_KB`, or `~/.aibrowse/kb.sqlite`.
/// `AIBROWSE_KB=off` disables it.
pub fn knowledge_base_path() -> Option<PathBuf> {
    match std::env::var_os("AIBROWSE_KB") {
        Some(path) if path == "off" => None,
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => dirs::home_dir().map(|home| home.join(".aibrowse/kb.sqlite")),
    }
}

enum Command {
    /// Add the endpoints known for a site to the catalog.
    Load(String),
    /// Answer once every earlier command is done.
    Barrier(oneshot::Sender<()>),
    /// Write pending changes now, then answer.
    Flush(oneshot::Sender<()>),
}

/// The handle to the knowledge base's thread. The thread ends when the catalog that owns
/// this handle is dropped.
#[derive(Clone)]
pub(crate) struct Worker {
    commands: mpsc::Sender<Command>,
}

impl Worker {
    pub(crate) fn spawn(path: PathBuf, catalog: WeakCatalog) -> Self {
        let (commands, receiver) = mpsc::channel();
        if let Err(err) = std::thread::Builder::new()
            .name("aibrowse-kb".into())
            .spawn(move || run(&path, &receiver, &catalog))
        {
            tracing::warn!(%err, "could not start the site knowledge base");
        }
        Self { commands }
    }

    pub(crate) fn load(&self, site: String) {
        // A failed send means the store couldn't be opened (already logged): the session
        // simply starts without earlier knowledge.
        let _ = self.commands.send(Command::Load(site));
    }

    pub(crate) async fn barrier(&self) {
        self.round_trip(Command::Barrier).await;
    }

    pub(crate) async fn flush(&self) {
        self.round_trip(Command::Flush).await;
    }

    async fn round_trip(&self, command: fn(oneshot::Sender<()>) -> Command) {
        let (done, wait) = oneshot::channel();
        if self.commands.send(command(done)).is_ok() {
            let _ = wait.await;
        }
    }
}

fn run(path: &Path, commands: &Receiver<Command>, catalog: &WeakCatalog) {
    let mut store = match Store::open(path) {
        Ok(store) => store,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "site knowledge base unavailable; API knowledge won't persist");
            return;
        }
    };
    let mut next_write = Instant::now() + WRITE_INTERVAL;
    loop {
        let wait = next_write.saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(Command::Load(site)) => match store.load(&site) {
                Ok(records) => {
                    let Some(catalog) = catalog.upgrade() else {
                        return;
                    };
                    catalog.learn(records);
                }
                Err(err) => tracing::warn!(%err, %site, "could not load the site's known API"),
            },
            Ok(Command::Barrier(done)) => {
                let _ = done.send(());
            }
            Ok(Command::Flush(done)) => {
                write(&mut store, catalog);
                let _ = done.send(());
            }
            Err(RecvTimeoutError::Timeout) => {
                if !write(&mut store, catalog) {
                    return;
                }
                next_write = Instant::now() + WRITE_INTERVAL;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Writes the catalog's pending changes. False once the catalog is gone.
fn write(store: &mut Store, catalog: &WeakCatalog) -> bool {
    let Some(catalog) = catalog.upgrade() else {
        return false;
    };
    let records = catalog.take_changes();
    // Not kept across the write: the browser may be closing.
    drop(catalog);
    if !records.is_empty()
        && let Err(err) = store.save(records, unix_now())
    {
        tracing::warn!(%err, "could not save learned API structure");
    }
    true
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
