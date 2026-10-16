//! How aibrowse talks to the Chrome it launches. On macOS and Linux, CDP runs over a
//! pipe on file descriptors 3 and 4 (`--remote-debugging-pipe`): nothing listens on a
//! port, so a page has no debugging endpoint to discover. Windows has no such inherited
//! descriptors for a non-CRT parent, so there Chrome opens its DevTools port on
//! localhost (`--remote-debugging-port=0`, a free port it picks and writes to
//! `DevToolsActivePort` in the profile) and aibrowse attaches over its WebSocket. That
//! port is reachable from the machine itself only, but it exists: a page probing
//! localhost could find it, which the pipe rules out.

use std::path::Path;
use std::time::Duration;

use aibrowse_cdp::Connection;
use serde_json::json;
use tokio::process::Child;

use crate::error::{BrowserError, Result};

/// The switch that names the transport to Chrome.
#[cfg(unix)]
pub(crate) const SWITCH: &str = "--remote-debugging-pipe";
#[cfg(windows)]
pub(crate) const SWITCH: &str = "--remote-debugging-port=0";

/// How long Chrome may take to answer its first call.
const READY: Duration = Duration::from_secs(20);

#[cfg(unix)]
use command_fds::{CommandFdExt, FdMapping};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use tokio::net::unix::pipe;
#[cfg(unix)]
use tokio::process::Command;

/// Spawns Chrome with a CDP pipe on fds 3/4 and waits until it answers.
#[cfg(unix)]
pub(crate) async fn spawn(chrome: &Path, args: Vec<String>) -> Result<(Child, Connection)> {
    // Chrome reads commands from fd 3 and writes responses to fd 4.
    let (to_chrome_read, to_chrome_write) = std::io::pipe()?;
    let (from_chrome_read, from_chrome_write) = std::io::pipe()?;

    let mut command = Command::new(chrome);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .fd_mappings(vec![
            FdMapping {
                parent_fd: to_chrome_read.into(),
                child_fd: 3,
            },
            FdMapping {
                parent_fd: from_chrome_write.into(),
                child_fd: 4,
            },
        ])
        .map_err(|e| BrowserError::Launch(e.to_string()))?;
    let child = command
        .spawn()
        .map_err(|e| BrowserError::Launch(format!("{}: {e}", chrome.display())))?;
    // Dropping the command closes our copies of the child's pipe ends; otherwise we'd
    // never observe EOF when Chrome exits.
    drop(command);

    let writer = pipe::Sender::from_owned_fd(to_chrome_write.into())?;
    let reader = pipe::Receiver::from_owned_fd(from_chrome_read.into())?;
    let conn = Connection::new(reader, writer);
    // The first call doubles as a readiness check for the pipe.
    conn.browser()
        .call_with_timeout("Browser.getVersion", json!({}), READY)
        .await?;
    Ok((child, conn))
}

/// Spawns Chrome with its DevTools port on localhost and attaches to it once it says
/// which port it took (`DevToolsActivePort` in the user data directory, written as
/// Chrome starts listening: the port, then the browser target's path).
#[cfg(windows)]
pub(crate) async fn spawn(chrome: &Path, args: Vec<String>) -> Result<(Child, Connection)> {
    let user_data_dir = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--user-data-dir="))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            BrowserError::Launch("no --user-data-dir to find the DevTools port in".into())
        })?;
    let port_file = user_data_dir.join("DevToolsActivePort");
    // A file left by an earlier run names a port nothing listens on any more.
    let _ = std::fs::remove_file(&port_file);
    let child = tokio::process::Command::new(chrome)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| BrowserError::Launch(format!("{}: {e}", chrome.display())))?;
    let deadline = tokio::time::Instant::now() + READY;
    let url = loop {
        if let Some(url) = std::fs::read_to_string(&port_file)
            .ok()
            .and_then(|text| devtools_url(&text))
        {
            break url;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::Launch(format!(
                "Chrome didn't open its DevTools port within {}s",
                READY.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let conn = Connection::connect(&url).await?;
    conn.browser()
        .call_with_timeout("Browser.getVersion", json!({}), READY)
        .await?;
    Ok((child, conn))
}

/// The browser's WebSocket address from `DevToolsActivePort`'s text ("port\n/path"),
/// once both lines are there.
#[cfg_attr(not(windows), allow(dead_code))]
fn devtools_url(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    path.starts_with('/')
        .then(|| format!("ws://127.0.0.1:{port}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_devtools_url_comes_from_both_lines() {
        assert_eq!(
            devtools_url("53712\n/devtools/browser/0d1e\n").as_deref(),
            Some("ws://127.0.0.1:53712/devtools/browser/0d1e")
        );
        // Still being written: no path yet.
        assert_eq!(devtools_url("53712\n"), None);
        assert_eq!(devtools_url(""), None);
    }
}
