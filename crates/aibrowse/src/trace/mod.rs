//! Session traces (docs/design.md §5.7, J13 "prove what happened"): every tool call is
//! recorded with what it was asked, what it did and what the page looked like after,
//! so a run can be audited, debugged and replayed as eval data. `aibrowse trace` turns a
//! trace into a static HTML page.
//!
//! Recording happens at the tool-call boundary (`record`), so tools don't each need to
//! know about it; they only report structured facts their text result doesn't carry
//! (`notes`). Writing happens on a thread of its own (`writer`).

pub mod event;
pub mod notes;
mod record;
pub mod viewer;
pub mod writer;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, sync_channel};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use writer::Message;

/// Where traces go: `AIBROWSE_TRACES` (a directory, or `off`), by default
/// `~/.aibrowse/traces`. `None` when tracing is off.
pub fn root() -> Option<PathBuf> {
    match std::env::var("AIBROWSE_TRACES") {
        Ok(v) if v == "off" || v == "0" => None,
        Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => dirs::home_dir().map(|home| home.join(".aibrowse").join("traces")),
    }
}

/// The recorder for this server's tool calls. Cheap to clone; clones share the writer.
#[derive(Clone, Default)]
pub struct Tracer {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    tx: Sender<Message>,
    seq: AtomicU64,
    /// Calls whose event is still being put together (the page capture runs after the
    /// tool has answered); `flush` waits for them.
    in_flight: AtomicUsize,
}

/// A tool call being traced: what is known when it starts.
pub struct Call {
    seq: u64,
    started: Instant,
    wall: SystemTime,
    tool: String,
    args: Value,
}

impl Tracer {
    pub fn from_env() -> Self {
        Self {
            inner: root().map(|root| {
                Arc::new(Inner {
                    tx: writer::spawn(root),
                    seq: AtomicU64::new(0),
                    in_flight: AtomicUsize::new(0),
                })
            }),
        }
    }

    pub fn is_on(&self) -> bool {
        self.inner.is_some()
    }

    /// Starts tracing a call. `args` must already be redacted.
    fn begin(&self, tool: &str, args: Value) -> Option<Call> {
        let inner = self.inner.as_ref()?;
        Some(Call {
            seq: inner.seq.fetch_add(1, Ordering::Relaxed) + 1,
            started: Instant::now(),
            wall: SystemTime::now(),
            tool: tool.to_owned(),
            args,
        })
    }

    fn send(&self, message: Message) {
        if let Some(inner) = &self.inner {
            // The writer only goes away if its thread couldn't start; tracing is off then.
            let _ = inner.tx.send(message);
        }
    }

    fn track(&self, delta: isize) {
        if let Some(inner) = &self.inner {
            if delta > 0 {
                inner.in_flight.fetch_add(1, Ordering::AcqRel);
            } else {
                inner.in_flight.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    /// Waits (briefly) until every call so far is on disk, e.g. before the process exits.
    pub async fn flush(&self) {
        let Some(inner) = &self.inner else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while inner.in_flight.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (done, wait) = sync_channel(1);
        self.send(Message::Flush(done));
        let _ =
            tokio::task::spawn_blocking(move || wait.recv_timeout(Duration::from_secs(5))).await;
    }
}
