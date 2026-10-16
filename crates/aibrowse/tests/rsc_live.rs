//! End to end with a React Server Components site: the page loads data and calls a
//! server action as flight (`text/x-component`) responses, and the agent gets them
//! decoded. Ignored by default; run with `make test-live`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::json;

mod common;
use common::{Client, set_policy};

/// What a Next.js App Router page does: fetch the next page of a server-rendered list
/// (`RSC: 1`), and call a server action (`Next-Action: <id>`, arguments as JSON).
const APP: &str = r#"<!doctype html><title>Invitations</title><h1>Invitations</h1>
<script>
const act = (id, args) => fetch('/rsc/actions', { method: 'POST',
  headers: { 'Next-Action': id, Accept: 'text/x-component', 'content-type': 'text/plain;charset=UTF-8' },
  body: JSON.stringify(args) });
// Two server actions, both posting to the page's own URL: accept one, ignore another.
fetch('/rsc/invitations?start=0', { headers: { RSC: '1' } })
  .then(() => act('7f3a9c0d', [{ invitationId: '7101' }]))
  .then(() => act('a1b2c3d4', [{ invitationId: '7105' }]));
</script>"#;

/// An element tree: a client component (import row 1) per invitation with a server
/// action prop (row 2), and a footer outlined to row 3.
const LIST: &str = r#"1:I["(app-pages-browser)/./components/Invitation.tsx",["static/chunks/inv-2b1.js"],"default"]
2:{"id":"7f3a9c0d","bound":null}
0:["$","ul",null,{"className":"list","children":[["$","$L1","7101",{"invitation":{"id":"7101","name":"Grace Hopper"},"onAccept":"$h2"}],["$","$L1","7102",{"invitation":{"id":"7102","name":"Alan Turing"},"onAccept":"$h2"}],["$","li",null,{"children":"$L3"}]]}]
3:["$","span",null,{"children":["Showing ",2," of ",5]}]
"#;

fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        data.extend_from_slice(&buf[..n]);
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        data.extend_from_slice(&buf[..n]);
    }
    let (line, headers) = head.split_once("\r\n").unwrap_or((&head, ""));
    let body = String::from_utf8_lossy(&data[head_end..head_end + length]).to_string();
    Some((line.to_owned(), headers.to_owned(), body))
}

/// Serves the app; returns the port and the invitation ids the action accepted.
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
                let Some((line, headers, body)) = read_request(&mut stream) else {
                    return;
                };
                let flight = "text/x-component";
                let (status, content_type, body) = if line.starts_with("get /app") {
                    ("200 OK", "text/html", APP.to_owned())
                } else if line.starts_with("get /rsc/invitations") && headers.contains("rsc: 1") {
                    ("200 OK", flight, LIST.to_owned())
                } else if line.starts_with("post /rsc/actions")
                    && headers.contains("next-action: a1b2c3d4")
                {
                    let id = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|args| args[0]["invitationId"].as_str().map(str::to_owned))
                        .unwrap_or_default();
                    accepted.lock().unwrap().push(format!("ignored {id}"));
                    let payload = format!("0:{{\"a\":\"$@1\"}}\n1:{{\"ignored\":\"{id}\"}}\n");
                    ("200 OK", flight, payload)
                } else if line.starts_with("post /rsc/actions")
                    && headers.contains("next-action: 7f3a9c0d")
                {
                    // Only the action the header names runs, as in Next.js.
                    let id = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|args| args[0]["invitationId"].as_str().map(str::to_owned))
                        .unwrap_or_default();
                    accepted.lock().unwrap().push(id.clone());
                    let payload = format!(
                        "0:{{\"a\":\"$@1\",\"f\":\"\",\"b\":\"build-7\"}}\n1:{{\"accepted\":\"{id}\",\"pending\":\"$n4\"}}\n"
                    );
                    ("200 OK", flight, payload)
                } else {
                    ("404 Not Found", "text/plain", "not found".to_owned())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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
fn decodes_server_component_responses() {
    set_policy("test-rsc", r#"{"confirm": "off"}"#);
    let (port, accepted) = serve();
    let mut c = Client::spawn("test-rsc", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/app") }),
    );
    // The page's server actions ran on load.
    for _ in 0..50 {
        if accepted.lock().unwrap().len() >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(*accepted.lock().unwrap(), ["7101", "ignored 7105"]);

    // A read-only flight endpoint is listed as data, with the decoded shape.
    let listing = c.ok("api_list", json!({}));
    let list = endpoint_id(&listing, "GET /rsc/invitations");
    // Each server action is an endpoint of its own, though both post to one URL.
    let action = endpoint_id(&listing, "(server action 7f3a9c0d)");
    let ignore = endpoint_id(&listing, "(server action a1b2c3d4)");
    assert_ne!(action, ignore);
    assert!(listing.contains("{text: [str], value: "), "{listing}");

    let out = c.ok("api_call", json!({ "endpoint": list }));
    assert!(out.starts_with("200"), "{out}");
    assert!(
        out.contains("decoded from a React Server Components (flight) payload of 4 rows"),
        "{out}"
    );
    assert!(out.contains(r#""text":["Showing 2 of 5"]"#), "{out}");
    assert!(
        out.contains(r#""$el":"Invitation""#)
            && out.contains(r#""onAccept":{"$action":"7f3a9c0d"}"#),
        "{out}"
    );
    let out = c.ok(
        "api_call",
        json!({ "endpoint": list, "pick": "value.1.invitation.name" }),
    );
    assert!(out.ends_with("\"Alan Turing\""), "{out}");
    let out = c.ok("api_call", json!({ "endpoint": list, "pick": "$raw" }));
    assert!(out.contains(r#"1:I["(app-pages-browser)"#), "{out}");

    // The server action replays with its Next-Action header and other arguments.
    let out = c.ok(
        "api_call",
        json!({ "endpoint": action, "body": "[{\"invitationId\":\"7102\"}]", "pick": "value.a" }),
    );
    assert!(
        out.ends_with(r#"{"accepted":"7102","pending":"4"}"#),
        "{out}"
    );
    assert_eq!(*accepted.lock().unwrap(), ["7101", "ignored 7105", "7102"]);
    // And the other action calls the other action, by its own id.
    let out = c.ok(
        "api_call",
        json!({ "endpoint": ignore, "body": "[{\"invitationId\":\"7106\"}]", "pick": "value.a" }),
    );
    assert!(out.ends_with(r#"{"ignored":"7106"}"#), "{out}");
    assert_eq!(
        *accepted.lock().unwrap(),
        ["7101", "ignored 7105", "7102", "ignored 7106"]
    );

    // The log shows the page's own flight responses decoded, and the raw rows.
    let log = c.ok("network_log", json!({ "filter": "rsc/invitations" }));
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
        entry.contains("Response body, decoded from a React Server Components")
            && entry.contains("Grace Hopper")
            && entry.contains("Raw response:\n1:I["),
        "{entry}"
    );
    c.ok("session_close", json!({}));
}
