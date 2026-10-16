//! The hook at the tool-call boundary: every call is turned into a trace event, with
//! the page captured after it.
//!
//! The capture (URL, title, screenshot, network) runs in a task after the tool has
//! answered, so tracing adds no latency to the call itself. It takes the session lock
//! like any tool, so it sees the state the call left, before the agent's next call.

use std::time::{Duration, Instant};

use aibrowse_network::render;
use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolRequestParams, CallToolResponse};
use serde_json::Value;

use super::event::{self, Event, MAX_RESULT_CHARS, Network};
use super::notes::Notes;
use super::writer::Message;
use super::{Call, Tracer};
use crate::respond::parse_ref;
use crate::server::Aibrowse;

/// The page capture after a call gives up after this long rather than hold the session.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// Tools that change what the page shows, so a screenshot after them shows the effect.
/// Read-only tools (observe, read, api_list...) get none: it would cost time and show
/// nothing new.
fn changes_page(tool: &str) -> bool {
    matches!(
        tool,
        "navigate" | "back" | "act" | "session_open" | "tabs" | "handoff_status"
    )
}

/// Tools whose requests are worth listing: those that act, and `api_call`.
fn makes_requests(tool: &str) -> bool {
    changes_page(tool) || tool == "api_call"
}

impl Aibrowse {
    /// Starts tracing a call, redacting its arguments. `None` when tracing is off.
    pub(crate) async fn trace_begin(&self, request: &CallToolRequestParams) -> Option<Call> {
        if !self.tracer.is_on() {
            return None;
        }
        let args = Value::Object(request.arguments.clone().unwrap_or_default());
        let secret = event::types_text(&request.name, &args) && self.types_secret(&args).await;
        let args = event::redact_args(&request.name, &args, secret);
        self.tracer.begin(&request.name, args)
    }

    /// Whether an `act fill`/`type` call is about to type into a secret field. Anything
    /// that can't be checked counts as secret.
    async fn types_secret(&self, args: &Value) -> bool {
        if self.user_has_browser() {
            return true;
        }
        let mut guard = self.live.lock().await;
        let Some(live) = guard.as_mut().filter(|l| l.has_tabs()) else {
            return true;
        };
        if live.tab().dialog().is_some() {
            return true; // the page is frozen; inspecting it would hang
        }
        let reference = match args["ref"].as_str().map(parse_ref) {
            Some(Ok(reference)) => Some(reference),
            Some(Err(_)) => return true,
            None if args["action"] == "fill" => return true,
            None => None,
        };
        live.tab().is_secret_input(reference).await
    }

    /// Finishes tracing a call: builds its event and hands it to a task that captures the
    /// page and sends it to the writer.
    pub(crate) fn trace_end(
        &self,
        call: Option<Call>,
        response: &Result<CallToolResponse, McpError>,
        notes: Notes,
    ) {
        let Some(call) = call else {
            return;
        };
        let mut screenshot = None;
        let (ok, mut result) = match response {
            Ok(CallToolResponse::Complete(result)) => {
                let texts: Vec<&str> = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
                    .collect();
                // A screenshot the tool returned anyway is the best picture of the page.
                screenshot = result
                    .content
                    .iter()
                    .find_map(|c| c.as_image().map(|i| i.data.clone()));
                (result.is_error != Some(true), texts.join("\n"))
            }
            Ok(_) => (true, String::new()),
            Err(err) => (false, err.message.to_string()),
        };
        let result_truncated = event::truncate(&mut result, MAX_RESULT_CHARS);
        let event = Event {
            seq: call.seq,
            time: event::rfc3339(call.wall),
            tool: call.tool.clone(),
            args: call.args,
            duration_ms: call.started.elapsed().as_millis() as u64,
            ok,
            result,
            result_truncated,
            actions: notes.actions,
            navigator: notes.navigator,
            human: notes.human,
            api: notes.api,
            ..Event::default()
        };
        let this = self.clone();
        let outline = notes.outline;
        self.tracer.track(1);
        tokio::spawn(async move {
            let mut event = event;
            let profile = match tokio::time::timeout(
                CAPTURE_TIMEOUT,
                this.capture(&mut event, call.started, &mut screenshot),
            )
            .await
            {
                Ok(profile) => profile,
                Err(_) => {
                    event.capture_skipped = Some("the page did not answer in time".into());
                    this.defaults.profile.clone()
                }
            };
            let closed = event.tool == "session_close";
            let tracer: &Tracer = &this.tracer;
            tracer.send(Message::Event {
                profile,
                event: Box::new(event),
                screenshot,
                outline,
            });
            if closed {
                tracer.send(Message::Rotate);
            }
            tracer.track(-1);
        });
    }

    /// Fills in the page after the call; returns the session's profile.
    async fn capture(
        &self,
        event: &mut Event,
        since: Instant,
        screenshot: &mut Option<String>,
    ) -> String {
        let mut guard = self.live.lock().await;
        // Checked with the session held: handoffs and approvals start under this lock,
        // so none can start between the check and the screenshot.
        if self.user_has_browser() {
            event.capture_skipped = Some("the user had the browser".into());
            return self.defaults.profile.clone();
        }
        let Some(live) = guard.as_mut() else {
            return self.defaults.profile.clone();
        };
        let profile = live.browser.profile().name.clone();
        if !live.has_tabs() {
            return profile;
        }
        let tab = live.tab();
        event.url = Some(render::url(&tab.url()));
        if tab.dialog().is_some() {
            event.capture_skipped = Some("a dialog was open".into());
            return profile;
        }
        event.title = Some(tab.title().await);
        if makes_requests(&event.tool) {
            let entries: Vec<_> = tab
                .network()
                .entries()
                .into_iter()
                .filter(|e| e.started >= since)
                .collect();
            event.network = Some(Network::from_entries(&entries));
        }
        if changes_page(&event.tool) && screenshot.is_none() {
            *screenshot = tab.screenshot().await.ok();
        }
        profile
    }

    /// Whether a handoff or approval is open: the user may be typing secrets, so
    /// nothing about the page is recorded.
    fn user_has_browser(&self) -> bool {
        self.handoff.get().is_some_and(|s| s.pending().is_some())
    }
}
