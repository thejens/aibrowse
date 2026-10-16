use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aibrowse_cdp::Session;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::watch;

use crate::input::{ClientEvent, to_cdp};
use crate::kinematics::{Recorder, Recording, RecordingSlot};
use crate::stream::{Ended, Link, Sink, send_json, stream_tab};
use crate::watch_view::{Beat, WatchState, follow_agent, watch_page, watch_socket};
use crate::{Active, Config, Kind, Shared, Status, Ticket, lock, same_token, token, trail};

/// A pending handoff the user never resolves is cancelled after this long, so a
/// forgotten link stops granting control of the browser.
const EXPIRY: Duration = Duration::from_secs(15 * 60);
/// How long a handoff link goes on showing the agent at work after the user hands back.
/// Bounded: the link was meant for one request, not as a standing view of the browser
/// (that is the watch link's job).
const AFTERWARDS: Duration = Duration::from_secs(5 * 60);

pub(crate) const PAGE: &str = include_str!("page.html");

/// Serves live views. One handoff can be active at a time, as there is one browser.
#[derive(Clone)]
pub struct HandoffServer {
    shared: Shared,
    watch: WatchState,
    recording: RecordingSlot,
    base_url: String,
}

/// Everything the HTTP handlers need.
#[derive(Clone)]
pub(crate) struct AppState {
    pub shared: Shared,
    pub watch: WatchState,
    pub recording: RecordingSlot,
}

impl HandoffServer {
    pub async fn start(config: Config) -> io::Result<Self> {
        let listener = bind(config.bind).await?;
        let local = listener.local_addr()?;
        let base_url = config.public_url.unwrap_or_else(|| {
            let host = config.public_host.unwrap_or_else(|| local.ip().to_string());
            format!("http://{host}:{}", local.port())
        });
        let shared = Shared::default();
        let watch = WatchState::new(config.watch_token);
        let recording = RecordingSlot::default();
        let app = Router::new()
            .route("/h/{token}", get(page))
            .route("/h/{token}/ws", get(socket))
            .route("/w/{token}", get(watch_page))
            .route("/w/{token}/ws", get(watch_socket))
            .with_state(AppState {
                shared: Arc::clone(&shared),
                watch: watch.clone(),
                recording: Arc::clone(&recording),
            });
        tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                tracing::error!(%err, "handoff server stopped");
            }
        });
        Ok(Self {
            shared,
            watch,
            recording,
            base_url,
        })
    }

    /// Starts a handoff of the tab behind `session`, cancelling any previous one.
    ///
    /// With `done_url`, the handoff also completes by itself once the tab's URL contains
    /// that text (e.g. the feed URL a login redirects to), so the user needn't remember
    /// to press Done.
    pub fn open(
        &self,
        session: Session,
        kind: Kind,
        reason: &str,
        done_url: Option<&str>,
    ) -> Ticket {
        let (id, token) = open_handoff(
            &self.shared,
            &self.watch,
            session,
            kind,
            reason,
            done_url,
            false,
            current_recording(&self.recording),
            None,
        );
        Ticket {
            id,
            url: format!("{}/h/{token}", self.base_url),
        }
    }

    /// Asks the user for a one-time code (2FA, an SMS code) on a page with nothing but a
    /// box for it; the code is typed into `field` (a backend node id in the page itself,
    /// or wherever the page has focus) and Enter pressed. Easier on a phone than
    /// driving the whole site.
    pub fn open_code(&self, session: Session, reason: &str, field: Option<i64>) -> Ticket {
        let (id, token) = open_handoff(
            &self.shared,
            &self.watch,
            session,
            Kind::Code,
            reason,
            None,
            false,
            None,
            field,
        );
        Ticket {
            id,
            url: format!("{}/h/{token}", self.base_url),
        }
    }

    /// What the user did during handoff `id`, and whether they took over on their own.
    pub fn report(&self, id: &str) -> Option<(bool, Vec<String>)> {
        let active = lock(&self.shared);
        let active = active.as_ref().filter(|a| a.id == id)?;
        Some((active.by_user, trail::entries(&active.trail)))
    }

    /// Privacy mode: true while the user has the browser, false otherwise. Whoever
    /// records what the browser sends (the network log) follows it.
    pub fn user_has_browser(&self) -> watch::Receiver<bool> {
        self.watch.user_has_browser.subscribe()
    }

    /// Whether a handoff is waiting for the user (agent tools should stay hands-off).
    pub fn pending(&self) -> Option<String> {
        self.expire();
        let active = lock(&self.shared);
        active
            .as_ref()
            .filter(|a| *a.status.borrow() == Status::Pending)
            .map(|a| a.id.clone())
    }

    /// Waits up to `timeout` for the handoff to be resolved. `None` if the id is unknown.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Option<Status> {
        self.expire();
        let (mut rx, left) = {
            let active = lock(&self.shared);
            let active = active.as_ref().filter(|a| a.id == id)?;
            let left = active
                .give_up_after
                .map(|after| after.saturating_sub(active.created.elapsed()));
            (active.status.subscribe(), left)
        };
        // Waiting ends when the handoff gives up on the user, not a call later.
        let timeout = left.map_or(timeout, |left| timeout.min(left));
        let _ = tokio::time::timeout(timeout, rx.wait_for(|s| *s != Status::Pending)).await;
        self.expire();
        let status = *rx.borrow();
        Some(status)
    }

    /// Marks the handoff done on the user's behalf (their task was detected as finished).
    pub fn complete(&self, id: &str) {
        if let Some(active) = lock(&self.shared).as_ref().filter(|a| a.id == id) {
            active.status.send_if_modified(|s| {
                let pending = *s == Status::Pending;
                if pending {
                    *s = Status::Done;
                }
                pending
            });
        }
    }

    /// Records the user's input kinematics into `recording` during handoffs from now on
    /// (`None`: don't). Set from the profile's policy; the live view shows when it's on.
    pub fn set_recording(&self, recording: Option<Recording>) {
        *self
            .recording
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = recording;
    }

    /// The persistent, read-only live view of whatever tab the agent works in. It turns
    /// into the controls whenever a handoff or approval is pending.
    pub fn watch_url(&self) -> String {
        format!("{}/w/{}", self.base_url, self.watch.token)
    }

    /// Points the watch view at the tab the agent now works in (`None`: no browser).
    pub fn set_tab(&self, session: Option<Session>) {
        let changed = self.watch.source.borrow().as_ref().map(Session::id)
            != session.as_ref().map(Session::id);
        if changed {
            self.watch.source.send_replace(session);
        }
    }

    /// How many watch views are open (if any, handoffs show up there).
    pub fn watchers(&self) -> usize {
        self.watch.viewers()
    }

    /// The most recent handoff's id, resolved or not.
    pub fn last_id(&self) -> Option<String> {
        lock(&self.shared).as_ref().map(|a| a.id.clone())
    }

    pub fn cancel(&self) {
        if let Some(active) = lock(&self.shared).as_ref() {
            active.status.send_replace(Status::Cancelled);
        }
    }

    /// How long handoff `id` waits for the user before giving up (`None`: as long as it
    /// takes). Handoffs start with `EXPIRY`.
    pub fn give_up_after(&self, id: &str, after: Option<Duration>) {
        if let Some(active) = lock(&self.shared).as_mut().filter(|a| a.id == id) {
            active.give_up_after = after;
        }
    }

    /// Whether handoff `id` was given up on because the user didn't respond in time.
    pub fn timed_out(&self, id: &str) -> bool {
        self.expire();
        lock(&self.shared)
            .as_ref()
            .is_some_and(|a| a.id == id && a.timed_out)
    }

    fn expire(&self) {
        if let Some(active) = lock(&self.shared).as_mut()
            && active
                .give_up_after
                .is_some_and(|after| active.created.elapsed() > after)
            && *active.status.borrow() == Status::Pending
        {
            active.timed_out = true;
            active.status.send_replace(Status::Cancelled);
        }
    }
}

/// Binds the live view port, retrying briefly: after a reload the previous server may
/// take a moment to release a fixed port.
async fn bind(address: std::net::SocketAddr) -> io::Result<tokio::net::TcpListener> {
    let mut attempt = 0;
    loop {
        match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(err) if err.kind() == io::ErrorKind::AddrInUse && attempt < 20 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

pub(crate) fn current_recording(slot: &RecordingSlot) -> Option<Recording> {
    slot.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Starts a handoff, cancelling any previous one. Returns (id, token).
#[allow(
    clippy::too_many_arguments,
    reason = "one call site per kind of handoff"
)]
pub(crate) fn open_handoff(
    shared: &Shared,
    watch_state: &WatchState,
    session: Session,
    kind: Kind,
    reason: &str,
    done_url: Option<&str>,
    by_user: bool,
    recording: Option<Recording>,
    code_field: Option<i64>,
) -> (String, String) {
    let mut active = lock(shared);
    if let Some(previous) = active.take() {
        previous.status.send_replace(Status::Cancelled);
    }
    let id = token()[..8].to_owned();
    let token = token();
    let (status, status_rx) = watch::channel(Status::Pending);
    watch_state.current.send_replace(Some(Beat {
        kind,
        by_user,
        reason: reason.to_owned(),
        recording: recording.is_some(),
        status: status_rx.clone(),
    }));
    if let Some(pattern) = done_url {
        tokio::spawn(complete_on_url(
            session.clone(),
            pattern.to_owned(),
            status.clone(),
        ));
    }
    // Privacy mode lasts while this handoff is pending; when it resolves, it ends unless
    // another handoff has taken its place in the meantime.
    watch_state.user_has_browser.send_replace(true);
    {
        let mut resolved = status_rx.clone();
        let shared = shared.clone();
        let user_has_browser = watch_state.user_has_browser.clone();
        tokio::spawn(async move {
            let _ = resolved.wait_for(|s| *s != Status::Pending).await;
            let another_pending = lock(&shared)
                .as_ref()
                .is_some_and(|a| *a.status.borrow() == Status::Pending);
            if !another_pending {
                user_has_browser.send_replace(false);
            }
        });
    }
    let trail = trail::Trail::default();
    let recorder = recording.map(|recording| Recorder::start(recording, status_rx.clone()));
    trail::record_navigation(session.clone(), trail.clone(), status_rx);
    *active = Some(Active {
        id: id.clone(),
        token: token.clone(),
        kind,
        by_user,
        trail,
        recorder,
        reason: reason.to_owned(),
        code_field,
        session,
        created: Instant::now(),
        give_up_after: Some(EXPIRY),
        timed_out: false,
        status,
    });
    (id, token)
}

/// Resolves the handoff once the tab's main frame reaches a URL containing `pattern`.
/// Whether `url` is where a handoff's `done_url` says the user is through: the pattern
/// in the address's host and path. A sign-in page carries where it returns to in its
/// query (`redirect_uri=https%3A%2F%2Fexcel.cloud.microsoft…`): matched anywhere, a
/// Microsoft sign-in handed the browser back the moment it redirected to the company's
/// Okta, before the user had signed in. A pattern with a query of its own is matched
/// against the whole address.
fn reached(url: &str, pattern: &str) -> bool {
    if pattern.contains('?') {
        return url.contains(pattern);
    }
    let place = url.split(['?', '#']).next().unwrap_or(url);
    place.contains(pattern)
}

async fn complete_on_url(session: Session, pattern: String, status: watch::Sender<Status>) {
    // Give the page a moment to render after arriving, so the agent's first look at it
    // isn't a half-loaded page.
    const ARRIVAL_GRACE: Duration = Duration::from_millis(1500);
    let mut events = session.events();
    let mut resolved = status.subscribe();
    let main_frame = session
        .call("Page.getFrameTree", json!({}))
        .await
        .ok()
        .and_then(|t| t["frameTree"]["frame"]["id"].as_str().map(str::to_owned));
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            _ = resolved.wait_for(|s| *s != Status::Pending) => return,
        };
        let Some(event) = event else { return };
        let url = match event.method.as_str() {
            "Page.frameNavigated" if event.params["frame"]["parentId"].is_null() => {
                event.params["frame"]["url"].as_str()
            }
            "Page.navigatedWithinDocument"
                if event.params["frameId"].as_str() == main_frame.as_deref() =>
            {
                event.params["url"].as_str()
            }
            _ => None,
        };
        if url.is_some_and(|url| reached(url, &pattern)) {
            tokio::time::sleep(ARRIVAL_GRACE).await;
            status.send_if_modified(|s| {
                let pending = *s == Status::Pending;
                if pending {
                    *s = Status::Done;
                }
                pending
            });
            return;
        }
    }
}

/// What a live view connection needs of the handoff it serves.
pub(crate) struct Grant {
    pub kind: Kind,
    pub reason: String,
    pub code_field: Option<i64>,
    pub session: Session,
    pub status: watch::Sender<Status>,
    pub trail: trail::Trail,
    pub recorder: Option<Arc<Recorder>>,
}

impl Grant {
    pub fn of(active: &Active) -> Self {
        Self {
            kind: active.kind,
            reason: active.reason.clone(),
            code_field: active.code_field,
            session: active.session.clone(),
            status: active.status.clone(),
            trail: active.trail.clone(),
            recorder: active.recorder.clone(),
        }
    }
}

/// The pending handoff matching `token`.
fn find(shared: &Shared, token: &str) -> Option<Grant> {
    let active = lock(shared);
    let active = active.as_ref()?;
    // Constant-time comparison: the token is the only credential.
    let matches = same_token(&active.token, token);
    let live = matches
        && *active.status.borrow() == Status::Pending
        && active
            .give_up_after
            .is_none_or(|after| active.created.elapsed() <= after);
    live.then(|| Grant::of(active))
}

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    match find(&state.shared, &token) {
        Some(grant) => Html(render_page(
            grant.kind,
            &grant.reason,
            grant.recorder.is_some(),
        ))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "This handoff link has expired or was already used.",
        )
            .into_response(),
    }
}

async fn socket(
    State(state): State<AppState>,
    Path(token): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    match find(&state.shared, &token) {
        Some(grant) => {
            let agent = state.watch.source.subscribe();
            upgrade.on_upgrade(move |ws| live_view(ws, grant, agent))
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Streams the tab to the user and the user's input to the tab, until Done/Cancel or
/// the connection drops.
async fn live_view(ws: WebSocket, grant: Grant, agent: watch::Receiver<Option<Session>>) {
    let Grant {
        session, status, ..
    } = &grant;
    let (sink, mut stream) = ws.split();
    let sink: Sink = Link::new(sink);
    let mut status_rx = status.subscribe();
    let code_only = grant.kind == Kind::Code;
    let show = async {
        // The code relay page shows no picture: nothing to stream.
        if code_only {
            let _ = status.subscribe().wait_for(|s| *s != Status::Pending).await;
            return;
        }
        let stopped = async {
            let _ = status_rx.wait_for(|s| *s != Status::Pending).await;
        };
        let viewer_there = stream_tab(session, &sink, stopped).await != Ended::ViewerLeft;
        // The picture can stop while the handoff is still open (the screencast couldn't
        // start under load, the tab went away). Done, Cancel and Approve must still get
        // through, so the connection stays until the handoff is resolved either way.
        let mut resolution = status.subscribe();
        let _ = resolution.wait_for(|s| *s != Status::Pending).await;
        if !viewer_there {
            return;
        }
        // Tell the page, also when it was resolved elsewhere (the goal was reached, or
        // the agent ended the session), so the user knows they are done. After Done or
        // Approve the page goes on showing the agent at work for a while, read-only:
        // the user sees what happens with what they handed over.
        let resolved = *status.borrow();
        let watching = resolved == Status::Done;
        let message = json!({
            "t": "resolved",
            "status": resolved,
            "watching": watching.then_some(AFTERWARDS.as_secs()),
        });
        if send_json(&sink, message).await && watching {
            follow_agent(agent, &sink, Some(tokio::time::Instant::now() + AFTERWARDS)).await;
        }
    };
    let forward = async {
        while let Some(Ok(message)) = stream.next().await {
            // Once the handoff is resolved the page only watches: nothing it sends
            // reaches the browser any more.
            if *status.borrow() != Status::Pending {
                continue;
            }
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<ClientEvent>(&text) else {
                continue;
            };
            if let ClientEvent::View { w, h } = event {
                sink.set_screen(w, h);
                continue;
            }
            apply_input(&grant, &event).await;
        }
    };
    tokio::select! {
        () = show => {}
        () = forward => {}
    }
}

/// Dispatches a viewer's event: input goes to the tab, Done/Cancel resolve the handoff.
pub(crate) async fn apply_input(grant: &Grant, event: &ClientEvent) {
    let Grant {
        session,
        status,
        trail,
        recorder,
        ..
    } = grant;
    match event {
        ClientEvent::Done => {
            status.send_replace(Status::Done);
            return;
        }
        ClientEvent::Cancel => {
            status.send_replace(Status::Cancelled);
            return;
        }
        ClientEvent::Code { code } => {
            relay_code(session, grant.code_field, code).await;
            trail::push(
                trail,
                format!("entered a code ({} characters)", code.chars().count()),
            );
            status.send_replace(Status::Done);
            return;
        }
        _ => {}
    }
    trail::note_input(session, trail, event).await;
    if let Some((method, params)) = to_cdp(event)
        && let Err(err) = session.call(method, params).await
    {
        tracing::debug!(%err, method, "live view input failed");
    }
    if let Some(recorder) = recorder {
        recorder.observe(session, event).await;
    }
}

/// Types a relayed code into the field (focused first, when known) and presses Enter,
/// as real keyboard input.
async fn relay_code(session: &Session, field: Option<i64>, code: &str) {
    if let Some(backend) = field
        && let Err(err) = session
            .call("DOM.focus", json!({ "backendNodeId": backend }))
            .await
    {
        tracing::debug!(%err, "could not focus the code field");
    }
    let _ = session
        .call("Input.insertText", json!({ "text": code.trim() }))
        .await;
    for kind in ["keyDown", "keyUp"] {
        let _ = session
            .call(
                "Input.dispatchKeyEvent",
                json!({ "type": kind, "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13, "text": if kind == "keyDown" { "\r" } else { "" } }),
            )
            .await;
    }
}

/// The live view page, worded for what the user is asked to do.
pub(crate) fn render_page(kind: Kind, reason: &str, recording: bool) -> String {
    let (hint, done, cancel, typebox) = match kind {
        Kind::Takeover => (
            "You're controlling the agent's browser. It notices when you're through; press Done if it doesn't.",
            "Done",
            "Cancel",
            "",
        ),
        Kind::Approval => (
            "The agent is waiting for your decision. Nothing happens unless you approve.",
            "Approve",
            "Deny",
            "display:none",
        ),
        Kind::Code => (
            "Type the code you received. It is entered on the page for you.",
            "Send code",
            "Cancel",
            "",
        ),
    };
    let kind_name = match kind {
        Kind::Takeover => "takeover",
        Kind::Approval => "approval",
        Kind::Code => "code",
    };
    PAGE.replace("{{MODE}}", "handoff")
        .replace("{{KIND}}", kind_name)
        .replace("{{REASON}}", &escape_html(reason))
        .replace("{{HINT}}", hint)
        .replace("{{DONE}}", done)
        .replace("{{CANCEL}}", cancel)
        .replace("{{TYPEBOX_STYLE}}", typebox)
        .replace("{{RECORDING}}", if recording { "" } else { "hidden" })
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::reached;

    #[test]
    fn a_done_url_is_a_place_not_a_parameter() {
        let sign_in = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?redirect_uri=https%3A%2F%2Fexcel.cloud.microsoft%2Fauth.html";
        assert!(!reached(sign_in, "excel.cloud.microsoft"));
        assert!(reached(
            "https://excel.cloud.microsoft/en-us/?wdOrigin=X",
            "excel.cloud.microsoft"
        ));
        assert!(reached(
            "https://www.linkedin.com/feed/",
            "linkedin.com/feed"
        ));
        // A pattern with a query is matched as written.
        assert!(reached("https://app.test/done?step=2", "done?step=2"));
    }
}
