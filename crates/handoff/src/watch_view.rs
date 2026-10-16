//! The watch view: one stable link per server that always shows what the agent's
//! browser is doing, read-only. When a handoff or approval is pending, the same page
//! becomes the controls for it, and goes back to watching once it is resolved.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aibrowse_cdp::Session;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::input::ClientEvent;
use crate::server::{AppState, Grant, PAGE, apply_input, current_recording, open_handoff};
use crate::stream::{Ended, Link, Sink, send_json, stream_tab};
use crate::{Kind, Status, lock, same_token, token};

/// A pending handoff as watchers see it.
#[derive(Clone)]
pub(crate) struct Beat {
    pub kind: Kind,
    pub by_user: bool,
    pub reason: String,
    /// The user's input kinematics are being recorded (the page shows it).
    pub recording: bool,
    pub status: watch::Receiver<Status>,
}

#[derive(Clone)]
pub(crate) struct WatchState {
    /// Guards the watch link like a handoff token: whoever has it sees the browser, and
    /// controls it while a handoff is pending.
    pub token: String,
    /// The tab the agent works in.
    pub source: watch::Sender<Option<Session>>,
    /// The latest handoff, if any.
    pub current: watch::Sender<Option<Beat>>,
    /// Whether the user has the browser (a handoff is pending): privacy mode, in which
    /// what they send isn't kept (see `HandoffServer::user_has_browser`).
    pub user_has_browser: watch::Sender<bool>,
    viewers: Arc<AtomicUsize>,
}

impl WatchState {
    pub fn new(fixed_token: Option<String>) -> Self {
        Self {
            token: fixed_token.unwrap_or_else(token),
            source: watch::channel(None).0,
            current: watch::channel(None).0,
            user_has_browser: watch::channel(false).0,
            viewers: Arc::default(),
        }
    }

    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::Relaxed)
    }
}

/// Counts an open watch view for as long as it lives.
struct Viewer(Arc<AtomicUsize>);

impl Viewer {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(counter))
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn authorized(state: &AppState, token: &str) -> bool {
    same_token(&state.watch.token, token)
}

pub(crate) async fn watch_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Response {
    if !authorized(&state, &token) {
        return (StatusCode::NOT_FOUND, "Unknown live view link.").into_response();
    }
    let page = PAGE
        .replace("{{MODE}}", "watch")
        .replace("{{KIND}}", "")
        .replace("{{REASON}}", "Watching the agent's browser")
        .replace(
            "{{HINT}}",
            "Read-only. If the agent needs you, this page turns into the controls.",
        )
        .replace("{{DONE}}", "Done")
        .replace("{{CANCEL}}", "Cancel")
        .replace("{{TYPEBOX_STYLE}}", "display:none")
        .replace("{{RECORDING}}", "hidden");
    Html(page).into_response()
}

pub(crate) async fn watch_socket(
    State(state): State<AppState>,
    Path(token): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !authorized(&state, &token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    upgrade.on_upgrade(move |ws| watch(ws, state))
}

/// Streams the tab the agent works in, following it to other tabs, until `until` (when
/// given) or the viewer leaves. Returns false once the viewer left.
pub(crate) async fn follow_agent(
    mut source: watch::Receiver<Option<Session>>,
    sink: &Sink,
    until: Option<Instant>,
) -> bool {
    let over = || async move {
        match until {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    };
    loop {
        if until.is_some_and(|deadline| deadline <= Instant::now()) {
            return true;
        }
        let session = source.borrow_and_update().clone();
        match session {
            Some(session) => {
                // Restart the stream on the agent's new tab, or stop it when time is up
                // (through `stream_tab`'s own stop, so the screencast is ended).
                let changed = async {
                    tokio::select! {
                        _ = source.changed() => {}
                        () = over() => {}
                    }
                };
                match stream_tab(&session, sink, changed).await {
                    Ended::ViewerLeft => return false,
                    Ended::Stopped => {}
                    // A newer live view (another handoff's) has the tab's picture: this
                    // view waits for the agent's next tab rather than take it back.
                    Ended::Superseded => tokio::select! {
                        changed = source.changed() => if changed.is_err() { return true },
                        () = over() => return true,
                    },
                }
            }
            None => {
                if !send_json(sink, json!({ "t": "idle" })).await {
                    return false;
                }
                tokio::select! {
                    changed = source.changed() => if changed.is_err() { return true },
                    () = over() => return true,
                }
            }
        }
    }
}

async fn watch(ws: WebSocket, state: AppState) {
    let _viewer = Viewer::new(&state.watch.viewers);
    let (sink, mut stream) = ws.split();
    let sink: Sink = Link::new(sink);

    let show = follow_agent(state.watch.source.subscribe(), &sink, None);

    // Switch the page between watching and the controls of a pending handoff.
    let mut current = state.watch.current.subscribe();
    let modes = async {
        loop {
            let beat = current.borrow_and_update().clone();
            match beat.filter(|b| *b.status.borrow() == Status::Pending) {
                Some(mut beat) => {
                    let handoff = json!({ "kind": beat.kind, "reason": beat.reason, "by_user": beat.by_user, "recording": beat.recording });
                    if !send_json(&sink, json!({ "t": "mode", "handoff": handoff })).await {
                        return;
                    }
                    tokio::select! {
                        _ = beat.status.wait_for(|s| *s != Status::Pending) => {}
                        _ = current.changed() => continue,
                    }
                    if !send_json(&sink, json!({ "t": "mode", "handoff": null })).await {
                        return;
                    }
                }
                None => {
                    if !send_json(&sink, json!({ "t": "mode", "handoff": null })).await {
                        return;
                    }
                }
            }
            if current.changed().await.is_err() {
                return;
            }
        }
    };

    // Input counts only while a handoff is pending, and goes to that handoff's tab.
    let input = async {
        while let Some(Ok(message)) = stream.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<ClientEvent>(&text) else {
                continue;
            };
            // The picture's size counts while only watching too.
            if let ClientEvent::View { w, h } = event {
                sink.set_screen(w, h);
                continue;
            }
            let pending = {
                let active = lock(&state.shared);
                active
                    .as_ref()
                    .filter(|a| *a.status.borrow() == Status::Pending)
                    .map(Grant::of)
            };
            match (pending, &event) {
                (Some(grant), _) => {
                    apply_input(&grant, &event).await;
                }
                // Nothing pending: the user may take over the agent's tab.
                (None, ClientEvent::Takeover) => {
                    let tab = state.watch.source.borrow().clone();
                    if let Some(session) = tab {
                        open_handoff(
                            &state.shared,
                            &state.watch,
                            session,
                            Kind::Takeover,
                            "You have the browser. The agent is paused until you hand it back.",
                            None,
                            true,
                            current_recording(&state.recording),
                            None,
                        );
                    }
                }
                _ => {} // input while only watching is ignored
            }
        }
    };

    tokio::select! {
        _ = show => {}
        () = modes => {}
        () = input => {}
    }
}
