//! Minimal Chrome DevTools Protocol transport.
//!
//! We deliberately own the transport instead of using a high-level driver: aibrowse must
//! control exactly which CDP domains are enabled on each target (enabling `Runtime` on a
//! page, for example, is observable by page scripts). Generated protocol *types* come from
//! `chromiumoxide_cdp`; for large, fast-moving payloads (AX trees, DOM snapshots) callers
//! should prefer [`Session::call_as`] with their own lenient structs, because strict
//! generated enums break whenever Chrome adds a new variant.

mod connection;
mod error;
mod session;

pub use chromiumoxide_cdp::cdp;
pub use chromiumoxide_types::{Command, Method};
pub use connection::{Connection, Event};
pub use error::{CdpError, Result};
pub use session::{Session, SessionEvents};
