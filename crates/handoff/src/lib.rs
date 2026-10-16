//! Human handoff (docs/design.md §5.6): when the agent hits something only the user can
//! do (logging in, a 2FA code, a CAPTCHA), it hands the browser to the user through a
//! live view, a web page that streams the tab and forwards the user's real input. The
//! agent is paused until the user presses Done (or Cancel).
//!
//! The live view works the same for headless and headful browsers, locally or remotely:
//! it only needs the tab's CDP session. Access is guarded by an unguessable per-handoff
//! token in the URL; by default the server listens on localhost only.

mod input;
mod kinematics;
mod server;
mod stream;
mod trail;
mod watch_view;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aibrowse_cdp::Session;
use rand::RngExt;
use tokio::sync::watch;

pub use kinematics::{Recording, load as load_kinematics};
pub use server::HandoffServer;

/// Where a handoff stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Waiting for the user, who may be working in the live view.
    Pending,
    /// The user pressed Done: the agent may continue.
    Done,
    /// The user declined, or the handoff expired.
    Cancelled,
}

/// What the user is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Take over the browser (log in, solve a CAPTCHA...), then hand it back.
    Takeover,
    /// Approve or deny an action the agent is about to take.
    Approval,
    /// Relay a one-time code (2FA, an SMS code): the user types it into a single box,
    /// without the page, and it is typed into the page's field for them.
    Code,
}

/// One request for the user's help, live until resolved.
pub(crate) struct Active {
    pub id: String,
    pub token: String,
    pub kind: Kind,
    /// The user took over on their own (from the watch view), rather than the agent
    /// asking. Such a handoff ends only when they hand back.
    pub by_user: bool,
    pub trail: trail::Trail,
    /// Records the user's input kinematics, when the profile opted in.
    pub recorder: Option<Arc<kinematics::Recorder>>,
    pub reason: String,
    /// For a code relay: the field (backend node id, in the page itself) the code goes
    /// into; without one, it goes wherever the page has focus.
    pub code_field: Option<i64>,
    pub session: Session,
    pub created: Instant,
    /// How long it waits for the user before giving up on them (they may be away from
    /// the computer); `None`: as long as it takes, when nothing can go on without them.
    pub give_up_after: Option<std::time::Duration>,
    /// It was given up on (expired), rather than declined by the user.
    pub timed_out: bool,
    pub status: watch::Sender<Status>,
}

pub(crate) type Shared = Arc<Mutex<Option<Active>>>;

pub(crate) fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Option<Active>> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the agent gets back when it requests a handoff.
#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: String,
    pub url: String,
}

/// 128 random bits, hex encoded: the live view grants full control of the browser, so
/// the URL must not be guessable.
pub fn token() -> String {
    let mut rng = rand::rng();
    (0..16)
        .map(|_| format!("{:02x}", rng.random::<u8>()))
        .collect()
}

/// Compares two secrets in time that depends only on their lengths, so a client can't
/// learn a token byte by byte from response times. Lengths aren't secret (tokens have a
/// fixed format).
pub fn same_token(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected
            .bytes()
            .zip(given.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Where the live view is reachable. `AIBROWSE_HANDOFF_BIND` (e.g. `0.0.0.0:7777`) and
/// `AIBROWSE_HANDOFF_HOST` (e.g. a Tailscale name) expose it beyond this machine, for
/// handing off to a phone. Behind a reverse proxy, `AIBROWSE_PUBLIC_URL` (e.g.
/// `https://browse.example.com`) is what links start with instead.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub public_host: Option<String>,
    /// The externally visible origin (and optional path prefix) of the live view. Links
    /// are `<public_url>/h/<token>` and `<public_url>/w/<token>`; the proxy forwards those
    /// paths to `bind` unchanged (the page derives its WebSocket URL from its own).
    pub public_url: Option<String>,
    /// A fixed watch-view token (`AIBROWSE_WATCH_TOKEN`). The reload front sets it, with
    /// a fixed bind address, so the watch link survives a server restart.
    pub watch_token: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: std::env::var("AIBROWSE_HANDOFF_BIND")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0))),
            public_host: std::env::var("AIBROWSE_HANDOFF_HOST").ok(),
            public_url: public_url(std::env::var("AIBROWSE_PUBLIC_URL").ok()),
            watch_token: std::env::var("AIBROWSE_WATCH_TOKEN")
                .ok()
                .filter(|t| t.len() >= 32),
        }
    }
}

/// A usable public URL: an absolute http(s) URL, without the trailing slash that would
/// double up in `<url>/h/<token>`. Anything else is ignored, keeping local links.
fn public_url(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim().trim_end_matches('/');
    let valid = trimmed.starts_with("https://") || trimmed.starts_with("http://");
    if !valid {
        tracing::warn!(
            value = trimmed,
            "AIBROWSE_PUBLIC_URL must start with https:// or http://; ignored"
        );
    }
    valid.then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_token_needs_equal_length_and_bytes() {
        assert!(same_token("abcd", "abcd"));
        assert!(!same_token("abcd", "abce"));
        assert!(!same_token("abcd", "abc"));
        assert!(!same_token("abcd", ""));
    }

    #[test]
    fn public_url_is_absolute_without_trailing_slash() {
        let url = |v: &str| public_url(Some(v.to_owned()));
        assert_eq!(
            url("https://b.example.com/").as_deref(),
            Some("https://b.example.com")
        );
        assert_eq!(
            url("https://x.org/aibrowse").as_deref(),
            Some("https://x.org/aibrowse")
        );
        assert_eq!(url("b.example.com"), None);
        assert_eq!(public_url(None), None);
    }
}
