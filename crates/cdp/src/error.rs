use std::time::Duration;

pub type Result<T, E = CdpError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    /// The browser answered the call with a protocol error.
    #[error("{method} failed ({code}): {message}")]
    Protocol {
        method: String,
        code: i64,
        message: String,
    },
    #[error("{method} timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    /// The browser went away (crashed, was closed, or the pipe broke).
    #[error("connection to the browser is closed")]
    Closed,
    /// A remote browser's DevTools endpoint couldn't be reached.
    #[error("could not connect to the browser at {url}: {reason}")]
    Connect { url: String, reason: String },
    #[error("failed to (de)serialize {method}: {source}")]
    Serde {
        method: String,
        #[source]
        source: serde_json::Error,
    },
}
