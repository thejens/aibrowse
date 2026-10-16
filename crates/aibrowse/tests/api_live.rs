//! End-to-end API discovery: the agent uses a page, learns the API its code calls, and
//! calls it directly. Ignored by default; run with `make test-live`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::json;

mod common;
use common::{Client, find_ref, has_jev_key, set_policy};

/// A LinkedIn-shaped app: the page loads invitations with fetch and accepts them with a
/// POST, both sending the double-submit CSRF header (`csrf-token` = the JSESSIONID cookie
/// without quotes) that the server checks.
const APP: &str = r#"<!doctype html><title>Invitations</title><h1>Invitations</h1><ul id="list"></ul>
<script>
const csrf = () => document.cookie.match(/JSESSIONID="?([^";]+)/)[1];
async function accept(id, li) {
  const r = await fetch('/api/invitations/' + id + '?action=accept', { method: 'POST',
    headers: { 'csrf-token': csrf(), 'content-type': 'application/json' },
    body: JSON.stringify({ invitationId: id }) });
  if (r.ok) li.textContent = 'Accepted ' + id;
}
fetch('/api/invitations?start=0&count=10', { headers: { 'csrf-token': csrf(), accept: 'application/json' } })
  .then((r) => r.json())
  .then((data) => data.elements.forEach((inv) => {
    const li = document.createElement('li');
    li.textContent = inv.name + ' ';
    const b = document.createElement('button');
    b.textContent = 'Accept';
    b.onclick = () => accept(inv.id, li);
    li.append(b);
    document.getElementById('list').append(li);
  }));
</script>"#;

struct Request {
    line: String,
    headers: String,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        data.extend_from_slice(&buf[..n]);
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        data.extend_from_slice(&buf[..n]);
    }
    let (line, headers) = head.split_once("\r\n").unwrap_or((&head, ""));
    Some(Request {
        line: line.to_owned(),
        headers: headers.to_ascii_lowercase(),
        body: String::from_utf8_lossy(&data[head_end..head_end + length]).to_string(),
    })
}

/// Whether the request carries the CSRF header matching its JSESSIONID cookie.
fn csrf_ok(headers: &str) -> bool {
    let cookie = headers
        .split("jsessionid=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next());
    let header = headers
        .lines()
        .find_map(|l| l.strip_prefix("csrf-token:"))
        .map(str::trim);
    cookie.is_some() && cookie == header
}

/// Serves the app; returns the port and the invitation ids accepted so far.
fn serve() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let accepted = Arc::clone(&state);
            std::thread::spawn(move || {
                let Some(request) = read_request(&mut stream) else {
                    return;
                };
                let path = request.line.split(' ').nth(1).unwrap_or("/").to_owned();
                let cookie = "set-cookie: JSESSIONID=\"ajax:4061995172801\"; Path=/\r\n";
                let (status, content_type, extra, body) = if path.starts_with("/app") {
                    ("200 OK", "text/html", cookie, APP.to_owned())
                } else if path == "/home" {
                    // A page that calls no API.
                    let home = "<!doctype html><title>Home</title><h1>Home</h1>";
                    ("200 OK", "text/html", cookie, home.to_owned())
                } else if path == "/api/accepted" {
                    let list = accepted.lock().unwrap().clone();
                    (
                        "200 OK",
                        "application/json",
                        "",
                        json!({ "accepted": list }).to_string(),
                    )
                } else if !csrf_ok(&request.headers) {
                    (
                        "403 Forbidden",
                        "application/json",
                        "",
                        r#"{"error":"CSRF check failed"}"#.to_owned(),
                    )
                } else if request.line.starts_with("GET /api/invitations?") {
                    let elements = json!([
                        { "id": "7101", "name": "Grace Hopper" },
                        { "id": "7102", "name": "Alan Turing" },
                        { "id": "7103", "name": "Barbara Liskov" },
                    ]);
                    let count: usize = path
                        .split("count=")
                        .nth(1)
                        .and_then(|c| c.split('&').next()?.parse().ok())
                        .unwrap_or(10);
                    let elements: Vec<_> = elements
                        .as_array()
                        .unwrap()
                        .iter()
                        .take(count)
                        .cloned()
                        .collect();
                    (
                        "200 OK",
                        "application/json",
                        "",
                        json!({ "elements": elements, "paging": { "count": count } }).to_string(),
                    )
                } else if request.line.starts_with("POST /api/invitations/")
                    && path.ends_with("?action=accept")
                {
                    let id = path
                        .trim_start_matches("/api/invitations/")
                        .trim_end_matches("?action=accept")
                        .to_owned();
                    let body_id = serde_json::from_str::<serde_json::Value>(&request.body)
                        .ok()
                        .and_then(|b| b["invitationId"].as_str().map(str::to_owned));
                    if body_id.as_deref() == Some(id.as_str()) {
                        accepted.lock().unwrap().push(id.clone());
                        (
                            "200 OK",
                            "application/json",
                            "",
                            json!({ "value": { "accepted": id } }).to_string(),
                        )
                    } else {
                        (
                            "400 Bad Request",
                            "application/json",
                            "",
                            r#"{"error":"body does not match"}"#.to_owned(),
                        )
                    }
                } else {
                    ("404 Not Found", "text/plain", "", "not found".to_owned())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (port, accepted)
}

/// The id of the endpoint on the api_list line containing `needle`.
fn endpoint_id(listing: &str, needle: &str) -> String {
    let line = listing
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not in:\n{listing}"));
    line.split_whitespace().next().unwrap().to_owned()
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn learns_and_calls_the_sites_api() {
    // Mechanics only: approvals are covered below.
    set_policy("test-api", r#"{"confirm": "off"}"#);
    let (port, accepted) = serve();
    let mut c = Client::spawn("test-api", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/app") }),
    );

    // Clicking Accept tells the agent which API call the button made.
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "button \"Accept\"") }),
    );
    assert!(out.contains("click button \"Accept\""), "{out}");
    assert!(
        out.contains("this called the site's API: POST")
            && out.contains("/api/invitations/{invitations_id}?action=accept → 200"),
        "{out}"
    );
    assert_eq!(*accepted.lock().unwrap(), ["7101"]);

    let listing = c.ok("api_list", json!({}));
    let list = endpoint_id(&listing, "GET /api/invitations");
    let accept = endpoint_id(
        &listing,
        "POST /api/invitations/{invitations_id}?action=accept",
    );
    assert!(listing.contains("← click button \"Accept\""), "{listing}");
    assert!(
        listing.contains("{elements: [{id: str, name: str}], paging: {count: num}}"),
        "{listing}"
    );

    // Details show where the CSRF header comes from, never its value.
    let detail = c.ok("api_list", json!({ "endpoint": accept }));
    assert!(
        detail.contains("Path parameters: invitations_id"),
        "{detail}"
    );
    assert!(
        detail.contains("csrf-token (from cookie JSESSIONID)"),
        "{detail}"
    );
    assert!(!detail.contains("ajax:4061995172801"), "{detail}");

    let log = c.ok("network_log", json!({ "filter": "action=accept" }));
    assert!(
        log.contains(&format!("[{accept}] ← click button \"Accept\"")),
        "{log}"
    );
    let seq: u64 = log
        .lines()
        .next()
        .unwrap()
        .trim_start_matches('#')
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let entry = c.ok("network_log", json!({ "id": seq }));
    assert!(
        entry.contains("csrf-token: [redacted]") && !entry.contains("ajax:4061995172801"),
        "{entry}"
    );

    // The site rotates its session token: replays follow the cookie.
    c.ok(
        "evaluate",
        json!({ "expression": "document.cookie = 'JSESSIONID=\"ajax:9038826450117\"; path=/'" }),
    );
    let out = c.ok(
        "api_call",
        json!({ "endpoint": accept, "path": { "invitations_id": "7103" }, "body": { "invitationId": "7103" } }),
    );
    assert!(out.starts_with("200"), "{out}");
    assert!(out.contains(r#"{"value":{"accepted":"7103"}}"#), "{out}");
    assert_eq!(*accepted.lock().unwrap(), ["7101", "7103"]);

    let out = c.ok(
        "api_call",
        json!({ "endpoint": list, "query": { "count": "2" }, "pick": "elements.1.name" }),
    );
    assert!(out.ends_with("\"Alan Turing\""), "{out}");
    let out = c.ok(
        "api_call",
        json!({ "endpoint": list, "query": { "count": "2" }, "pick": "elements.*.{name}" }),
    );
    assert!(
        out.ends_with(r#"[{"name":"Grace Hopper"},{"name":"Alan Turing"}]"#),
        "{out}"
    );

    let out = c.ok(
        "api_call",
        json!({ "url": format!("http://localhost:{port}/api/accepted") }),
    );
    assert!(out.contains(r#"{"accepted":["7101","7103"]}"#), "{out}");

    // A rejected call says so, and doesn't replace the endpoint's good sample.
    let out = c.ok(
        "api_call",
        json!({ "endpoint": accept, "path": { "invitations_id": "7102" }, "body": { "invitationId": "0" } }),
    );
    assert!(out.starts_with("400") && out.contains("rejected"), "{out}");
    let detail = c.ok("api_list", json!({ "endpoint": accept }));
    assert!(!detail.contains("invitations/7102"), "{detail}");

    let (err, out) = c.call("api_call", json!({ "endpoint": "a99" }));
    assert!(err && out.contains("unknown endpoint"), "{out}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn state_changing_api_calls_need_approval() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    // Its own profile: tests run in parallel, and a profile holds one browser.
    set_policy("test-api-gate", r#"{"confirm": "consequential"}"#);
    let (port, accepted) = serve();
    let mut c = Client::spawn("test-api-gate", true);
    c.ok(
        "session_open",
        json!({ "url": format!("http://localhost:{port}/app"), "attended": false }),
    );
    // Reads don't need anyone's approval; the list call happened on load.
    let listing = c.ok("api_list", json!({}));
    let list = endpoint_id(&listing, "GET /api/invitations");
    assert!(
        c.ok("api_call", json!({ "endpoint": list }))
            .starts_with("200")
    );

    let accept = json!({ "url": format!("http://localhost:{port}/api/invitations/7102?action=accept"), "body": { "invitationId": "7102" } });
    let out = c.ok("api_call", accept.clone());
    assert!(out.contains("no user is present"), "{out}");
    assert!(accepted.lock().unwrap().is_empty());

    let mut confirmed = accept;
    confirmed["confirm"] = json!(true);
    let out = c.ok("api_call", confirmed);
    assert!(out.starts_with("200"), "{out}");
    assert_eq!(*accepted.lock().unwrap(), ["7102"]);
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_new_session_knows_the_api_learned_earlier() {
    set_policy("test-kb", r#"{"confirm": "off"}"#);
    let (port, accepted) = serve();
    let kb = tempfile::tempdir().unwrap();
    let kb_path = kb.path().join("kb.sqlite");
    let env = [("AIBROWSE_KB", kb_path.to_str().unwrap())];

    // Session 1 learns the API by using the page.
    let mut c = Client::spawn_with("test-kb", true, &env);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/app") }),
    );
    c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "button \"Accept\"") }),
    );
    assert_eq!(*accepted.lock().unwrap(), ["7101"]);
    c.ok("session_close", json!({}));
    drop(c);

    // Session 2 knows it before the site makes a single call.
    let mut c = Client::spawn_with("test-kb", true, &env);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/home") }),
    );
    let listing = c.ok("api_list", json!({}));
    let accept = endpoint_id(
        &listing,
        "POST /api/invitations/{invitations_id}?action=accept — seen in an earlier session",
    );
    let list = endpoint_id(
        &listing,
        "GET /api/invitations — seen in an earlier session",
    );
    // The trigger keeps the element's role, not its (possibly personal) name.
    assert!(
        listing.contains("← click button") && !listing.contains("\"Accept\""),
        "{listing}"
    );
    let detail = c.ok("api_list", json!({ "endpoint": accept }));
    assert!(
        detail.contains("Seen in an earlier session (1 calls")
            && detail.contains("csrf-token (from cookie JSESSIONID)"),
        "{detail}"
    );

    // Without a sample, path parameters are required; with them the call works, its
    // CSRF header filled from the cookie as learned.
    let (err, out) = c.call(
        "api_call",
        json!({ "endpoint": accept, "body": { "invitationId": "7102" } }),
    );
    assert!(err && out.contains("invitations_id"), "{out}");
    let out = c.ok(
        "api_call",
        json!({ "endpoint": accept, "path": { "invitations_id": "7102" }, "body": { "invitationId": "7102" } }),
    );
    assert!(out.starts_with("200"), "{out}");
    assert_eq!(*accepted.lock().unwrap(), ["7101", "7102"]);

    // Seen live again, an endpoint keeps its id rather than appearing twice.
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/app") }),
    );
    let listing = c.ok("api_list", json!({}));
    assert_eq!(
        listing.matches("GET /api/invitations —").count(),
        1,
        "{listing}"
    );
    assert!(
        listing.contains(&format!("{list} GET /api/invitations — 1× 200")),
        "{listing}"
    );
    c.ok("session_close", json!({}));
}
