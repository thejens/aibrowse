//! One line of a trace: what a tool call was asked, what it did and what it saw, with
//! secrets removed before anything is written. Everything recorded passes through here.

use std::time::{SystemTime, UNIX_EPOCH};

use aibrowse_browser::ActionResult;
use aibrowse_network::REDACTED;
use aibrowse_network::render;
use serde::Serialize;
use serde_json::Value;

use super::notes::{ApiTrace, Human, NavigatorTrace};

/// Result text kept inline in an event. Outlines are kept whole in their own files;
/// past this, a result is mostly page text the outline or screenshot already shows.
pub const MAX_RESULT_CHARS: usize = 8_000;
/// What replaces text typed into a password (or other secret) field.
pub const SECRET_TEXT: &str = "[redacted: secret field]";

/// A traced tool call, written as one JSON line to `events.jsonl`.
#[derive(Debug, Default, Serialize)]
pub struct Event {
    /// Order of the call in the session; lines can land out of order, `seq` doesn't.
    pub seq: u64,
    /// When the call started, RFC 3339 in UTC.
    pub time: String,
    pub tool: String,
    /// The arguments as given, with credentials and secret-field text redacted.
    pub args: Value,
    pub duration_ms: u64,
    pub ok: bool,
    /// The text the agent got back, truncated to [`MAX_RESULT_CHARS`].
    pub result: String,
    #[serde(skip_serializing_if = "is_false")]
    pub result_truncated: bool,
    /// The active page after the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionResult>,
    /// Path of the JPEG after the call, relative to the trace directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// Path of the page outline the call produced, relative to the trace directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    /// The navigator's run for each goal (or part of one) the call pursued.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub navigator: Vec<NavigatorTrace>,
    /// Approvals and handoffs: requested, approved, denied, done, cancelled.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub human: Vec<Human>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub api: Vec<ApiTrace>,
    /// Why the page wasn't captured after the call, when it wasn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_skipped: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// The requests the page made during a call.
#[derive(Debug, Default, Serialize)]
pub struct Network {
    /// Page loads and API calls (XHR/fetch), in order.
    pub requests: Vec<Request>,
    /// Scripts, images, styles, fonts and the like: counted, not listed.
    pub other: usize,
}

#[derive(Debug, Serialize)]
pub struct Request {
    /// Position in the tab's log (what `network_log id=` takes).
    pub seq: u64,
    pub method: String,
    /// Redacted like `network_log` shows it.
    pub url: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Requests listed per call; a page load can make hundreds.
const MAX_REQUESTS: usize = 100;

impl Network {
    pub fn from_entries(entries: &[aibrowse_network::Entry]) -> Self {
        let mut network = Network::default();
        for e in entries {
            if !(e.is_api_kind() || e.kind == "Document") || network.requests.len() >= MAX_REQUESTS
            {
                network.other += 1;
                continue;
            }
            network.requests.push(Request {
                seq: e.seq,
                method: e.method.clone(),
                url: render::url(&e.url),
                kind: e.kind.clone(),
                status: e.status,
                endpoint: e.endpoint.clone(),
                ms: e.duration.map(|d| d.as_millis() as u64),
                error: e.error.clone(),
            });
        }
        network
    }
}

/// The arguments of a tool call as they may be written down: credential fields and
/// URL query values redacted, and the text of `fill`/`type` replaced when it goes into
/// a secret field (`secret_text`, decided by looking at the field itself).
pub fn redact_args(tool: &str, args: &Value, secret_text: bool) -> Value {
    let mut args = args.clone();
    render::json(&mut args);
    let Value::Object(map) = &mut args else {
        return args;
    };
    for (key, value) in map.iter_mut() {
        match (key.as_str(), &*value) {
            ("url" | "done_url", Value::String(url)) => *value = Value::String(render::url(url)),
            ("body", Value::String(body)) => *value = Value::String(render::body(body, 4_000)),
            ("query", Value::Object(_)) => redact_query(value),
            _ => {}
        }
    }
    let typing = matches!(
        map.get("action").and_then(Value::as_str),
        Some("fill" | "type")
    );
    if tool == "act" && typing && secret_text && map.contains_key("text") {
        map.insert("text".into(), Value::String(SECRET_TEXT.into()));
    }
    args
}

/// `api_call` query parameters are named like URL ones; redact them by the same rule.
fn redact_query(query: &mut Value) {
    if let Value::Object(params) = query {
        for (name, value) in params.iter_mut() {
            if render::is_secret_field(name) && !value.is_null() {
                *value = Value::String(REDACTED.into());
            }
        }
    }
}

/// Whether a call types text whose secrecy depends on the field it goes into.
pub fn types_text(tool: &str, args: &Value) -> bool {
    tool == "act"
        && matches!(args["action"].as_str(), Some("fill" | "type"))
        && args.get("text").is_some()
}

/// Cuts `text` to at most `max` characters; returns whether it was cut.
pub fn truncate(text: &mut String, max: usize) -> bool {
    match text.char_indices().nth(max) {
        Some((cut, _)) => {
            text.truncate(cut);
            text.push_str(" …");
            true
        }
        None => false,
    }
}

/// `time` as RFC 3339 in UTC with milliseconds, e.g. `2026-10-06T09:14:03.120Z`.
pub fn rfc3339(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (date, clock) = civil(since.as_secs());
    format!("{date}T{clock}.{:03}Z", since.subsec_millis())
}

/// `time` as a directory name that sorts by time and is safe on every filesystem,
/// e.g. `2026-10-06T09-14-03Z`.
pub fn dir_stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (date, clock) = civil(secs);
    format!("{date}T{}Z", clock.replace(':', "-"))
}

/// Unix seconds to ("YYYY-MM-DD", "HH:MM:SS") in UTC, by Howard Hinnant's
/// days-to-civil algorithm (proleptic Gregorian calendar).
fn civil(secs: u64) -> (String, String) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}:{:02}", rem / 3_600, rem % 3_600 / 60, rem % 60),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    #[test]
    fn password_text_is_never_written() {
        let args = json!({ "action": "fill", "ref": "e12", "text": "hunter2" });
        let line = serde_json::to_string(&redact_args("act", &args, true)).unwrap();
        assert!(!line.contains("hunter2"), "{line}");
        assert!(line.contains(SECRET_TEXT));
        let typed = json!({ "action": "type", "text": "hunter2" });
        let line = serde_json::to_string(&redact_args("act", &typed, true)).unwrap();
        assert!(!line.contains("hunter2"), "{line}");
        // Text for ordinary fields is the point of the trace, so it stays.
        let search = json!({ "action": "fill", "ref": "e3", "text": "rust traces" });
        assert_eq!(redact_args("act", &search, false)["text"], "rust traces");
        assert!(types_text("act", &args) && !types_text("act", &json!({"action": "click"})));
    }

    #[test]
    fn credentials_in_arguments_are_redacted() {
        let args = json!({
            "url": "https://x.com/api?access_token=abc&page=2",
            "query": { "csrf": "t0k3n", "q": "shoes" },
            "body": { "password": "hunter2", "name": "Ada" },
        });
        let redacted = redact_args("api_call", &args, false);
        let line = serde_json::to_string(&redacted).unwrap();
        for secret in ["abc", "t0k3n", "hunter2"] {
            assert!(!line.contains(secret), "{secret} leaked: {line}");
        }
        assert_eq!(redacted["query"]["q"], "shoes");
        assert_eq!(redacted["body"]["name"], "Ada");
        let form = json!({ "body": "user=ada&password=hunter2" });
        assert!(
            !redact_args("api_call", &form, false)["body"]
                .as_str()
                .unwrap()
                .contains("hunter2")
        );
    }

    #[test]
    fn events_serialize_compactly() {
        let event = Event {
            seq: 3,
            time: rfc3339(UNIX_EPOCH + Duration::from_millis(1_791_278_043_120)),
            tool: "observe".into(),
            args: json!({}),
            ok: true,
            result: "outline".into(),
            ..Event::default()
        };
        let line = serde_json::to_string(&event).unwrap();
        assert_eq!(
            line,
            r#"{"seq":3,"time":"2026-10-06T09:14:03.120Z","tool":"observe","args":{},"duration_ms":0,"ok":true,"result":"outline"}"#
        );
    }

    #[test]
    fn times_are_utc_calendar_dates() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(dir_stamp(leap), "2000-02-29T00-00-00Z");
    }

    #[test]
    fn truncation_respects_characters() {
        let mut text = "ééééé".to_owned();
        assert!(truncate(&mut text, 2));
        assert_eq!(text, "éé …");
        let mut short = "ok".to_owned();
        assert!(!truncate(&mut short, 2));
    }
}
