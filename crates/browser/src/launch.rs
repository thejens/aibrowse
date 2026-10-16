use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use aibrowse_cdp::{Connection, Session};
use aibrowse_network::Catalog;
use command_fds::{CommandFdExt, FdMapping};
use serde::Deserialize;
use serde_json::json;
use tokio::net::unix::pipe;
use tokio::process::{Child, Command};

use crate::calibrate::{UserAgentIdentity, user_agent_identity};
use crate::downloads::Downloads;
use crate::early_identity;
use crate::error::{BrowserError, Result};
use crate::locate::find_chrome;
use crate::platform;
use crate::profile::Profile;
use crate::profile_prefs;
use crate::tab::{Hand, Tab};
use crate::targets::{SharedTargets, watch_targets};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub profile: String,
    pub headless: bool,
    pub window_width: u32,
    pub window_height: u32,
    pub chrome_path: Option<PathBuf>,
    /// The Site Knowledge Base file: API structure learned in earlier sessions is loaded
    /// from it and this session's is saved to it. `None` keeps API knowledge in memory.
    pub knowledge_base: Option<PathBuf>,
    /// Silence the browser's sound output (`--mute-audio`), for test and eval runs that
    /// open sites with autoplaying video. Pages still get working audio APIs. Defaults to
    /// `AIBROWSE_MUTE=1`.
    pub mute: bool,
    /// Drive a browser running elsewhere instead of launching one: its DevTools
    /// WebSocket (`ws://host:port/devtools/browser/<id>` from a Chrome started with
    /// `--remote-debugging-port`, or the `wss://` address a hosted browser hands out).
    /// The local profile still holds the persona, settings and knowledge; the remote
    /// browser keeps its own cookies and files, and outlives the session. Defaults to
    /// `AIBROWSE_CDP_URL`.
    pub remote: Option<String>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            profile: "default".into(),
            headless: false,
            window_width: 1440,
            window_height: 900,
            chrome_path: None,
            knowledge_base: None,
            mute: std::env::var("AIBROWSE_MUTE").is_ok_and(|v| v == "1"),
            remote: std::env::var("AIBROWSE_CDP_URL")
                .ok()
                .filter(|url| !url.trim().is_empty()),
        }
    }
}

/// A Chrome driven over CDP: one this process launched (over `--remote-debugging-pipe`),
/// or one running elsewhere, reached over its DevTools WebSocket.
pub struct Browser {
    /// The Chrome process this browser launched; `None` for a remote one.
    child: Option<Child>,
    conn: Connection,
    profile: Profile,
    /// Set when headless: the regular-Chrome identity each page must present.
    identity: Option<UserAgentIdentity>,
    options: LaunchOptions,
    pub(crate) targets: SharedTargets,
    /// API endpoints seen in any tab of this browser.
    catalog: Catalog,
    downloads: Downloads,
    /// The one hand all its tabs send input with (see `tab::Hand`).
    hand: Hand,
}

impl Browser {
    pub async fn launch(options: LaunchOptions) -> Result<Self> {
        if let Some(url) = options.remote.clone() {
            return Self::connect(&url, options).await;
        }
        let profile = Profile::open(&options.profile)?;
        let chrome = match &options.chrome_path {
            Some(path) => path.clone(),
            None => find_chrome()?,
        };
        let identity = if options.headless {
            let mut identity = user_agent_identity(&chrome).await?;
            identity.media = profile.display.media_features();
            Some(identity)
        } else {
            None
        };
        profile.ensure_not_in_use()?;
        if let Some(egress) = &profile.egress {
            egress.ensure_reachable().await?;
        }
        profile_prefs::prepare(&profile.user_data_dir(), &profile.downloads_dir())?;
        let downloads = Downloads::prepare(profile.downloads_dir())?;
        let (child, conn) =
            spawn(&chrome, chrome_args(&profile, &options, identity.as_ref())).await?;
        let targets = watch_targets(&conn).await?;
        if let Some(identity) = &identity {
            early_identity::present_identity(&conn, identity.clone()).await?;
        }
        tracing::info!(profile = %profile.name, headless = options.headless, "browser launched");
        let catalog = match &options.knowledge_base {
            Some(path) => Catalog::persistent(path.clone()),
            None => Catalog::default(),
        };
        let hand = Hand::new(conn.browser());
        Ok(Self {
            child: Some(child),
            conn,
            profile,
            identity,
            options,
            targets,
            catalog,
            downloads,
            hand,
        })
    }

    /// Attaches to a browser running elsewhere (see `LaunchOptions::remote`). Nothing is
    /// launched or locked locally: the remote browser presents its own identity, keeps
    /// its own cookies, and is left running when this session closes.
    async fn connect(url: &str, options: LaunchOptions) -> Result<Self> {
        let profile = Profile::open(&options.profile)?;
        let downloads = Downloads::prepare(profile.downloads_dir())?;
        let conn = Connection::connect(url).await?;
        let targets = watch_targets(&conn).await?;
        tracing::info!(profile = %profile.name, "attached to a remote browser");
        let catalog = match &options.knowledge_base {
            Some(path) => Catalog::persistent(path.clone()),
            None => Catalog::default(),
        };
        let hand = Hand::new(conn.browser());
        Ok(Self {
            child: None,
            conn,
            profile,
            identity: None,
            options,
            targets,
            catalog,
            downloads,
            hand,
        })
    }

    /// Whether this browser runs elsewhere, attached over its DevTools WebSocket.
    pub fn is_remote(&self) -> bool {
        self.child.is_none()
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The knowledge base this browser learns into, if any.
    pub fn knowledge_base(&self) -> Option<&Path> {
        self.options.knowledge_base.as_deref()
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Files the pages of this session downloaded.
    pub fn downloads(&self) -> &Downloads {
        &self.downloads
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Opens the session's working tab: a fresh foreground tab in a window placed on the
    /// profile's display.
    ///
    /// Any other tabs are closed. On relaunch Chrome restores the previous session's tabs
    /// in the background; attaching to one of those would leave the agent working in a
    /// hidden tab (`visibilityState: "hidden"`, `outerWidth: 0`), which behaves and looks
    /// unlike a person's browser.
    pub async fn open_tab(&self) -> Result<Tab> {
        let root = self.conn.browser();
        let created: CreatedTarget = root
            .call_as("Target.createTarget", json!({ "url": "about:blank" }))
            .await?;
        let targets: TargetList = root.call_as("Target.getTargets", json!({})).await?;
        for stale in targets
            .target_infos
            .iter()
            .filter(|t| t.kind == "page" && t.target_id != created.target_id)
        {
            root.call("Target.closeTarget", json!({ "targetId": stale.target_id }))
                .await?;
        }
        self.place_window(&root, &created.target_id).await?;
        self.attach_target(&created.target_id).await
    }

    /// Puts the window where the profile's display says it is. Headful macOS ignores
    /// `--window-position` and reopens windows on whichever monitor it likes, which
    /// would make the screen a page sees change between sessions.
    async fn place_window(&self, root: &Session, target_id: &str) -> Result<()> {
        let window = root
            .call(
                "Browser.getWindowForTarget",
                json!({ "targetId": target_id }),
            )
            .await?;
        let bounds = WindowBounds::for_display(&self.profile.display, &self.options);
        root.call(
            "Browser.setWindowBounds",
            json!({
                "windowId": window["windowId"],
                "bounds": {
                    "left": bounds.left,
                    "top": bounds.top,
                    "width": bounds.width,
                    "height": bounds.height,
                    "windowState": "normal",
                },
            }),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn attach_target(&self, target_id: &str) -> Result<Tab> {
        let attached: Attached = self
            .conn
            .browser()
            .call_as(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .await?;
        Tab::attach(
            self.conn.session(attached.session_id),
            target_id.to_owned(),
            self.profile.persona.clone(),
            self.identity.as_ref(),
            self.catalog.clone(),
            self.downloads.clone(),
            self.hand.clone(),
        )
        .await
    }

    /// The user agent pages see: the calibrated identity when headless (Chrome's own
    /// would say "HeadlessChrome"), otherwise Chrome's own.
    pub async fn user_agent(&self) -> Result<String> {
        if let Some(identity) = &self.identity {
            return Ok(identity.user_agent.clone());
        }
        let version = self
            .conn
            .browser()
            .call("Browser.getVersion", json!({}))
            .await?;
        Ok(version["userAgent"].as_str().unwrap_or_default().to_owned())
    }

    /// Asks Chrome to exit cleanly (flushing cookies and storage to the profile), and
    /// kills it if it doesn't within a few seconds. What the session learned about sites'
    /// APIs is saved first.
    pub async fn close(self) -> Result<()> {
        self.close_within(CLOSE_GRACE).await
    }

    /// [`Browser::close`], waiting up to `grace` after `Browser.close` for Chrome to exit
    /// before killing it. Headless Chrome can take far longer than headful to tear down
    /// (a known issue); a caller that no agent is waiting on can afford to
    /// give it that time.
    pub async fn close_within(mut self, grace: Duration) -> Result<()> {
        self.catalog.flush().await;
        // A remote browser outlives the session: its owner (a hosted service, another
        // process) decides when it ends.
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let started = std::time::Instant::now();
        // Browser.close, not SIGTERM: measured on Chrome 154, SIGTERM exits in ~0.1s but
        // loses localStorage written just before; Browser.close keeps it even when the
        // process is killed later in its teardown. Closing the tabs first makes no
        // reliable difference.
        let _ = self
            .conn
            .browser()
            .call_with_timeout("Browser.close", json!({}), CLOSE_GRACE)
            .await;
        match tokio::time::timeout(grace, child.wait()).await {
            Ok(status) => tracing::info!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                status = ?status.ok(),
                "Chrome exited"
            ),
            Err(_) => {
                tracing::warn!(
                    grace_ms = grace.as_millis() as u64,
                    "Chrome did not exit after Browser.close; killing it"
                );
                child.kill().await?;
            }
        }
        Ok(())
    }
}

/// How long [`Browser::close`] waits for Chrome to exit by itself.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

fn chrome_args(
    profile: &Profile,
    options: &LaunchOptions,
    identity: Option<&UserAgentIdentity>,
) -> Vec<String> {
    let window = WindowBounds::for_display(&profile.display, options);
    // Deliberately absent: --enable-automation (sets navigator.webdriver and shows the
    // "controlled by automated software" bar) and --remote-debugging-port (an open,
    // discoverable debugging port). Keep this list minimal: every non-default switch is a
    // potential behavioral difference from a normal user's browser.
    let mut args = vec![
        "--remote-debugging-pipe".to_owned(),
        format!("--user-data-dir={}", profile.user_data_dir().display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        // When the user covers the window, Chrome would otherwise mark the page hidden
        // and stop rendering, which hangs screenshots and flips visibilityState mid-task.
        // A user actively working in a window keeps it visible, so this is also the
        // consistent state to present.
        "--disable-backgrounding-occluded-windows".to_owned(),
        // Any remote-debugging transport (pipe included) turns on Blink's
        // AutomationControlled feature, which sets navigator.webdriver = true. This browser
        // acts for its owner in their own session, so it presents as their browser.
        "--disable-blink-features=AutomationControlled".to_owned(),
        // WebMCP (`document.modelContext`, design.md §5.10) is on by default in Chrome for
        // Testing 154 but off in distribution Chromium builds of the same version (Debian's,
        // which Linux servers run). Naming it keeps both browsers the same; where it is
        // already on this changes nothing.
        "--enable-features=WebMCP".to_owned(),
        // Initial placement; `Browser::place_window` enforces it once the window exists.
        format!("--window-size={},{}", window.width, window.height),
        format!("--window-position={},{}", window.left, window.top),
    ];
    args.extend(platform::switches(options.headless));
    if options.mute {
        args.push("--mute-audio".to_owned());
    }
    if let Some(egress) = &profile.egress {
        args.extend(egress.chrome_switches());
    }
    if options.headless {
        args.push("--headless".to_owned());
        args.push(profile.display.screen_info_switch());
    }
    // The switch fixes navigator.userAgent everywhere, workers included; pages
    // additionally get the full client-hint metadata in Tab::attach.
    if let Some(identity) = identity {
        args.push(format!("--user-agent={}", identity.user_agent));
    }
    args.push("about:blank".to_owned());
    args
}

/// Spawns Chrome with a CDP pipe on fds 3/4 and waits until it answers.
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
        .call_with_timeout("Browser.getVersion", json!({}), Duration::from_secs(20))
        .await?;
    Ok((child, conn))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetList {
    target_infos: Vec<TargetInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetInfo {
    target_id: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedTarget {
    target_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attached {
    session_id: String,
}

/// Where the browser window sits, in screen coordinates (CSS pixels).
struct WindowBounds {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

impl WindowBounds {
    /// The window must fit on, and sit within, the screen it claims to be on: below the
    /// menu bar and horizontally centered.
    fn for_display(display: &crate::identity::Display, options: &LaunchOptions) -> Self {
        let width = options.window_width.min(display.width);
        let height = options
            .window_height
            .min(display.height.saturating_sub(display.work_area_top));
        Self {
            left: (display.width - width) / 2,
            top: display.work_area_top,
            width,
            height,
        }
    }
}
