//! Where a build of aibrowse adds to the server's life without changing it: a store that
//! keeps profiles beyond this machine (saved when the browser closes on one, restored
//! before a launch on one this machine doesn't have). The public binary has none; a
//! hosted build registers one with `set_profile_store` before `run`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::OnceLock;

/// A future a hook returns.
pub type HookFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Keeps profiles somewhere besides this machine's disk.
pub trait ProfileStore: Send + Sync + 'static {
    /// Before a launch on `profile`, before its directory is opened: brings the profile
    /// here when this machine doesn't have it, and waits for a save still running.
    fn before_launch<'a>(&'a self, profile: &'a str) -> HookFuture<'a>;
    /// The browser closed on `profile` (its files at `root`): may save it, in the
    /// background, holding off the next launch until done.
    fn after_close<'a>(&'a self, profile: &'a str, root: PathBuf) -> HookFuture<'a>;
    /// The server is about to exit after closing `profile`: saves it and waits.
    fn before_exit<'a>(&'a self, profile: &'a str, root: PathBuf) -> HookFuture<'a>;
}

static PROFILE_STORE: OnceLock<Box<dyn ProfileStore>> = OnceLock::new();

/// Registers the profile store, once, before `run`. Returns false when one already was.
pub fn set_profile_store(store: impl ProfileStore) -> bool {
    PROFILE_STORE.set(Box::new(store)).is_ok()
}

pub(crate) async fn before_launch(profile: &str) {
    if let Some(store) = PROFILE_STORE.get() {
        store.before_launch(profile).await;
    }
}

pub(crate) async fn after_close(profile: &str, root: PathBuf) {
    if let Some(store) = PROFILE_STORE.get() {
        store.after_close(profile, root).await;
    }
}

pub(crate) async fn before_exit(profile: &str, root: PathBuf) {
    if let Some(store) = PROFILE_STORE.get() {
        store.before_exit(profile, root).await;
    }
}
