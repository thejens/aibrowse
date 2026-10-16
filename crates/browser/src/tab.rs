use std::sync::Arc;
use std::time::{Duration, Instant};

use aibrowse_cdp::Session;
use aibrowse_input::{Pace, Persona, Point, Rect};
use aibrowse_network::{Catalog, NetworkLog};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::calibrate::UserAgentIdentity;
use crate::downloads::Downloads;
use crate::error::{BrowserError, Result};
use crate::frames::{ElementRef, FrameSetup, SharedChildren, watch_frames};
use crate::isolated::{IsolatedWorld, MUTATION_COUNTER};
use crate::live_regions::announcement_note;
use crate::settle::{Dialog, SharedActivity, lock, spawn_tracker};
use crate::webmcp::{SharedTools, WebMcpTool, lock_tools, watch_webmcp};

/// How long the DOM and network must both be quiet for the page to count as settled.
const QUIET: Duration = Duration::from_millis(350);
/// How long a loaded page gets to go quiet before the action is reported as unsettled.
const QUIET_BUDGET: Duration = Duration::from_secs(4);
const POLL: Duration = Duration::from_millis(100);
/// Effects of an action (a request, a re-render) need a moment to begin; checking for
/// quiet immediately would declare victory before anything happened.
const GRACE: Duration = Duration::from_millis(120);
const ACTION_SETTLE_TIMEOUT: Duration = Duration::from_secs(8);
const NAVIGATION_SETTLE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an action waits for a download it started to finish. Small files make it,
/// so the agent can use them right away; larger ones are reported as in progress.
const DOWNLOAD_WAIT: Duration = Duration::from_secs(5);

/// One attached page target and the input state that belongs to it.
pub struct Tab {
    pub(crate) session: Session,
    target_id: String,
    pub(crate) main_frame_id: String,
    pub(crate) activity: SharedActivity,
    /// Out-of-process iframes attached under this tab.
    pub(crate) frames: SharedChildren,
    /// Requests the page makes, for API discovery.
    network: NetworkLog,
    /// Tools the page registers for agents (WebMCP).
    webmcp: SharedTools,
    /// The browser's downloads, shared by all tabs.
    downloads: Downloads,
    world: Option<IsolatedWorld>,
    pub(crate) persona: Persona,
    pub(crate) pace: Pace,
    /// Where our virtual cursor is. Every pointer action starts from here, so the page
    /// sees one continuous cursor rather than events appearing out of nowhere.
    pub(crate) cursor: Point,
    /// Modifier keys held down right now (`Modifiers` flags), reported with every
    /// mouse event as a real keyboard state would be.
    pub(crate) held: u32,
    pub(crate) rng: StdRng,
    /// The browser's one hand, shared by all its tabs (see `take_hand`).
    hand: Hand,
    /// The bridge to the page's `navigator.modelContext` polyfill, if it has one, and the
    /// navigation it belongs to (webmcp_polyfill.rs).
    pub(crate) polyfill_bridge: Option<(u64, String)>,
    /// The frame of the field `fill` last typed into, to read it back after Enter
    /// (`submit_field`).
    pub(crate) last_field: Option<(Session, String)>,
}

/// The browser's one hand: whoever holds it sends input. A person has one cursor and
/// one keyboard, used in the tab in front; tabs worked side by side (a fan-out, a
/// survey) take turns at it, and only while they send input: the page loading after a
/// click is waited for with the hand free, as a person switches to the next tab then.
#[derive(Clone)]
pub(crate) struct Hand {
    held: Arc<tokio::sync::Mutex<()>>,
    /// The browser's own session, for bringing a tab in front: through the page's
    /// session, a tab still loading in the background answers only when it can.
    browser: Session,
}

impl Hand {
    pub(crate) fn new(browser: Session) -> Self {
        Self {
            held: Arc::default(),
            browser,
        }
    }
}

/// What an action did, so the agent can decide its next step without re-observing.
#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub action: String,
    /// Network and DOM went quiet before the timeout.
    pub settled: bool,
    /// A new document was loaded in the main frame.
    pub navigated: bool,
    pub url_before: String,
    pub url_after: String,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// What a page tool (WebMCP) returned. Written by the page: untrusted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub content_width: f64,
    pub content_height: f64,
    /// Device pixels per CSS pixel (2 on Retina).
    #[serde(default = "one")]
    pub device_pixel_ratio: f64,
}

fn one() -> f64 {
    1.0
}

pub(crate) struct ActionStart {
    started: Instant,
    url: String,
    navigation: u64,
    /// The network log's id for this action, attributing the requests it causes.
    network_action: u64,
    /// Downloads that begin from here on were started by this action.
    downloads: usize,
}

impl Tab {
    pub(crate) async fn attach(
        session: Session,
        target_id: String,
        persona: Persona,
        identity: Option<&UserAgentIdentity>,
        catalog: Catalog,
        downloads: Downloads,
        hand: Hand,
    ) -> Result<Self> {
        // Subscribe before enabling domains so no early event is missed.
        let events = session.events();
        let webmcp_events = session.events();
        let network = NetworkLog::spawn(session.events(), session.clone(), catalog);
        // Page, Network and DOM are invisible to page scripts. Runtime is deliberately
        // never enabled (see IsolatedWorld).
        for domain in ["Page.enable", "Network.enable", "DOM.enable"] {
            session.call(domain, json!({})).await?;
        }
        // File choosers come to us instead of opening a native dialog (see FileChooser).
        session
            .call(
                "Page.setInterceptFileChooserDialog",
                json!({ "enabled": true }),
            )
            .await?;
        // Headless exactly when an identity is set: there is no window for passkey dialogs.
        if let Some(identity) = identity {
            identity.apply(&session).await?;
            crate::passkeys::decline(&session).await?;
        }
        // A person using the browser has the page focused. Without this, document.hasFocus()
        // is false whenever the window isn't frontmost (always, when headless), and
        // focus-dependent UI (autocomplete dropdowns, keyboard handlers) misbehaves. This
        // is engine-level emulation, not a JavaScript override, so pages can't detect it.
        session
            .call(
                "Emulation.setFocusEmulationEnabled",
                json!({ "enabled": true }),
            )
            .await?;
        let tree: FrameTree = session.call_as("Page.getFrameTree", json!({})).await?;
        let main_frame_id = tree.frame_tree.frame.id;
        let webmcp = watch_webmcp(&session, webmcp_events, main_frame_id.clone()).await;
        let activity = spawn_tracker(events, session.clone(), main_frame_id.clone());
        let frames = SharedChildren::default();
        watch_frames(
            &session,
            FrameSetup {
                children: frames.clone(),
                activity: activity.clone(),
                identity: identity.cloned(),
            },
        )
        .await?;
        lock(&activity).url = format!(
            "{}{}",
            tree.frame_tree.frame.url, tree.frame_tree.frame.url_fragment
        );

        let mut rng = StdRng::seed_from_u64(rand::random());
        let mut tab = Self {
            session,
            target_id,
            main_frame_id,
            activity,
            frames,
            network,
            webmcp,
            downloads,
            world: None,
            persona,
            pace: Pace::Human,
            cursor: Point::new(0.0, 0.0),
            held: 0,
            rng: StdRng::seed_from_u64(0),
            hand,
            polyfill_bridge: None,
            last_field: None,
        };
        // The cursor starts somewhere plausible in the middle of the window, as if the
        // user's hand were resting there; nothing is dispatched until the first action.
        let viewport = tab.viewport().await?;
        tab.cursor = Point::new(
            viewport.width * rng.random_range(0.35..0.65),
            viewport.height * rng.random_range(0.35..0.65),
        );
        tab.rng = rng;
        Ok(tab)
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Waits for the browser's hand and brings this tab in front, where the input goes:
    /// no page gets input while hidden. Held for the input itself, released before the
    /// page is waited on.
    pub(crate) async fn take_hand(&self) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        let held = Arc::clone(&self.hand.held).lock_owned().await;
        self.hand
            .browser
            .call(
                "Target.activateTarget",
                json!({ "targetId": self.target_id }),
            )
            .await?;
        Ok(held)
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub fn pace(&self) -> Pace {
        self.pace
    }

    pub fn set_pace(&mut self, pace: Pace) {
        self.pace = pace;
    }

    pub fn network(&self) -> &NetworkLog {
        &self.network
    }

    /// Tools the current page offers agents (WebMCP), newest registration last.
    pub fn webmcp_tools(&self) -> Vec<WebMcpTool> {
        lock_tools(&self.webmcp).clone()
    }

    pub fn main_frame_id(&self) -> &str {
        &self.main_frame_id
    }

    pub fn url(&self) -> String {
        lock(&self.activity).url.clone()
    }

    /// The dialog currently blocking the page, if any.
    pub fn dialog(&self) -> Option<Dialog> {
        lock(&self.activity).dialog.clone()
    }

    /// Accepts or dismisses the open dialog; `prompt_text` answers a prompt().
    pub async fn handle_dialog(
        &mut self,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<ActionResult> {
        if self.dialog().is_none() {
            return Err(BrowserError::Element("no dialog is open".into()));
        }
        let start = self.begin();
        let mut params = json!({ "accept": accept });
        if let Some(text) = prompt_text {
            params["promptText"] = json!(text);
        }
        self.session
            .call("Page.handleJavaScriptDialog", params)
            .await?;
        lock(&self.activity).dialog = None;
        let action = if accept {
            "accept dialog"
        } else {
            "dismiss dialog"
        };
        self.finish(action, start, ACTION_SETTLE_TIMEOUT, Vec::new())
            .await
    }

    pub async fn navigate(&mut self, url: &str) -> Result<ActionResult> {
        let start = self.begin();
        // The page it is on already is loaded again: to the same address with a
        // fragment ("…/edit#gid=0"), Chrome would only jump to the fragment, and a page
        // whose access was just granted kept showing "You need access".
        if self.url() == url {
            self.session
                .call("Page.reload", json!({ "ignoreCache": false }))
                .await?;
            lock(&self.activity).main_frame_loading = true;
            return self
                .finish("navigate", start, NAVIGATION_SETTLE_TIMEOUT, Vec::new())
                .await;
        }
        let response = self
            .session
            .call("Page.navigate", json!({ "url": url }))
            .await?;
        if let Some(error) = response["errorText"].as_str().filter(|e| !e.is_empty()) {
            return Err(BrowserError::Element(format!(
                "navigation to {url} failed: {error}"
            )));
        }
        lock(&self.activity).main_frame_loading = true;
        self.finish("navigate", start, NAVIGATION_SETTLE_TIMEOUT, Vec::new())
            .await
    }

    pub async fn back(&mut self) -> Result<ActionResult> {
        let start = self.begin();
        let history = self
            .session
            .call("Page.getNavigationHistory", json!({}))
            .await?;
        let current = history["currentIndex"].as_u64().unwrap_or(0) as usize;
        let Some(entry_id) = current
            .checked_sub(1)
            .and_then(|i| history["entries"][i]["id"].as_i64())
        else {
            return Err(BrowserError::Element(
                "there is no previous page in this tab's history".into(),
            ));
        };
        self.session
            .call(
                "Page.navigateToHistoryEntry",
                json!({ "entryId": entry_id }),
            )
            .await?;
        self.finish("back", start, NAVIGATION_SETTLE_TIMEOUT, Vec::new())
            .await
    }

    /// The element's visible box in viewport coordinates, or an error the agent can act
    /// on if the element is gone or has no box. A checkbox or radio button that can't
    /// be clicked where it is itself (no box, drawn invisible, or under another
    /// control's label) is where its label is (see `label_rect`).
    pub(crate) async fn element_rect(&self, element: ElementRef) -> Result<Rect> {
        if let Some(rect) = self.label_rect(element).await {
            return Ok(rect);
        }
        if let Some(rect) = self.box_rect(element).await? {
            return Ok(rect);
        }
        Err(BrowserError::Element(format!(
            "element {element} has no visible box (hidden, collapsed, or display:none)"
        )))
    }

    /// The largest visible quad of the element's own box, in viewport coordinates.
    pub(crate) async fn box_rect(&self, element: ElementRef) -> Result<Option<Rect>> {
        let session = self.session_for(element.target)?;
        let (dx, dy) = self.target_offset(element.target).await?;
        let response = session
            .call(
                "DOM.getContentQuads",
                json!({ "backendNodeId": element.backend }),
            )
            .await
            .map_err(|_| stale(element))?;
        let quads = response["quads"].as_array().cloned().unwrap_or_default();
        Ok(quads
            .iter()
            .filter_map(|quad| {
                let q: Vec<f64> = quad.as_array()?.iter().filter_map(|v| v.as_f64()).collect();
                (q.len() == 8).then(|| {
                    let xs = [q[0], q[2], q[4], q[6]];
                    let ys = [q[1], q[3], q[5], q[7]];
                    let (x0, x1) = (
                        xs.iter().copied().fold(f64::MAX, f64::min),
                        xs.iter().copied().fold(f64::MIN, f64::max),
                    );
                    let (y0, y1) = (
                        ys.iter().copied().fold(f64::MAX, f64::min),
                        ys.iter().copied().fold(f64::MIN, f64::max),
                    );
                    Rect::new(x0 + dx, y0 + dy, x1 - x0, y1 - y0)
                })
            })
            .filter(|r| r.width >= 1.0 && r.height >= 1.0)
            .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height))))
    }

    pub(crate) async fn world(&mut self) -> Result<&IsolatedWorld> {
        let navigation = lock(&self.activity).navigation_count;
        let stale_world = self
            .world
            .as_ref()
            .is_none_or(|w| !w.is_current(navigation));
        if stale_world {
            self.world =
                Some(IsolatedWorld::create(&self.session, &self.main_frame_id, navigation).await?);
        }
        Ok(self.world.as_ref().expect("world was just ensured"))
    }

    async fn mutation_count(&mut self) -> Option<i64> {
        let session = self.session.clone();
        let world = self.world().await.ok()?;
        match world.evaluate(&session, MUTATION_COUNTER).await {
            Ok(value) => value.as_i64(),
            Err(_) => {
                // The context can die between our navigation check and the call (a
                // navigation raced us); drop it so the next poll recreates it.
                self.world = None;
                None
            }
        }
    }

    pub(crate) fn begin(&self) -> ActionStart {
        let activity = lock(&self.activity);
        ActionStart {
            started: Instant::now(),
            url: activity.url.clone(),
            navigation: activity.navigation_count,
            network_action: self.network.begin_action(),
            downloads: self.downloads.mark(),
        }
    }

    pub(crate) async fn finish(
        &mut self,
        action: &str,
        start: ActionStart,
        timeout: Duration,
        notes: Vec<String>,
    ) -> Result<ActionResult> {
        let waiting = Instant::now();
        let settled = self.settle(timeout).await;
        tracing::debug!(
            action,
            settled,
            settle_ms = waiting.elapsed().as_millis() as u64,
            total_ms = start.started.elapsed().as_millis() as u64,
            "action finished"
        );
        let mut notes = notes;
        let announced = self.take_announcements().await;
        // A new document's live regions fill as it renders; that is the page, not news
        // about what the action did, and the outline shows it.
        let new_document = lock(&self.activity).navigation_count != start.navigation;
        if !new_document && !matches!(action, "navigate" | "back") && !announced.is_empty() {
            notes.push(announcement_note(&announced));
        }
        let api_notes = self.network.end_action(start.network_action, action);
        // Loading a page always fires requests, POSTs included (LinkedIn renders through
        // server-component POSTs); only an interaction's calls tell the agent something.
        if !matches!(action, "navigate" | "back") {
            notes.extend(api_notes);
        }
        notes.extend(self.downloads.report(start.downloads, DOWNLOAD_WAIT).await);
        let mut activity = lock(&self.activity);
        for alert in activity.accepted_alerts.drain(..) {
            notes.push(format!("the page showed an alert (accepted): {alert:?}"));
        }
        if activity.file_chooser.take().is_some() {
            notes.push(
                "the page opened a file chooser. To choose files, use act upload with the ref you just clicked and `files`; otherwise ignore it.".into(),
            );
        }
        if let Some(dialog) = &activity.dialog {
            notes.push(format!(
                "a {} dialog is open and blocks the page: {:?}. Accept or dismiss it before anything else.",
                dialog.kind, dialog.message
            ));
        }
        Ok(ActionResult {
            action: action.to_owned(),
            settled,
            navigated: activity.navigation_count != start.navigation,
            url_before: start.url,
            url_after: activity.url.clone(),
            elapsed_ms: start.started.elapsed().as_millis() as u64,
            notes,
            output: None,
        })
    }

    /// Waits for the page to load and go quiet (see `settle`), e.g. a tab that just
    /// opened in the background. Returns false if it was still busy at `timeout`.
    pub async fn wait_settled(&mut self, timeout: Duration) -> bool {
        self.settle(timeout).await
    }

    pub(crate) fn settle_timeout() -> Duration {
        ACTION_SETTLE_TIMEOUT
    }

    /// Waits until the main frame is loaded and both network and DOM have been quiet for
    /// `QUIET`. Returns false on timeout; the page may still be usable (e.g. a ticker that
    /// never stops mutating), so this is reported rather than treated as an error.
    ///
    /// Loading may take up to `timeout`, but once loaded, the page gets only `QUIET_BUDGET`
    /// to go quiet. Feeds, chat widgets and analytics keep some pages busy forever; past
    /// the load they are usable, and waiting out the full timeout only makes the agent slow.
    async fn settle(&mut self, timeout: Duration) -> bool {
        tokio::time::sleep(GRACE).await;
        let load_deadline = Instant::now() + timeout;
        let mut deadline = load_deadline;
        let mut quiet_phase = false;
        let mut last_count: Option<i64> = None;
        let mut dom_changed_at = Instant::now();
        while Instant::now() < deadline {
            let (loading, network_quiet) = {
                let activity = lock(&self.activity);
                // A dialog freezes the page; it won't settle until the agent decides.
                if activity.dialog.is_some() {
                    return true;
                }
                let now = Instant::now();
                // While the document loads, everything it fetches counts; once it has,
                // only its own site's requests (see `active_own_requests`).
                let quiet = if activity.main_frame_loading {
                    activity.active_requests(now) == 0
                        && now.duration_since(activity.last_network_change) >= QUIET
                } else {
                    activity.active_own_requests(now) == 0
                        && now.duration_since(activity.last_own_change) >= QUIET
                };
                (activity.main_frame_loading, quiet)
            };
            if loading && quiet_phase {
                // A navigation started after all (e.g. a click whose handler navigates
                // asynchronously): it gets the full load allowance again.
                quiet_phase = false;
                deadline = load_deadline;
            }
            if !loading {
                if !quiet_phase {
                    quiet_phase = true;
                    deadline = deadline.min(Instant::now() + QUIET_BUDGET);
                }
                // A renderer that stopped answering (a stalled popup shares it) would
                // hold the call for the whole CDP timeout; the deadline still rules.
                let left = deadline.saturating_duration_since(Instant::now());
                let count = tokio::time::timeout(left, self.mutation_count())
                    .await
                    .ok()
                    .flatten();
                if count.is_none() || count != last_count {
                    last_count = count;
                    dom_changed_at = Instant::now();
                } else if network_quiet && dom_changed_at.elapsed() >= QUIET {
                    return true;
                }
            }
            tokio::time::sleep(POLL).await;
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            let activity = lock(&self.activity);
            let now = Instant::now();
            tracing::debug!(
                loading = activity.main_frame_loading,
                requests = activity.active_requests(now),
                network_quiet_ms =
                    now.duration_since(activity.last_network_change).as_millis() as u64,
                dom_quiet_ms = dom_changed_at.elapsed().as_millis() as u64,
                "page still busy when the wait ended"
            );
        }
        false
    }
}

pub(crate) fn stale(element: ElementRef) -> BrowserError {
    BrowserError::Element(format!(
        "element {element} no longer exists; the page changed. Call observe again."
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameTree {
    frame_tree: FrameTreeNode,
}

#[derive(Deserialize)]
struct FrameTreeNode {
    frame: Frame,
}

#[derive(Deserialize)]
struct Frame {
    id: String,
    url: String,
    /// The fragment, with its `#`, which CDP keeps apart from `url`.
    #[serde(default, rename = "urlFragment")]
    url_fragment: String,
}
