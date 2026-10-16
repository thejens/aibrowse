//! Agent-facing text for the network tools. Everything leaving this crate for the agent
//! goes through here, which is where redaction is applied.

use std::fmt::Write;

use serde_json::Value;

use crate::call::{ApiResponse, MAX_FETCHED_CHARS, ReplayHeader};
use crate::capture::{Entry, NetworkLog};
use crate::catalog::Endpoint;
use crate::payload::{parse_json, truncate};
use crate::redact;
use crate::rsc::{self, Flight};

const DETAIL_BODY: usize = 6_000;
/// The `api_call` pick that returns a decoded response's raw payload instead.
const RAW_PICK: &str = "$raw";
/// The raw text shown under a decoded flight response: enough to check the decoding.
const RAW_FLIGHT_BODY: usize = 1_500;

/// Which requests `network_log` lists.
#[derive(Debug, Default, Clone)]
pub struct LogFilter {
    /// Case-insensitive substring of the method and URL.
    pub text: Option<String>,
    /// Include documents, scripts, images, styles...; by default only XHR/fetch and
    /// page loads are listed.
    pub all: bool,
    pub limit: usize,
}

/// One line per request, newest last.
pub fn log(log: &NetworkLog, filter: &LogFilter) -> String {
    let needle = filter.text.as_deref().map(str::to_ascii_lowercase);
    let entries: Vec<Entry> = log
        .entries()
        .into_iter()
        .filter(|e| filter.all || e.is_api_kind() || e.kind == "Document")
        .filter(|e| {
            needle.as_ref().is_none_or(|n| {
                format!("{} {}", e.method, e.url)
                    .to_ascii_lowercase()
                    .contains(n)
            })
        })
        .collect();
    if entries.is_empty() {
        return "No matching requests recorded in this tab yet.".into();
    }
    let skipped = entries.len().saturating_sub(filter.limit);
    let mut out = String::new();
    if skipped > 0 {
        let _ = writeln!(out, "({skipped} earlier requests not shown)");
    }
    for entry in &entries[skipped..] {
        out.push_str(&line(log, entry));
        out.push('\n');
    }
    out.push_str("network_log id=<n> shows one request in full (credentials redacted).");
    out
}

fn line(log: &NetworkLog, e: &Entry) -> String {
    let status = match (&e.error, e.status) {
        (Some(error), _) => error.clone(),
        (None, Some(status)) => status.to_string(),
        (None, None) => "…".into(),
    };
    let mut line = format!("#{} {} {status} {}", e.seq, e.method, short_kind(&e.kind));
    if let Some(size) = e.size {
        let _ = write!(line, " {}", human_size(size));
    }
    if let Some(duration) = e.duration {
        let _ = write!(line, " {}ms", duration.as_millis());
    }
    let _ = write!(line, " {}", ellipsize(&redact::url(&e.url), 160));
    if let Some(id) = &e.endpoint {
        let _ = write!(line, " [{id}]");
    }
    if let Some(label) = e.action.and_then(|a| log.label(a)) {
        let _ = write!(line, " ← {label}");
    }
    if e.private {
        line.push_str(" (the user's)");
    }
    line
}

/// One request in full: headers, bodies, timing.
pub fn entry(log: &NetworkLog, e: &Entry) -> String {
    let mut out = line(log, e);
    out.push_str("\n\nRequest headers:\n");
    for (name, value) in redact::headers(&e.request_headers) {
        let _ = writeln!(out, "  {name}: {}", ellipsize(&value, 200));
    }
    if e.private {
        out.push_str("Sent while the user had the browser: bodies were not kept.\n");
    }
    if let Some(sent) = &e.request_body {
        let _ = writeln!(out, "Request body:\n{}", body(sent, DETAIL_BODY));
    }
    if e.status.is_some() {
        out.push_str("Response headers:\n");
        for (name, value) in redact::headers(&e.response_headers) {
            let _ = writeln!(out, "  {name}: {}", ellipsize(&value, 200));
        }
    }
    match &e.response_body {
        Some(received) if e.flight.is_some() => {
            let flight = e.flight.as_deref().expect("checked above");
            let mut doc = flight.document();
            redact::json(&mut doc);
            let decoded = serde_json::to_string(&doc).unwrap_or_default();
            let _ = writeln!(
                out,
                "Response body, {}:\n{}{}\nRaw response{}:\n{}",
                flight_note(flight),
                truncate(&decoded, DETAIL_BODY),
                if decoded.len() > DETAIL_BODY {
                    " …"
                } else {
                    ""
                },
                if e.body_truncated { " (truncated)" } else { "" },
                raw_flight(received, e.body_truncated, RAW_FLIGHT_BODY)
            );
        }
        Some(received) => {
            let _ = writeln!(
                out,
                "Response body{}:\n{}",
                if e.body_truncated { " (truncated)" } else { "" },
                body(received, DETAIL_BODY)
            );
        }
        None if e.is_api_kind() => out.push_str("(response body not captured)\n"),
        None => {}
    }
    out
}

/// The catalog, grouped by origin, API-like endpoints only unless `all`.
/// Documented endpoints not called yet that api_list shows without being asked.
const MAX_UNSEEN_DOCUMENTED: usize = 40;

/// `site` is the site of the page in front: without a filter, only its endpoints are
/// listed, and the other sites visited get one line (a session soon knows dozens of
/// endpoints across sites, most beside the point).
pub fn endpoints(
    endpoints: &[Endpoint],
    filter: Option<&str>,
    all: bool,
    site: Option<&str>,
) -> String {
    let needle = filter.map(str::to_ascii_lowercase);
    let mut shown: Vec<&Endpoint> = endpoints
        .iter()
        .filter(|e| all || e.is_api())
        .filter(|e| {
            needle.as_ref().is_none_or(|n| {
                let summary = e.documented.as_ref().and_then(|d| d.summary.as_deref());
                format!(
                    "{} {}{} {} {}",
                    e.method,
                    e.template.origin,
                    e.template,
                    summary.unwrap_or_default(),
                    e.site
                )
                .to_ascii_lowercase()
                .contains(n)
            })
        })
        .collect();
    // Other sites' endpoints, counted by site, when only this site's are listed.
    let mut elsewhere: Vec<(String, usize)> = Vec::new();
    if let Some(site) = site.filter(|_| needle.is_none() && !all) {
        shown.retain(|e| {
            if e.site == site {
                return true;
            }
            match elsewhere.iter_mut().find(|(s, _)| *s == e.site) {
                Some((_, n)) => *n += 1,
                None => elsewhere.push((e.site.clone(), 1)),
            }
            false
        });
        elsewhere.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    }
    let elsewhere_note = (!elsewhere.is_empty()).then(|| {
        let total: usize = elsewhere.iter().map(|(_, n)| n).sum();
        let sites: Vec<String> = elsewhere
            .iter()
            .take(5)
            .map(|(s, n)| format!("{s} {n}"))
            .collect();
        format!(
            "+ {total} endpoints on other sites visited ({}{}): filter=<site> lists them.",
            sites.join(", "),
            if elsewhere.len() > 5 { ", …" } else { "" }
        )
    });
    // A large API description would bury what the site was seen calling: past a few
    // dozen, documented endpoints not called yet are listed only when asked for.
    let documented_only =
        |e: &Endpoint| e.calls == 0 && e.learned.is_none() && e.documented.is_some();
    let unseen = shown.iter().filter(|e| documented_only(e)).count();
    let held_back = if needle.is_none() && !all && unseen > MAX_UNSEEN_DOCUMENTED {
        shown.retain(|e| !documented_only(e));
        unseen
    } else {
        0
    };
    // Grouped by origin (in order of first appearance), endpoints in discovery order.
    let mut origins: Vec<&str> = Vec::new();
    for e in &shown {
        if !origins.contains(&e.template.origin.as_str()) {
            origins.push(&e.template.origin);
        }
    }
    shown.sort_by_key(|e| origins.iter().position(|o| *o == e.template.origin));
    if shown.is_empty()
        && let (Some(site), Some(note)) = (site, &elsewhere_note)
    {
        return format!(
            "No API endpoints seen on {site} yet. Use the site normally (navigate, act): the calls its own code makes are recorded here.\n{note}"
        );
    }
    if shown.is_empty() {
        let hidden = endpoints.len();
        return if hidden > 0 && !all {
            format!(
                "No API endpoints match. {hidden} other requests (assets, third parties) are hidden; all=true shows them."
            )
        } else {
            "No API endpoints seen yet. Use the site normally (navigate, act): the calls its own code makes are recorded here.".into()
        };
    }
    // The same call to several hosts of one site (a search service's regional replicas
    // probed for latency, sharded CDNs) is one endpoint to the agent: listed once, at the
    // first host, with the others named.
    let mut replicas: Vec<(usize, Vec<&str>)> = Vec::new();
    let mut kept: Vec<&Endpoint> = Vec::new();
    for e in shown {
        let same = |k: &&Endpoint| {
            k.method == e.method
                && k.template.to_string() == e.template.to_string()
                && host_site(&k.template.origin) == host_site(&e.template.origin)
        };
        match kept.iter().position(same) {
            Some(first) => match replicas.iter_mut().find(|(i, _)| *i == first) {
                Some((_, hosts)) => hosts.push(host(&e.template.origin)),
                None => replicas.push((first, vec![host(&e.template.origin)])),
            },
            None => kept.push(e),
        }
    }
    let mut out = String::new();
    let mut origin = "";
    for (i, e) in kept.into_iter().enumerate() {
        if e.template.origin != origin {
            origin = &e.template.origin;
            let _ = writeln!(out, "{origin}");
        }
        let seen = match (&e.learned, e.calls, &e.documented) {
            (Some(_), 0, _) => "seen in an earlier session".to_owned(),
            (None, 0, Some(_)) => "documented, not called yet".to_owned(),
            _ => {
                let status = e.last_status.map_or("…".into(), |s| s.to_string());
                format!("{}× {status}", e.calls)
            }
        };
        let _ = write!(out, "  {} {} {} — {seen}", e.id, e.method, e.template);
        if let Some(summary) = e.documented.as_ref().and_then(|d| d.summary.as_deref()) {
            let _ = write!(out, ": {summary}");
        }
        if e.third_party {
            out.push_str(", another site");
        }
        if let Some(trigger) = e.triggers.last() {
            let _ = write!(out, " ← {trigger}");
        }
        if let Some((_, hosts)) = replicas.iter().find(|(first, _)| *first == i) {
            let _ = write!(out, " (also on {})", hosts.join(", "));
        }
        out.push('\n');
        if let Some(shape) = &e.response_shape {
            let _ = writeln!(out, "      → {}", ellipsize(&shape.render(2, 8), 240));
        }
    }
    if held_back > 0 {
        let _ = writeln!(
            out,
            "+ {held_back} more documented endpoints, not called yet: api_list filter=<word> finds them (paths and summaries), all=true lists them."
        );
    }
    if let Some(note) = &elsewhere_note {
        let _ = writeln!(out, "{note}");
    }
    out.push_str("api_list endpoint=<id> shows one in detail; api_call endpoint=<id> calls it from the page.");
    out
}

fn host(origin: &str) -> &str {
    let rest = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    rest.split([':', '/']).next().unwrap_or(rest)
}

fn host_site(origin: &str) -> String {
    crate::template::site(host(origin))
}

/// One endpoint in detail: parameters, the last request, shapes. `replayed` are the
/// headers `api_call` would send from what is known about the endpoint.
pub fn endpoint(e: &Endpoint, replayed: &[ReplayHeader]) -> String {
    let mut out = format!(
        "{} {} {}{}\n",
        e.id, e.method, e.template.origin, e.template
    );
    let seen = match (&e.learned, e.calls) {
        (Some(learned), 0) => format!(
            "Seen in an earlier session ({} calls, last {}), not yet in this one",
            learned.calls,
            days_ago(learned.last_seen)
        ),
        (None, 0) if e.documented.is_some() => "Documented, not called yet".to_owned(),
        _ => format!(
            "{} calls, last status {}",
            e.calls,
            e.last_status.map_or("…".into(), |s| s.to_string())
        ),
    };
    let _ = writeln!(
        out,
        "{seen}{}{}",
        if e.mutating {
            ", may change state (api_call checks it against the approval policy)"
        } else {
            ", read-only"
        },
        if e.third_party { ", third-party" } else { "" },
    );
    if let Some(doc) = &e.documented {
        let _ = writeln!(
            out,
            "Documented in {}{}",
            doc.spec,
            doc.summary
                .as_deref()
                .map(|s| format!(": {s}"))
                .unwrap_or_default()
        );
        if !doc.params.is_empty() {
            let params: Vec<String> = doc
                .params
                .iter()
                .map(|p| {
                    format!(
                        "{} ({}{})",
                        p.name,
                        p.location,
                        if p.required { ", required" } else { "" }
                    )
                })
                .collect();
            let _ = writeln!(out, "Documented parameters: {}", params.join(", "));
        }
        if let Some(body) = &doc.body {
            let _ = writeln!(out, "Takes a body: {body}");
        }
    }
    let params: Vec<&str> = e.template.params().collect();
    if !params.is_empty() {
        let _ = writeln!(out, "Path parameters: {}", params.join(", "));
    }
    if !e.query_keys.is_empty() {
        let keys: Vec<&str> = e.query_keys.iter().map(String::as_str).collect();
        let _ = writeln!(out, "Query parameters seen: {}", keys.join(", "));
    }
    if !e.triggers.is_empty() {
        let _ = writeln!(out, "Called when: {}", e.triggers.join("; "));
    }
    match &e.sample {
        Some(sample) => {
            let _ = writeln!(out, "Last successful request: {}", redact::url(&sample.url));
            if let Some(sent) = &sample.body {
                let _ = writeln!(out, "  body: {}", body(sent, 2_000));
            }
        }
        None => out.push_str("No successful request seen this session: api_call needs every path parameter, and any query and body.\n"),
    }
    let names: Vec<String> = replayed
        .iter()
        .map(|h| match &h.cookie {
            Some(cookie) => format!("{} (from cookie {cookie})", h.name),
            None => h.name.clone(),
        })
        .collect();
    if !names.is_empty() {
        let _ = writeln!(out, "  headers api_call sends: {}", names.join(", "));
    }
    if let Some(shape) = &e.request_shape {
        let _ = writeln!(
            out,
            "Request body shape: {}",
            ellipsize(&shape.render(4, 20), 1_500)
        );
    }
    if let Some(shape) = &e.response_shape {
        let _ = writeln!(
            out,
            "Response shape: {}",
            ellipsize(&shape.render(4, 20), 3_000)
        );
    }
    out
}

/// An `api_call` result. `pick` selects part of a JSON response (pick.rs: `data.items`,
/// `included.0`, `hits.*.{title,url}`).
pub fn response(response: &ApiResponse, pick: Option<&str>, max_chars: usize) -> String {
    if let Some(error) = &response.error {
        return format!(
            "The request failed in the page: {error}. It runs as a fetch from the active page, so that page must be on the site (or one allowed to call it). Fall back to the UI if it keeps failing."
        );
    }
    let mut out = format!("{} {}", response.status, response.status_text);
    if response.redirected {
        let _ = write!(out, " (redirected to {})", redact::url(&response.url));
    }
    let _ = write!(
        out,
        ", {}, {} chars",
        response.content_type, response.length
    );
    out.push('\n');
    let ok = (200..300).contains(&response.status);
    // An error body explains the failure; a pick into it would only hide that.
    let pick = pick.filter(|_| ok);
    let flight = rsc::is_flight_response(&response.content_type, &response.body)
        .then(|| rsc::decode(&response.body))
        .flatten();
    if let Some(flight) = &flight {
        let _ = writeln!(
            out,
            "Response {}. pick narrows it by dot path (\"value.0\", \"text\"); pick \"{RAW_PICK}\" returns the raw payload.",
            flight_note(flight)
        );
    }
    let json = match (&flight, pick) {
        (Some(_), Some(RAW_PICK)) => None,
        (Some(flight), _) => Some(flight.document()),
        (None, _) => parse_json(&response.body),
    };
    let body = match (json, pick) {
        (None, _) if flight.is_some() => raw_flight(
            &response.body,
            response.length > MAX_FETCHED_CHARS,
            usize::MAX,
        ),
        // Redacted before picking: a picked field keeps its path as its name, which
        // needn't say what it holds.
        (Some(mut value), Some(path)) => {
            redact::json(&mut value);
            match crate::pick(&value, path) {
                Some(picked) => serde_json::to_string(&picked).unwrap_or_default(),
                None => return format!("{out}Nothing at {path:?} in the response."),
            }
        }
        (Some(mut value), None) => {
            redact::json(&mut value);
            serde_json::to_string(&value).unwrap_or_default()
        }
        (None, _) => redact::body(&response.body),
    };
    out.push_str(truncate(&body, max_chars));
    if body.len() > max_chars {
        let _ = write!(
            out,
            "\n… truncated at {max_chars} of {} chars. Narrow it with pick: a dot path into the JSON, with * for every item and {{a,b}} for some fields (\"items.*.{{id,name}}\").",
            body.len()
        );
    }
    if !(200..300).contains(&response.status) {
        out.push_str("\nThe site rejected the call. Check the parameters against api_list, or fall back to the UI.");
    }
    out
}

/// Headers as shown to the agent or the user: credential-carrying ones redacted.
pub fn headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    redact::headers(headers)
}

/// JSON as shown to the agent or the user: credential fields (passwords, tokens, CSRF
/// values...) redacted in place, at any depth.
pub fn json(value: &mut Value) {
    redact::json(value);
}

/// Whether a field or query parameter with this name holds a credential.
pub fn is_secret_field(name: &str) -> bool {
    redact::is_secret_field(name)
}

/// A request URL as shown to the agent or the user: credentials in the query redacted.
pub fn url(raw: &str) -> String {
    ellipsize(&redact::url(raw), 300)
}

/// A body as shown to the agent or the user: credentials redacted, size-capped.
pub fn body(body: &str, max: usize) -> String {
    let redacted = redact::body(body);
    let mut text = truncate(&redacted, max).to_owned();
    if redacted.len() > max {
        text.push_str(" …");
    }
    text
}

/// A raw flight payload, rows redacted, size-capped. A payload that was cut short loses
/// its last, partial row: half a row isn't JSON, so it couldn't be redacted.
fn raw_flight(text: &str, cut: bool, max: usize) -> String {
    let complete = match text.rfind('\n') {
        Some(end) if cut => &text[..end],
        _ => text,
    };
    let redacted = redact::flight(complete);
    let mut shown = truncate(&redacted, max).to_owned();
    if redacted.len() > max || cut {
        shown.push_str(" …");
    }
    shown
}

/// How a flight response was turned into the JSON shown.
fn flight_note(flight: &Flight) -> String {
    format!(
        "decoded from a React Server Components (flight) payload of {} rows{} (\"text\" is what it renders, \"value\" its data with references resolved)",
        flight.rows,
        if flight.truncated {
            ", incomplete or cut to bound its size"
        } else {
            ""
        }
    )
}

fn short_kind(kind: &str) -> &str {
    match kind {
        "XHR" => "xhr",
        "Fetch" => "fetch",
        "Document" => "doc",
        "Script" => "js",
        "Stylesheet" => "css",
        "Image" => "img",
        other => other,
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1_024 => format!("{bytes}B"),
        1_024..1_048_576 => format!("{:.1}kB", bytes as f64 / 1_024.0),
        _ => format!("{:.1}MB", bytes as f64 / 1_048_576.0),
    }
}

/// `today`, `1 day ago`, `12 days ago`, from Unix seconds.
fn days_ago(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    match (now - unix).max(0) / 86_400 {
        0 => "today".into(),
        1 => "1 day ago".into(),
        days => format!("{days} days ago"),
    }
}

fn ellipsize(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    format!("{}…", truncate(text, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn one_call_to_several_hosts_is_listed_once() {
        let catalog = crate::Catalog::default();
        let page = "https://hn.algolia.com/";
        for host in ["t32-usw-1", "c3-de-1", "t5-eu-1"] {
            let url = format!("https://{host}.algolia.net/1/isalive");
            let id = catalog.classify("GET", &url, None, &[], page).unwrap();
            catalog.record_response(&id, 200, Some("application/json".into()));
        }
        let query = "https://dsn.algolia.net/1/indexes/Item_dev/query";
        let id = catalog
            .classify("POST", query, Some("{}"), &[], page)
            .unwrap();
        catalog.record_response(&id, 200, Some("application/json".into()));
        let listed = endpoints(&catalog.endpoints(), None, false, None);
        assert_eq!(listed.matches("/1/isalive").count(), 1, "{listed}");
        assert!(
            listed.contains("(also on c3-de-1.algolia.net, t5-eu-1.algolia.net)"),
            "{listed}"
        );
        assert!(
            listed.contains("POST /1/indexes/Item_dev/query"),
            "{listed}"
        );
    }

    #[test]
    fn lists_the_current_sites_endpoints_and_counts_the_rest() {
        let catalog = crate::Catalog::default();
        for (url, page) in [
            (
                "https://github.com/tokio-rs/tokio/latest-commit",
                "https://github.com/tokio-rs/tokio",
            ),
            (
                "https://web-api.ikea.com/mp/v2/se/en",
                "https://www.ikea.com/se/en/",
            ),
            (
                "https://www.ikea.com/se/en/stores.json",
                "https://www.ikea.com/se/en/",
            ),
        ] {
            let id = catalog.classify("GET", url, None, &[], page).unwrap();
            catalog.record_response(&id, 200, Some("application/json".into()));
        }
        let listed = endpoints(&catalog.endpoints(), None, false, Some("github.com"));
        assert!(listed.contains("/tokio-rs/tokio/latest-commit"), "{listed}");
        assert!(!listed.contains("/mp/v2"), "{listed}");
        assert!(
            listed.contains("+ 2 endpoints on other sites visited (ikea.com 2)"),
            "{listed}"
        );
        // A filter looks everywhere, sites included.
        let ikea = endpoints(
            &catalog.endpoints(),
            Some("ikea.com"),
            false,
            Some("github.com"),
        );
        assert!(
            ikea.contains("/mp/v2") && ikea.contains("stores.json"),
            "{ikea}"
        );
        let none = endpoints(&catalog.endpoints(), None, false, Some("example.org"));
        assert!(
            none.starts_with("No API endpoints seen on example.org yet"),
            "{none}"
        );
    }

    #[test]
    fn picks_and_redacts_responses() {
        let r = ApiResponse {
            status: 200,
            status_text: "OK".into(),
            content_type: "application/json".into(),
            body: json!({"data": {"items": [{"id": 1, "token": "t"}]}}).to_string(),
            length: 40,
            ..ApiResponse::default()
        };
        let text = response(&r, Some("data.items.0"), 1000);
        assert!(text.ends_with(r#"{"id":1,"token":"[redacted]"}"#), "{text}");
        assert!(response(&r, Some("data.nope"), 1000).contains("Nothing at"));
        assert!(response(&r, None, 10).contains("truncated at 10"));
    }

    #[test]
    fn decodes_server_component_responses() {
        let r = ApiResponse {
            status: 200,
            status_text: "OK".into(),
            content_type: "text/x-component".into(),
            body: concat!(
                "0:{\"a\":\"$@1\",\"f\":\"\",\"b\":\"build\"}\n",
                "1:{\"items\":[{\"id\":\"7101\",\"name\":\"Grace Hopper\"}],\"csrfToken\":\"s3cret\"}\n",
            )
            .into(),
            length: 100,
            ..ApiResponse::default()
        };
        let text = response(&r, None, 10_000);
        assert!(
            text.contains("decoded from a React Server Components"),
            "{text}"
        );
        assert!(text.contains(r#""name":"Grace Hopper""#), "{text}");
        assert!(!text.contains("s3cret"), "{text}");
        let text = response(&r, Some("value.a.items.0.id"), 10_000);
        assert!(text.ends_with(r#""7101""#), "{text}");
        let text = response(&r, Some(RAW_PICK), 10_000);
        assert!(text.contains("1:{\"csrfToken\":\"[redacted]\""), "{text}");
        assert!(!text.contains("s3cret"), "{text}");
    }
}
