use aibrowse_cdp::CdpError;

pub type Result<T, E = BrowserError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error(transparent)]
    Cdp(#[from] CdpError),
    #[error("could not find a Chrome binary: {0}")]
    ChromeNotFound(String),
    #[error("failed to launch Chrome: {0}")]
    Launch(String),
    #[error("profile error: {0}")]
    Profile(String),
    /// The target element is gone, invisible, or covered; the message says which.
    #[error("{0}")]
    Element(String),
    #[error("unknown key or chord: {0}")]
    UnknownKey(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
