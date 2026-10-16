//! End-to-end session traces: a few tool calls against a local page are recorded as
//! JSONL events with screenshots, a password typed on the way never reaches the disk,
//! and `aibrowse trace` turns the trace into a page. Ignored by default; run with
//! `make test-live`.

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

mod common;
use common::{Client, find_ref, set_policy};

const PASSWORD: &str = "hunter2-do-not-record";

const LOGIN: &str = r#"<!doctype html><title>Sign in</title><h1>Sign in</h1>
<form onsubmit="return false"><label>Username <input id="user" name="user"></label>
<label>Password <input id="pass" type="password" name="pass"></label></form>
<a href="/next">Continue</a>
<script>fetch('/api/ping?access_token=SECRETTOKEN');</script>"#;

const NEXT: &str = "<!doctype html><title>Next</title><h1>Signed in</h1><p>Welcome.</p>";

fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let (kind, body) = if head.starts_with("GET /next") {
                ("text/html", NEXT)
            } else if head.starts_with("GET /api/") {
                ("application/json", r#"{"ok":true}"#)
            } else {
                ("text/html", LOGIN)
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

fn headless() -> bool {
    std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1")
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .flat_map(|e| {
            if e.file_type().unwrap().is_dir() {
                files(&e.path())
            } else {
                vec![e.path()]
            }
        })
        .collect()
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn tool_calls_are_traced_and_viewable() {
    let profile = "test-trace";
    set_policy(profile, r#"{"confirm": "off"}"#);
    let traces = tempfile::tempdir().unwrap();
    let port = serve();
    let mut c = Client::spawn_with(
        profile,
        headless(),
        &[("AIBROWSE_TRACES", traces.path().to_str().unwrap())],
    );

    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let user = find_ref(&outline, "Username");
    let pass = find_ref(&outline, "password: [");
    let next = find_ref(&outline, "Continue");
    c.ok(
        "act",
        json!({ "action": "fill", "ref": user, "text": "ada" }),
    );
    c.ok(
        "act",
        json!({ "action": "fill", "ref": pass, "text": PASSWORD }),
    );
    c.ok("act", json!({ "action": "click", "ref": next }));
    c.ok("observe", json!({}));
    c.ok("session_close", json!({}));

    // Closing stdin ends the server the way an agent host does, flushing the trace.
    drop(c.stdin);
    let deadline = Instant::now() + Duration::from_secs(30);
    while c.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "server did not exit");
        std::thread::sleep(Duration::from_millis(100));
    }

    let sessions: Vec<PathBuf> = std::fs::read_dir(traces.path().join(profile))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let dir = &sessions[0];
    let mut events: Vec<Value> = std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    events.sort_by_key(|e| e["seq"].as_u64());
    let tools: Vec<&str> = events.iter().map(|e| e["tool"].as_str().unwrap()).collect();
    assert_eq!(
        tools,
        ["navigate", "act", "act", "act", "observe", "session_close"]
    );
    assert!(events.iter().all(|e| e["ok"] == true), "{events:#?}");

    // The page load: what it looked like, what it fetched (redacted), what it showed.
    let load = &events[0];
    assert_eq!(load["title"], "Sign in");
    assert!(load["actions"][0]["navigated"] == true, "{load}");
    let screenshot = dir.join(load["screenshot"].as_str().expect("a screenshot"));
    assert!(std::fs::metadata(&screenshot).unwrap().len() > 1000);
    assert!(dir.join(load["outline"].as_str().unwrap()).is_file());
    let requests = load["network"]["requests"].as_array().unwrap();
    assert!(requests.iter().any(|r| r["kind"] == "Document"), "{load}");

    // Ordinary text is recorded; the password is not, anywhere.
    assert_eq!(events[1]["args"]["text"], "ada");
    assert!(
        events[2]["args"]["text"]
            .as_str()
            .unwrap()
            .contains("redacted")
    );
    for file in files(dir) {
        let bytes = std::fs::read(&file).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(PASSWORD),
            "{} has the password",
            file.display()
        );
        assert!(
            !text.contains("SECRETTOKEN"),
            "{} has the token",
            file.display()
        );
    }

    // The click navigated; read-only calls take no screenshot.
    assert_eq!(events[3]["title"], "Next");
    assert!(events[3]["screenshot"].is_string());
    assert!(events[4]["screenshot"].is_null(), "{}", events[4]);
    assert!(events[4]["outline"].is_string());

    // The viewer: one self-contained page referencing the screenshots.
    let out = Command::new(env!("CARGO_BIN_EXE_aibrowse"))
        .args(["trace", dir.to_str().unwrap(), "--no-open"])
        .env("AIBROWSE_HANDOFF_OPEN", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let page = std::fs::read_to_string(dir.join("index.html")).unwrap();
    assert!(page.contains(load["screenshot"].as_str().unwrap()));
    assert!(page.contains("Signed in") || page.contains("Next"));
    assert!(!page.contains(PASSWORD));
}
