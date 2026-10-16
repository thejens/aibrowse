//! The network tap: a per-tab log of requests built from the CDP `Network` events the tab
//! already receives (enabling `Network` is invisible to pages), feeding the catalog.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aibrowse_cdp::{Event, Session, SessionEvents};
use serde_json::{Value, json};

use crate::call::{ReplayHeader, replay_headers};
use crate::catalog::{Catalog, Sample, is_json};
use crate::payload::{parse_json, truncate};
use crate::rsc::{Flight, decode, is_flight_response};
use crate::shape::Shape;

/// Requests kept per tab. Older ones fall off; the catalog keeps what was learned.
const CAPACITY: usize = 400;
/// Body bytes kept per request for `network_log`.
const KEPT_BODY: usize = 32 * 1024;
/// Responses larger than this aren't fetched for shape inference.
const MAX_FETCHED_BODY: f64 = 4.0 * 1024.0 * 1024.0;
/// Action labels remembered, for attributing requests in the log.
const LABELS: usize = 64;
/// Cookie events that arrived before their request, waiting to be matched.
const EARLY_EXTRA: usize = 256;

/// One request as seen by the browser.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Position in this tab's log, shown to the agent as `#seq`.
    pub seq: u64,
    pub request_id: String,
    pub method: String,
    pub url: String,
    /// CDP resource type: "XHR", "Fetch", "Document", "Script", "Image"...
    pub kind: String,
    pub request_headers: Vec<(String, String)>,
    pub request_body: Option<String>,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub response_headers: Vec<(String, String)>,
    pub response_body: Option<String>,
    pub body_truncated: bool,
    /// A React Server Components response, decoded from the whole body (the kept
    /// `response_body` may be cut short).
    pub flight: Option<Arc<Flight>>,
    /// Bytes on the wire.
    pub size: Option<u64>,
    pub error: Option<String>,
    /// Catalog id, for API-like requests (XHR and fetch).
    pub endpoint: Option<String>,
    /// The agent action during which the request started.
    pub action: Option<u64>,
    pub started: Instant,
    pub duration: Option<Duration>,
    pub cookies: Vec<(String, String)>,
    /// Sent while the user had the browser: kept without bodies (privacy mode).
    pub private: bool,
}

impl Entry {
    pub fn is_api_kind(&self) -> bool {
        matches!(self.kind.as_str(), "XHR" | "Fetch")
    }
}

#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    next_seq: u64,
    by_request: HashMap<String, u64>,
    early_cookies: HashMap<String, Vec<(String, String)>>,
    action: Option<u64>,
    next_action: u64,
    labels: VecDeque<(u64, String)>,
}

impl State {
    fn by_request(&mut self, request_id: &str) -> Option<&mut Entry> {
        let seq = *self.by_request.get(request_id)?;
        self.by_seq(seq)
    }

    fn by_seq(&mut self, seq: u64) -> Option<&mut Entry> {
        // Entries are in seq order with no gaps, so the position is computable.
        let first = self.entries.front()?.seq;
        let index = usize::try_from(seq.checked_sub(first)?).ok()?;
        self.entries.get_mut(index)
    }

    fn push(&mut self, entry: Entry) {
        if self.entries.len() >= CAPACITY
            && let Some(old) = self.entries.pop_front()
            && self.by_request.get(&old.request_id) == Some(&old.seq)
        {
            self.by_request.remove(&old.request_id);
        }
        self.by_request.insert(entry.request_id.clone(), entry.seq);
        self.entries.push_back(entry);
    }
}

/// A tab's network log. Cheap to clone; all clones share the log.
#[derive(Clone)]
pub struct NetworkLog {
    state: Arc<Mutex<State>>,
    catalog: Catalog,
    /// While set, requests are logged without bodies and taught to no catalog: the
    /// user has the browser (a handoff), and what they send is theirs (passwords, codes).
    private: Arc<AtomicBool>,
}

impl NetworkLog {
    /// Starts recording from `events`, which must be subscribed before `Network.enable`
    /// so no early request is missed. The task ends with the session's event stream.
    pub fn spawn(events: SessionEvents, session: Session, catalog: Catalog) -> Self {
        let log = Self {
            state: Arc::default(),
            catalog,
            private: Arc::default(),
        };
        tokio::spawn(log.clone().run(events, session));
        log
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Privacy mode: on while the user has the browser, off when they hand it back.
    pub fn set_private(&self, on: bool) {
        self.private.store(on, Ordering::Relaxed);
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Marks the start of an agent action: requests from now on are attributed to it.
    pub fn begin_action(&self) -> u64 {
        let mut state = self.lock();
        state.next_action += 1;
        let id = state.next_action;
        state.action = Some(id);
        id
    }

    /// Ends the action, names it (`click button "Accept"`) in the catalog of every
    /// endpoint it called, and describes the state-changing calls it made, so the agent
    /// learns which API a button uses as a side effect of pressing it.
    pub fn end_action(&self, id: u64, label: &str) -> Vec<String> {
        let called: Vec<(String, Option<u16>)> = {
            let mut state = self.lock();
            if state.action == Some(id) {
                state.action = None;
            }
            state.labels.push_back((id, label.to_owned()));
            if state.labels.len() > LABELS {
                state.labels.pop_front();
            }
            state
                .entries
                .iter()
                .filter(|e| e.action == Some(id))
                .filter_map(|e| e.endpoint.clone().map(|id| (id, e.status)))
                .collect()
        };
        let mut notes = Vec::new();
        let mut seen = Vec::new();
        for (endpoint_id, status) in called {
            if seen.contains(&endpoint_id) {
                continue;
            }
            seen.push(endpoint_id.clone());
            self.catalog.add_trigger(&endpoint_id, label);
            let Some(endpoint) = self.catalog.get(&endpoint_id) else {
                continue;
            };
            if endpoint.mutating && !endpoint.third_party && notes.len() < 3 {
                let status = status.map_or("pending".to_owned(), |s| s.to_string());
                notes.push(format!(
                    "this called the site's API: {} {}{} → {status} (endpoint {endpoint_id}; api_list shows it, api_call repeats it)",
                    endpoint.method, endpoint.template.origin, endpoint.template
                ));
            }
        }
        notes
    }

    pub fn label(&self, action: u64) -> Option<String> {
        self.lock()
            .labels
            .iter()
            .find(|(id, _)| *id == action)
            .map(|(_, label)| label.clone())
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.lock().entries.iter().cloned().collect()
    }

    pub fn entry(&self, seq: u64) -> Option<Entry> {
        self.lock().by_seq(seq).cloned()
    }

    /// The most recent successful API request to `origin`: the headers the site's own
    /// code sends there, for requests to URLs that aren't a known endpoint yet.
    pub fn latest_to(&self, origin: &str) -> Option<Entry> {
        self.lock()
            .entries
            .iter()
            .rev()
            .find(|e| {
                e.is_api_kind()
                    && e.status.is_some_and(|s| (200..300).contains(&s))
                    && url::Url::parse(&e.url)
                        .is_ok_and(|u| u.origin().ascii_serialization() == origin)
            })
            .cloned()
    }

    /// Headers for a request to `url`, borrowed from the page's latest successful API
    /// call to the same origin, and whether that call carried cookies. Sites expect the
    /// same CSRF and client headers on every call, so these usually work for endpoints
    /// the page hasn't called yet.
    pub fn replay_for(&self, url: &str) -> (Vec<ReplayHeader>, bool) {
        let Some(entry) = url::Url::parse(url)
            .ok()
            .and_then(|u| self.latest_to(&u.origin().ascii_serialization()))
        else {
            return (Vec::new(), false);
        };
        let headers = replay_headers(&entry.request_headers, &entry.cookies)
            .into_iter()
            // The body type belongs to that other request, not this one.
            .filter(|h| !h.name.eq_ignore_ascii_case("content-type"))
            .collect();
        (headers, !entry.cookies.is_empty())
    }

    async fn run(self, mut events: SessionEvents, session: Session) {
        while let Some(event) = events.recv().await {
            match event.method.as_str() {
                "Network.requestWillBeSent" => self.on_request(&event, &session),
                "Network.requestWillBeSentExtraInfo" => self.on_cookies(&event),
                "Network.responseReceived" => self.on_response(&event),
                "Network.loadingFinished" => self.on_finished(&event, &session),
                "Network.loadingFailed" => self.on_failed(&event),
                // The main frame arriving on a site is when earlier sessions' knowledge
                // of its API is wanted, before the page makes any call.
                "Page.frameNavigated" if event.params["frame"]["parentId"].is_null() => {
                    self.catalog
                        .visit(event.params["frame"]["url"].as_str().unwrap_or_default());
                }
                _ => {}
            }
        }
    }

    fn on_request(&self, event: &Event, session: &Session) {
        let p = &event.params;
        let request_id = str_of(&p["requestId"]);
        let request = &p["request"];
        let url = str_of(&request["url"]);
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        let mut state = self.lock();
        // A redirect reuses the request id: the previous hop ends here.
        if let Some(redirect) = p.get("redirectResponse")
            && let Some(previous) = state.by_request(&request_id)
        {
            previous.status = redirect["status"].as_u64().map(|s| s as u16);
            previous.duration = Some(previous.started.elapsed());
        }
        let seq = state.next_seq;
        state.next_seq += 1;
        let kind = p["type"].as_str().unwrap_or("Other").to_owned();
        let private = self.private.load(Ordering::Relaxed);
        let body = request["postData"]
            .as_str()
            .filter(|_| !private)
            .map(str::to_owned);
        let body_pending =
            !private && body.is_none() && request["hasPostData"].as_bool() == Some(true);
        let entry = Entry {
            seq,
            request_id: request_id.clone(),
            method: str_of(&request["method"]),
            url,
            kind,
            request_headers: header_pairs(&request["headers"]),
            request_body: body,
            status: None,
            content_type: None,
            response_headers: Vec::new(),
            response_body: None,
            body_truncated: false,
            flight: None,
            size: None,
            error: None,
            endpoint: None,
            action: state.action,
            started: Instant::now(),
            duration: None,
            cookies: state.early_cookies.remove(&request_id).unwrap_or_default(),
            private,
        };
        // The user's own requests teach the catalog nothing: they are theirs.
        let api = entry.is_api_kind() && !private;
        state.push(entry);
        drop(state);
        if !api {
            return;
        }
        let document_url = str_of(&p["documentURL"]);
        if body_pending {
            // Large bodies aren't inlined in the event; fetch it before classifying, since
            // a GraphQL operation is named in the body.
            let log = self.clone();
            let session = session.clone();
            tokio::spawn(async move {
                let body = session
                    .call(
                        "Network.getRequestPostData",
                        json!({ "requestId": request_id }),
                    )
                    .await
                    .ok()
                    .and_then(|r| r["postData"].as_str().map(str::to_owned));
                if let Some(entry) = log.lock().by_seq(seq) {
                    entry.request_body.clone_from(&body);
                }
                log.classify(seq, &document_url);
            });
        } else {
            self.classify(seq, &document_url);
        }
    }

    fn classify(&self, seq: u64, document_url: &str) {
        let Some((method, url, body, headers)) = self.lock().by_seq(seq).map(|e| {
            (
                e.method.clone(),
                e.url.clone(),
                e.request_body.clone(),
                e.request_headers.clone(),
            )
        }) else {
            return;
        };
        let id = self
            .catalog
            .classify(&method, &url, body.as_deref(), &headers, document_url);
        let sample = {
            let mut state = self.lock();
            let Some(entry) = state.by_seq(seq) else {
                return;
            };
            entry.endpoint.clone_from(&id);
            // The response may already be in (classification can wait on the body).
            entry
                .status
                .filter(|s| (200..300).contains(s))
                .map(|_| sample_of(entry))
        };
        if let (Some(id), Some(sample)) = (id, sample) {
            self.catalog.record_sample(&id, sample);
        }
    }

    fn on_cookies(&self, event: &Event) {
        let p = &event.params;
        let request_id = str_of(&p["requestId"]);
        let cookies: Vec<(String, String)> = p["associatedCookies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["blockedReasons"].as_array().is_none_or(|r| r.is_empty()))
            .map(|c| (str_of(&c["cookie"]["name"]), str_of(&c["cookie"]["value"])))
            .collect();
        let mut state = self.lock();
        let Some(entry) = state.by_request(&request_id) else {
            // Extra info can arrive before the request event itself.
            if state.early_cookies.len() >= EARLY_EXTRA {
                state.early_cookies.clear();
            }
            state.early_cookies.insert(request_id, cookies);
            return;
        };
        entry.cookies.clone_from(&cookies);
        let endpoint = entry.endpoint.clone();
        drop(state);
        if let Some(id) = endpoint {
            self.catalog.record_cookies(&id, &request_id, &cookies);
        }
    }

    fn on_response(&self, event: &Event) {
        let p = &event.params;
        let response = &p["response"];
        let status = response["status"].as_u64().map(|s| s as u16);
        let mut state = self.lock();
        let Some(entry) = state.by_request(&str_of(&p["requestId"])) else {
            return;
        };
        entry.status = status;
        entry.content_type = response["mimeType"].as_str().map(str::to_owned);
        entry.response_headers = header_pairs(&response["headers"]);
        let Some(id) = entry.endpoint.clone() else {
            return;
        };
        let content_type = entry.content_type.clone();
        // Only successful requests become the sample: a failed one (including the agent's
        // own malformed api_call) would teach the wrong request.
        let sample = status
            .filter(|s| (200..300).contains(s))
            .map(|_| sample_of(entry));
        drop(state);
        if let Some(status) = status {
            self.catalog.record_response(&id, status, content_type);
        }
        if let Some(sample) = sample {
            self.catalog.record_sample(&id, sample);
        }
    }

    fn on_finished(&self, event: &Event, session: &Session) {
        let p = &event.params;
        let request_id = str_of(&p["requestId"]);
        let size = p["encodedDataLength"].as_f64().unwrap_or(0.0);
        let mut state = self.lock();
        let Some(entry) = state.by_request(&request_id) else {
            return;
        };
        entry.size = Some(size as u64);
        entry.duration = Some(entry.started.elapsed());
        let wanted = entry.is_api_kind()
            && !entry.private
            && size <= MAX_FETCHED_BODY
            && entry
                .content_type
                .as_deref()
                // Opaque bytes too: some sites serve data (server-component streams) as
                // application/octet-stream; the body is sniffed below.
                .is_some_and(|ct| {
                    is_json(ct) || ct.starts_with("text/") || ct.contains("octet-stream")
                });
        if !wanted {
            return;
        }
        let (seq, endpoint) = (entry.seq, entry.endpoint.clone());
        let content_type = entry.content_type.clone().unwrap_or_default();
        drop(state);
        let log = self.clone();
        let session = session.clone();
        tokio::spawn(async move {
            let Ok(response) = session
                .call(
                    "Network.getResponseBody",
                    json!({ "requestId": request_id }),
                )
                .await
            else {
                return; // evicted, or the page navigated away
            };
            let raw = response["body"].as_str().unwrap_or_default();
            // Binary-typed responses arrive base64-encoded; only text is of use here.
            let decoded;
            let body = if response["base64Encoded"].as_bool() == Some(true) {
                use base64::Engine;
                let Some(text) = base64::engine::general_purpose::STANDARD
                    .decode(raw)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                else {
                    return;
                };
                decoded = text;
                decoded.as_str()
            } else {
                raw
            };
            // Checked before JSON: a one-row flight payload (`0:{...}`) would otherwise
            // pass as JSON behind a prefix.
            let flight = is_flight_response(&content_type, body)
                .then(|| decode(body))
                .flatten()
                .map(Arc::new);
            let value = match &flight {
                Some(flight) => Some(flight.document()),
                None => parse_json(body),
            };
            if let (Some(id), Some(value)) = (&endpoint, value) {
                log.catalog.record_body(id, Shape::of(&value));
            }
            if let Some(entry) = log.lock().by_seq(seq) {
                entry.body_truncated = body.len() > KEPT_BODY;
                entry.response_body = Some(truncate(body, KEPT_BODY).to_owned());
                entry.flight = flight;
            }
        });
    }

    fn on_failed(&self, event: &Event) {
        let p = &event.params;
        let mut state = self.lock();
        if let Some(entry) = state.by_request(&str_of(&p["requestId"])) {
            entry.error = Some(if p["canceled"].as_bool() == Some(true) {
                "canceled".to_owned()
            } else {
                str_of(&p["errorText"])
            });
            entry.duration = Some(entry.started.elapsed());
        }
    }
}

fn sample_of(entry: &Entry) -> Sample {
    Sample {
        request_id: entry.request_id.clone(),
        url: entry.url.clone(),
        headers: entry.request_headers.clone(),
        body: entry.request_body.clone(),
        cookies: entry.cookies.clone(),
    }
}

fn str_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn header_pairs(headers: &Value) -> Vec<(String, String)> {
    headers
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
        .collect()
}
