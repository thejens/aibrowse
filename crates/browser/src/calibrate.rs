//! Measuring the user-agent identity a headless browser must present.
//!
//! Headless Chrome is the same browser as headful Chrome, except that its user agent says
//! "HeadlessChrome". Overriding only the user agent string is not enough: Chrome then
//! drops the detailed client hints (`navigator.userAgentData.getHighEntropyValues()`
//! returns an empty `fullVersionList`), which real Chrome never does. So we measure the
//! real client hints once per Chrome version, from a headless instance with no override
//! (its client hints are identical to headful), and replay them alongside the corrected
//! user agent on every page.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aibrowse_cdp::{CdpError, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::{BrowserError, Result};
use crate::launch::spawn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAgentIdentity {
    pub user_agent: String,
    /// `navigator.platform`, e.g. "MacIntel".
    pub platform: String,
    /// CDP `Emulation.UserAgentMetadata`.
    pub metadata: Value,
    /// CSS media features of the profile's display (`Display::media_features`). Set per
    /// launch and never cached: the display belongs to the profile, not to Chrome.
    #[serde(skip)]
    pub media: Value,
}

impl UserAgentIdentity {
    /// Whether the metadata has every hint pages can ask for. Identities cached before a
    /// hint was added lack it, and are measured again rather than replayed with a gap.
    fn is_complete(&self) -> bool {
        self.metadata["fullVersion"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    }

    /// Must run before the page navigates; it applies to every later document in it.
    ///
    /// Both commands are sent before either answer is awaited: for a target paused at
    /// start (frames.rs, early_identity.rs) the caller queues the resume right after
    /// them, and the renderer answers none of them until it is resumed.
    pub(crate) async fn apply(&self, session: &Session) -> Result<()> {
        let user_agent = session.call(
            "Emulation.setUserAgentOverride",
            json!({
                "userAgent": self.user_agent,
                "platform": self.platform,
                "userAgentMetadata": self.metadata,
            }),
        );
        let media = async {
            if self.media.as_array().is_some_and(|m| !m.is_empty()) {
                session
                    .call(
                        "Emulation.setEmulatedMedia",
                        json!({ "media": "", "features": self.media }),
                    )
                    .await?;
            }
            Ok(())
        };
        let (user_agent, media) = tokio::join!(user_agent, media);
        user_agent?;
        media
    }
}

/// The identity for this Chrome binary, measured on first use and cached per version.
pub(crate) async fn user_agent_identity(chrome: &Path) -> Result<UserAgentIdentity> {
    let version = chrome_version(chrome)?;
    let cache = cache_path(&version)?;
    if let Ok(bytes) = std::fs::read(&cache)
        && let Ok(identity) = serde_json::from_slice::<UserAgentIdentity>(&bytes)
        && identity.is_complete()
    {
        return Ok(identity);
    }
    tracing::info!(%version, "calibrating headless user-agent identity");
    let identity = measure(chrome).await?;
    std::fs::create_dir_all(cache.parent().expect("cache path has a parent"))?;
    std::fs::write(
        &cache,
        serde_json::to_vec_pretty(&identity).map_err(|e| BrowserError::Launch(e.to_string()))?,
    )?;
    Ok(identity)
}

fn chrome_version(chrome: &Path) -> Result<String> {
    let output = std::process::Command::new(chrome)
        .arg("--version")
        .output()
        .map_err(|e| BrowserError::Launch(format!("{} --version: {e}", chrome.display())))?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find(|word| {
            word.split('.').count() == 4 && word.split('.').all(|p| p.parse::<u32>().is_ok())
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            BrowserError::Launch(format!("unrecognized Chrome version output: {text:?}"))
        })
}

fn cache_path(version: &str) -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| BrowserError::Launch("cannot determine home directory".into()))?;
    Ok(home.join(format!(".aibrowse/chrome/identity-{version}.json")))
}

/// Read in an isolated world, so the probe page's own scripts can't interfere.
const PROBE: &str = r#"(async () => {
  const hints = await navigator.userAgentData.getHighEntropyValues([
    'architecture', 'bitness', 'formFactors', 'fullVersionList', 'model', 'platformVersion',
    'uaFullVersion', 'wow64',
  ]);
  return { userAgent: navigator.userAgent, platform: navigator.platform, hints };
})()"#;

async fn measure(chrome: &Path) -> Result<UserAgentIdentity> {
    let scratch = std::env::temp_dir().join(format!("aibrowse-calibrate-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let result = measure_in(chrome, &scratch).await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

async fn measure_in(chrome: &Path, scratch: &Path) -> Result<UserAgentIdentity> {
    // userAgentData requires a secure context; http://localhost qualifies.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf).await;
            let body = "<!doctype html><title>calibrate</title>";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });

    let mut args = vec![
        "--remote-debugging-pipe".to_owned(),
        format!("--user-data-dir={}", scratch.display()),
        "--headless".to_owned(),
        "--no-first-run".to_owned(),
    ];
    args.extend(crate::platform::switches(true));
    args.push("about:blank".to_owned());
    let (mut child, conn) = spawn(chrome, args).await?;
    let outcome = async {
        let root = conn.browser();
        let created = root
            .call(
                "Target.createTarget",
                json!({ "url": format!("http://localhost:{port}/") }),
            )
            .await?;
        let attached = root
            .call(
                "Target.attachToTarget",
                json!({ "targetId": created["targetId"], "flatten": true }),
            )
            .await?;
        let session = conn.session(attached["sessionId"].as_str().unwrap_or_default());
        let tree = session.call("Page.getFrameTree", json!({})).await?;
        let frame_id = tree["frameTree"]["frame"]["id"].clone();
        // The page is tiny; poll briefly until it has committed and the API is usable.
        // A world created before the commit dies with about:blank, so an evaluation
        // failing for a lost context just means "not yet".
        for _ in 0..50 {
            let world = session
                .call(
                    "Page.createIsolatedWorld",
                    json!({ "frameId": frame_id, "worldName": "aibrowse" }),
                )
                .await?;
            let response = session
                .call(
                    "Runtime.evaluate",
                    json!({
                        "expression": PROBE,
                        "contextId": world["executionContextId"],
                        "returnByValue": true,
                        "awaitPromise": true,
                    }),
                )
                .await;
            let response = match response {
                Ok(response) => response,
                Err(CdpError::Protocol { message, .. })
                    if message.contains("Cannot find context") =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            let value = &response["result"]["value"];
            if value["userAgent"].is_string() && response.get("exceptionDetails").is_none() {
                return Ok(identity_from(value));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(BrowserError::Launch(
            "calibration page never became ready".into(),
        ))
    }
    .await;
    let _ = conn.browser().call("Browser.close", json!({})).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    server.abort();
    outcome
}

fn identity_from(probe: &Value) -> UserAgentIdentity {
    let hints = &probe["hints"];
    UserAgentIdentity {
        user_agent: probe["userAgent"]
            .as_str()
            .unwrap_or_default()
            .replace("HeadlessChrome/", "Chrome/"),
        platform: probe["platform"].as_str().unwrap_or_default().to_owned(),
        media: Value::Null,
        metadata: json!({
            "brands": hints["brands"],
            "fullVersionList": hints["fullVersionList"],
            // The deprecated `uaFullVersion` hint, which fingerprinters (CreepJS) still
            // read; without it headless reports an empty string where Chrome has a version.
            "fullVersion": hints["uaFullVersion"],
            "platform": hints["platform"],
            "platformVersion": hints["platformVersion"],
            "architecture": hints["architecture"],
            "model": hints["model"],
            "mobile": hints["mobile"],
            "bitness": hints["bitness"],
            "wow64": hints["wow64"],
            "formFactors": hints["formFactors"],
        }),
    }
}
