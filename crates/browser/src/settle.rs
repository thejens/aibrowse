use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibrowse_cdp::{Event, Session, SessionEvents};
use serde::Serialize;
use serde_json::json;

/// Requests pending longer than this are treated as long-lived (long polling, analytics
/// beacons, streaming) and don't block "network quiet"; otherwise many SPAs never settle.
const LONG_LIVED: Duration = Duration::from_secs(3);

/// Continuously tracked page activity, fed by a background task reading CDP events.
#[derive(Debug)]
pub(crate) struct Activity {
    /// Pending requests by id: when each started, and the loader (document) it is for.
    inflight: HashMap<String, Pending>,
    pub last_network_change: Instant,
    /// The same, counting only the page's own site's requests (`Pending::foreign`).
    pub last_own_change: Instant,
    /// The main frame started loading a new document and hasn't fired `load` yet.
    pub main_frame_loading: bool,
    /// Bumped on every main-frame document navigation; also invalidates isolated worlds.
    pub navigation_count: u64,
    pub url: String,
    /// A confirm/prompt/beforeunload dialog blocking the page, awaiting a decision.
    pub dialog: Option<Dialog>,
    /// Messages of alerts that were accepted automatically, not yet reported.
    pub accepted_alerts: Vec<String>,
    /// The last file chooser the page opened and nobody has answered yet.
    pub file_chooser: Option<FileChooser>,
    /// Windows the page opened (`window.open`, target=_blank links), counted.
    pub windows_opened: u64,
}

/// A file chooser the page opened. Choosers are intercepted (never shown as a native
/// dialog, which would block the user's screen and which CDP can't drive); the files are
/// set on the `<input type=file>` behind it instead.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileChooser {
    /// The target the input lives in (0 = the page, n = OOPIF n).
    pub target: u32,
    /// The input element, when the chooser was opened by one (it nearly always is).
    pub backend: Option<i64>,
    pub multiple: bool,
}

impl FileChooser {
    fn from_event(target: u32, params: &serde_json::Value) -> Self {
        Self {
            target,
            backend: params["backendNodeId"].as_i64(),
            multiple: params["mode"] == "selectMultiple",
        }
    }
}

/// A JavaScript dialog. While one is open the page's main thread is blocked: nothing
/// renders or runs until it is accepted or dismissed.
#[derive(Debug, Clone, Serialize)]
pub struct Dialog {
    /// "confirm", "prompt" or "beforeunload".
    pub kind: String,
    pub message: String,
    pub default_prompt: String,
}

impl Activity {
    fn new() -> Self {
        Self {
            inflight: HashMap::new(),
            last_network_change: Instant::now(),
            last_own_change: Instant::now(),
            main_frame_loading: false,
            navigation_count: 0,
            url: String::new(),
            dialog: None,
            accepted_alerts: Vec::new(),
            file_chooser: None,
            windows_opened: 0,
        }
    }

    pub fn active_requests(&self, now: Instant) -> usize {
        self.inflight
            .values()
            .filter(|p| now.duration_since(p.started) < LONG_LIVED)
            .count()
    }

    /// Pending requests to the page's own site: once its document has loaded, those
    /// are what can still change it (its data). Ads, trackers and bidding go to other
    /// sites and never stop: on Allrecipes every action waited out its whole budget
    /// for them with the page itself long still.
    pub fn active_own_requests(&self, now: Instant) -> usize {
        self.inflight
            .values()
            .filter(|p| !p.foreign && now.duration_since(p.started) < LONG_LIVED)
            .count()
    }

    /// The main frame committed the document of `loader`. Requests the previous
    /// document left pending are forgotten: analytics it fired on its way out (beacons,
    /// tracking pixels, keepalive fetches) often never report completion, and nothing
    /// they return can reach the new page.
    fn committed(&mut self, loader: &str) {
        let before = self.inflight.len();
        self.inflight.retain(|_, p| p.loader == loader);
        if self.inflight.len() != before {
            self.last_network_change = Instant::now();
        }
    }
}

#[derive(Debug)]
struct Pending {
    started: Instant,
    loader: String,
    /// To another site than the document that sent it.
    foreign: bool,
}

/// A host's site, roughly: its last two labels ("cdn.shop.test" → "shop.test"). Under a
/// two-label public suffix ("co.uk") unrelated sites share one, which only counts a
/// foreign request as the page's own: waited for, as all requests were before.
fn site(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_owned();
    let labels: Vec<&str> = host.split('.').collect();
    Some(labels[labels.len().saturating_sub(2)..].join("."))
}

pub(crate) type SharedActivity = Arc<Mutex<Activity>>;

pub(crate) fn lock(activity: &SharedActivity) -> std::sync::MutexGuard<'_, Activity> {
    activity
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Spawns the tracker; it ends by itself when the session's event stream closes.
pub(crate) fn spawn_tracker(
    mut events: SessionEvents,
    session: Session,
    main_frame_id: String,
) -> SharedActivity {
    let activity = Arc::new(Mutex::new(Activity::new()));
    let shared = Arc::clone(&activity);
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if event.method == "Page.javascriptDialogOpening" {
                on_dialog(&shared, &session, &event).await;
            } else {
                apply(&mut lock(&shared), &event, &main_frame_id);
            }
        }
    });
    activity
}

/// Alerts carry information but no decision, so they are accepted at once (a blocked page
/// would stall every later action) and their text is reported with the next action.
/// Confirm, prompt and beforeunload ask for a decision, which belongs to the agent.
async fn on_dialog(shared: &SharedActivity, session: &Session, event: &Event) {
    let params = &event.params;
    let kind = params["type"].as_str().unwrap_or("alert").to_owned();
    let message = params["message"].as_str().unwrap_or_default().to_owned();
    if kind == "alert" {
        lock(shared).accepted_alerts.push(message);
        if let Err(err) = session
            .call("Page.handleJavaScriptDialog", json!({ "accept": true }))
            .await
        {
            tracing::warn!(%err, "could not accept alert");
        }
    } else {
        lock(shared).dialog = Some(Dialog {
            kind,
            message,
            default_prompt: params["defaultPrompt"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        });
    }
}

/// Tracks requests (and file choosers) of out-of-process iframe `target`, so "network
/// quiet" covers the whole page.
///
/// Requests share one map with the page's own: Chrome's request ids are unique across
/// renderers, and a frame's document request starts in the parent's session but finishes
/// in the frame's, so per-session bookkeeping would never see it complete. Requests of a
/// frame that goes away mid-flight simply age out (`LONG_LIVED`).
pub(crate) fn spawn_network_tracker(
    mut events: SessionEvents,
    activity: SharedActivity,
    target: u32,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let mut state = lock(&activity);
            if !apply_network(&mut state, &event) && event.method == "Page.fileChooserOpened" {
                state.file_chooser = Some(FileChooser::from_event(target, &event.params));
            }
        }
    });
}

/// Returns whether the event was a network event.
fn apply_network(state: &mut Activity, event: &Event) -> bool {
    let params = &event.params;
    let key = || params["requestId"].as_str().unwrap_or_default().to_owned();
    match event.method.as_str() {
        "Network.requestWillBeSent" => {
            // WebSockets and EventSource connections are long-lived by design. Pings
            // (`<a ping>`, `navigator.sendBeacon`) are fire-and-forget analytics whose
            // response the page never sees; one sent as a link navigates away often
            // gets no completion event at all, which held every such click for
            // `LONG_LIVED`.
            if !matches!(
                params["type"].as_str(),
                Some("WebSocket" | "EventSource" | "Ping")
            ) {
                let request = site(params["request"]["url"].as_str().unwrap_or_default());
                let document = site(params["documentURL"].as_str().unwrap_or_default());
                let foreign = request.is_some() && document.is_some() && request != document;
                let pending = Pending {
                    started: Instant::now(),
                    loader: params["loaderId"].as_str().unwrap_or_default().to_owned(),
                    foreign,
                };
                state.inflight.insert(key(), pending);
                state.last_network_change = Instant::now();
                if !foreign {
                    state.last_own_change = Instant::now();
                }
            }
            true
        }
        "Network.loadingFinished" | "Network.loadingFailed" => {
            if let Some(done) = state.inflight.remove(&key()) {
                state.last_network_change = Instant::now();
                if !done.foreign {
                    state.last_own_change = Instant::now();
                }
            }
            true
        }
        _ => false,
    }
}

fn apply(state: &mut Activity, event: &Event, main_frame_id: &str) {
    if apply_network(state, event) {
        return;
    }
    let params = &event.params;
    let is_main = |frame_id: Option<&str>| frame_id == Some(main_frame_id);
    match event.method.as_str() {
        "Page.frameStartedNavigating" | "Page.frameStartedLoading"
            if is_main(params["frameId"].as_str()) =>
        {
            state.main_frame_loading = true;
        }
        "Page.frameNavigated" => {
            let frame = &params["frame"];
            if frame["parentId"].is_null() && is_main(frame["id"].as_str()) {
                state.navigation_count += 1;
                state.committed(frame["loaderId"].as_str().unwrap_or_default());
                // CDP keeps the fragment (with its `#`) apart from the address.
                state.url = format!(
                    "{}{}",
                    frame["url"].as_str().unwrap_or_default(),
                    frame["urlFragment"].as_str().unwrap_or_default()
                );
            }
        }
        "Page.navigatedWithinDocument" if is_main(params["frameId"].as_str()) => {
            state.url = params["url"].as_str().unwrap_or_default().to_owned();
        }
        "Page.loadEventFired" => state.main_frame_loading = false,
        "Page.javascriptDialogClosed" => state.dialog = None,
        "Page.windowOpen" => state.windows_opened += 1,
        "Page.fileChooserOpened" => state.file_chooser = Some(FileChooser::from_event(0, params)),
        "Page.frameStoppedLoading" if is_main(params["frameId"].as_str()) => {
            state.main_frame_loading = false;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(method: &str, params: serde_json::Value) -> Event {
        Event {
            method: method.to_owned(),
            session_id: None,
            params,
        }
    }

    #[test]
    fn pings_do_not_count_as_pending_requests() {
        let mut state = Activity::new();
        for (id, kind) in [("1", "Ping"), ("2", "WebSocket"), ("3", "Fetch")] {
            apply_network(
                &mut state,
                &event(
                    "Network.requestWillBeSent",
                    json!({ "requestId": id, "type": kind }),
                ),
            );
        }
        assert_eq!(state.active_requests(Instant::now()), 1);
        apply_network(
            &mut state,
            &event("Network.loadingFinished", json!({ "requestId": "3" })),
        );
        assert_eq!(state.active_requests(Instant::now()), 0);
    }

    #[test]
    fn a_new_document_forgets_what_the_old_one_left_pending() {
        let mut state = Activity::new();
        for (id, loader) in [("beacon", "old"), ("document", "new")] {
            apply_network(
                &mut state,
                &event(
                    "Network.requestWillBeSent",
                    json!({ "requestId": id, "loaderId": loader, "type": "Fetch" }),
                ),
            );
        }
        apply(
            &mut state,
            &event(
                "Page.frameNavigated",
                json!({ "frame": { "id": "main", "loaderId": "new", "url": "https://x.test/" } }),
            ),
            "main",
        );
        assert_eq!(state.navigation_count, 1);
        assert_eq!(state.active_requests(Instant::now()), 1);
        assert!(state.inflight.contains_key("document"));
    }

    #[test]
    fn only_the_sites_own_requests_count_once_loaded() {
        let mut state = Activity::new();
        let page = "https://www.recipes.test/banana-bread";
        for (id, url) in [
            ("ad", "https://ads.bidder.example/auction"),
            ("api", "https://api.recipes.test/reviews"),
        ] {
            apply_network(
                &mut state,
                &event(
                    "Network.requestWillBeSent",
                    json!({ "requestId": id, "type": "Fetch", "documentURL": page, "request": { "url": url } }),
                ),
            );
        }
        let now = Instant::now();
        assert_eq!(state.active_requests(now), 2);
        assert_eq!(state.active_own_requests(now), 1);
        apply_network(
            &mut state,
            &event("Network.loadingFinished", json!({ "requestId": "api" })),
        );
        assert_eq!(state.active_own_requests(Instant::now()), 0);
        assert_eq!(state.active_requests(Instant::now()), 1);
        assert_eq!(
            site("https://cdn.shop.test/x.js").as_deref(),
            Some("shop.test")
        );
    }
}
