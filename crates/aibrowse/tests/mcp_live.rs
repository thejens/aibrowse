//! End-to-end: the real binary, spoken to over stdio MCP the way an agent host does.
//! Ignored by default; run with `make test-live`.
//!
//! Each test has its own profile: tests run in parallel, a profile holds one browser,
//! and its policy file is per profile.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

use serde_json::{Value, json};

mod common;
use common::{Client, find_ref, has_jev_key, http_get, set_policy};

const HOME: &str = r#"<!doctype html><title>Home</title>
<h1>Hi</h1>
<input aria-label="Name">
<button onclick="document.querySelector('h1').textContent='Clicked'">Press me</button>
<select aria-label="Color"><option value="r">Red</option><option value="g">Green</option></select>
<button id="confirm" onclick="this.textContent = confirm('Sure?') ? 'yes' : 'no'">Confirm</button>
<button onclick="alert('Saved!')">Alert</button>
<a href="/other" target="_blank">Open other</a>
<button onclick="window.open('/other')">Pop up other</button>
<main><h2>Article</h2><p>Some <b>readable</b> text with <a href="/x">a link</a>.</p>
<ul><li>one</li><li>two</li></ul></main>"#;

const OTHER: &str = "<!doctype html><title>Other</title><h1>Other page</h1>";

/// A 2FA step where another field has the focus: the code must still go into the code
/// field. Submitting puts what arrived into the title.
const CODE: &str = r#"<!doctype html><title>Verify</title><input autofocus aria-label="Search">
<form onsubmit="event.preventDefault(); document.title = 'got ' + this.c.value">
<input name="c" aria-label="Verification code"><button>Verify</button></form>"#;

/// A sign-in form that really posts its password.
const SIGNIN: &str = r#"<!doctype html><title>Sign in</title><h1>Sign in</h1>
<form method="post" action="/signin-done"><input name="email" aria-label="Email" value="ada@example.com">
<input type="password" name="secret" aria-label="Password" value="hunter2-very-secret">
<button autofocus>Sign in</button></form>"#;

/// A shop whose products are only reachable through its search.
const SHOP: &str = r#"<!doctype html><title>Teapot Shop</title><h1>Teapot Shop</h1>
<form action="/shop/results"><input type="search" name="q" aria-label="Search the catalog">
<button>Search</button></form>
<main><p>Fine teapots and mugs since 1902.</p><a href="/shop/about">About us</a>
<a href="/shop/shipping">Shipping</a></main>"#;
const SHOP_RESULTS: &str = r#"<!doctype html><title>Search results - Teapot Shop</title>
<h1>Search results</h1><main><ul><li><a href="/shop/item/blue-teapot">Blue teapot</a> 29 EUR</li>
<li><a href="/shop/item/red-mug">Red mug</a> 9 EUR</li></ul></main>"#;
const SHOP_ITEM: &str = r#"<!doctype html><title>Blue teapot - Teapot Shop</title>
<h1>Blue teapot</h1><main><p>A blue porcelain teapot, 1.2 litres. 29 EUR.</p>
<button>Add to cart</button></main>"#;

/// One same-origin and one cross-origin (out-of-process) iframe. localhost and 127.0.0.1
/// are different sites, so Chrome isolates the second into its own renderer.
const FRAMES: &str = r#"<!doctype html><title>Frames</title><h1>Embedding</h1>
<iframe id="same" title="Same origin" src="/inner?same" style="width:420px;height:260px"></iframe>
<iframe id="cross" title="Cross origin" style="width:420px;height:260px"></iframe>
<script>document.getElementById('cross').src = 'http://127.0.0.1:' + location.port + '/inner?cross';</script>"#;

const LOGIN: &str = r#"<!doctype html><title>Sign in</title><h1>Sign in to Example</h1>
<form onsubmit="event.preventDefault(); location = '/verify'">
<input aria-label="Email" value="ada@example.com"><input type="password" aria-label="Password" value="secret">
<button autofocus>Sign in</button></form>"#;

const VERIFY: &str = r#"<!doctype html><title>Verify it's you</title><h1>Enter the 6-digit code</h1>
<p>We sent a code to your phone.</p>
<form onsubmit="event.preventDefault(); location = '/feed'">
<input autofocus aria-label="Verification code" inputmode="numeric"><button>Verify</button></form>"#;

const FEED: &str = r#"<!doctype html><title>Home | Example</title>
<nav><a href="/feed">Home</a> <a href="/messages">Messages</a> <a href="/profile">Ada Lovelace</a></nav>
<main><h1>Your feed</h1><article><h2>Charles posted</h2><p>The Analytical Engine weaves algebraic patterns.</p></article></main>"#;

const COMPOSE: &str = r#"<!doctype html><title>Messages | Example</title><h1>Chat with Charles</h1>
<ul id="thread"><li>Charles: Shall we meet on Tuesday?</li></ul>
<button onclick="document.getElementById('older').hidden = false">Show older messages</button>
<p id="older" hidden>Charles: Hello Ada.</p>
<form onsubmit="event.preventDefault(); const li = document.createElement('li'); li.textContent = 'Ada: ' + this.msg.value; document.getElementById('thread').append(li); this.msg.value = ''">
<textarea name="msg" aria-label="Write a message"></textarea><button>Send</button></form>"#;

/// A message form that leaves the page when sent, for a user who sends it themselves in
/// the approval view.
const LEAVE: &str = r#"<!doctype html><title>Reply | Example</title><h1>Reply to Charles</h1>
<form action="/other"><input name="msg" aria-label="Your reply"><button>Send reply</button></form>"#;

/// A help page that answers "Service unavailable" the first time it is asked for, as a
/// throttled or briefly overloaded site does.
const BUSY: &str = "<!doctype html><title>Service unavailable</title><h1>Service unavailable</h1><p>Please try again later.</p>";
const HELP: &str = r#"<!doctype html><title>Help Center | Example</title><h1>Help Center</h1>
<main><p>Answers to common questions about your account, billing and orders.</p>
<a href="/other">Contact support</a></main>"#;

const INNER: &str = r#"<!doctype html><body style="height:1200px">
<button onclick="this.textContent = 'pressed in ' + location.hostname">Inner button</button>
<input aria-label="Inner field">
<button style="display:block;margin-top:900px" onclick="this.textContent = 'deep pressed'">Deep button</button>"#;

/// The requests each test server received, by its port (tests run in parallel, each
/// with its own server), for waiting on what a user does in the live view.
static RECEIVED: Mutex<Vec<(u16, String)>> = Mutex::new(Vec::new());

/// Waits up to 15 s for the server on `port` to receive a request starting with
/// `start` ("POST /signin-done"): on a loaded machine the user's input takes a while.
fn wait_for_request(port: u16, start: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let received = RECEIVED.lock().unwrap();
        if received
            .iter()
            .any(|(p, r)| *p == port && r.starts_with(start))
        {
            return;
        }
        drop(received);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Serves the test pages on localhost (HOME at /, OTHER, FRAMES, INNER at their paths).
/// One thread per connection: Chrome opens speculative preconnect sockets that may never
/// send a request, and a server reading them serially would stall every later request.
fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let first_help = request.starts_with("GET /help")
                    && !RECEIVED
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(p, r)| *p == port && r.starts_with("GET /help"));
                RECEIVED
                    .lock()
                    .unwrap()
                    .push((port, request.lines().next().unwrap_or_default().to_owned()));
                let body = if request.starts_with("GET /help") {
                    if first_help { BUSY } else { HELP }
                } else if request.starts_with("GET /code") {
                    CODE
                } else if request.starts_with("GET /signin") {
                    SIGNIN
                } else if request.starts_with("GET /shop/results") {
                    SHOP_RESULTS
                } else if request.starts_with("GET /shop/item") {
                    SHOP_ITEM
                } else if request.starts_with("GET /shop") {
                    SHOP
                } else if request.starts_with("GET /other") {
                    OTHER
                } else if request.starts_with("GET /frames") {
                    FRAMES
                } else if request.starts_with("GET /inner") {
                    INNER
                } else if request.starts_with("GET /login") {
                    LOGIN
                } else if request.starts_with("GET /leave") {
                    LEAVE
                } else if request.starts_with("GET /compose") {
                    COMPOSE
                } else if request.starts_with("GET /verify") {
                    VERIFY
                } else if request.starts_with("GET /feed") {
                    FEED
                } else {
                    HOME
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    port
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn agent_loop_over_mcp() {
    let port = serve();
    let headless_only = std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1");
    let modes: &[bool] = if headless_only {
        &[true]
    } else {
        &[false, true]
    };
    for &headless in modes {
        agent_loop(headless, &format!("http://localhost:{port}/"));
    }
}

fn agent_loop(headless: bool, base: &str) {
    // This scenario exercises mechanics, not judgment: no approval prompts.
    set_policy("test-mcp-loop", r#"{"confirm": "off"}"#);
    let mut c = Client::spawn("test-mcp-loop", headless);

    // The session starts by itself; navigate returns the outline.
    let outline = c.ok("navigate", json!({ "url": base }));
    assert!(outline.contains("heading \"Hi\""), "{outline}");

    let result = c.request(
        "tools/call",
        json!({ "name": "observe", "arguments": { "screenshot": true } }),
    );
    assert_eq!(result["content"][1]["type"], "image", "headless={headless}");

    let field = find_ref(&outline, "textbox \"Name\"");
    c.ok(
        "act",
        json!({ "action": "fill", "ref": field, "text": "Ada" }),
    );
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "\"Press me\""), "observe": true }),
    );
    assert!(
        out.contains("heading \"Clicked\"") && out.contains("value=\"Ada\""),
        "{out}"
    );

    let out = c.ok("act", json!({ "action": "select", "ref": find_ref(&outline, "combobox \"Color\""), "text": "green" }));
    assert!(out.contains("selected \"Green\""), "{out}");
    let value = c.ok(
        "evaluate",
        json!({ "expression": "document.querySelector('select').value" }),
    );
    assert_eq!(value, "g");

    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "\"Alert\"") }),
    );
    assert!(
        out.contains("alert (accepted)") && out.contains("Saved!"),
        "{out}"
    );

    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "\"Confirm\"") }),
    );
    assert!(out.contains("confirm dialog is open"), "{out}");
    let (err, out) = c.call("read", json!({}));
    assert!(
        err && out.contains("dismiss_dialog"),
        "a dialog must block other tools: {out}"
    );
    c.ok("act", json!({ "action": "dismiss_dialog" }));
    let label = c.ok(
        "evaluate",
        json!({ "expression": "document.getElementById('confirm').textContent" }),
    );
    assert_eq!(label, "no");

    let text = c.ok("read", json!({}));
    assert!(
        text.contains("## Article") && text.contains("- one"),
        "{text}"
    );
    assert!(text.contains("[a link](http://localhost:"), "{text}");
    // The digest: the header, then the page's own text with links reduced to words.
    let digest = c.ok("read", json!({ "view": "digest", "focus": "the article" }));
    assert!(
        digest.contains("url: http://localhost:")
            && digest.contains("Page text")
            && digest.contains("readable text with a link"),
        "{digest}"
    );

    // New tabs start paused when headless (early_identity.rs); a resume that waits on
    // an answer from the paused page is stuck until the 30s CDP timeout.
    let clicked = std::time::Instant::now();
    let out = c.ok("act", json!({ "action": "click", "ref": find_ref(&outline, "link \"Open other\""), "observe": true }));
    assert!(
        out.contains("opened a new tab") && out.contains("Other page"),
        "{out}"
    );
    assert!(
        clicked.elapsed() < std::time::Duration::from_secs(15),
        "new-tab click took {:?}",
        clicked.elapsed()
    );
    let tabs = c.ok("tabs", json!({}));
    assert!(
        tabs.contains("2. Other") && tabs.contains("(active)"),
        "{tabs}"
    );
    let tabs = c.ok("tabs", json!({ "close": 2 }));
    assert!(
        tabs.starts_with("1. Home") && !tabs.contains("2."),
        "{tabs}"
    );

    // A window.open popup keeps its opener (same renderer, unlike a target=_blank link).
    // Headless, Chrome holds such a popup's first navigation until a browser-level
    // auto-attached session resumes it; unresumed, the popup and its opener both hang.
    let outline = c.ok("observe", json!({}));
    let clicked = std::time::Instant::now();
    let out = c.ok("act", json!({ "action": "click", "ref": find_ref(&outline, "button \"Pop up other\""), "observe": true }));
    assert!(
        out.contains("opened a new tab") && out.contains("Other page"),
        "{out}"
    );
    assert!(
        clicked.elapsed() < std::time::Duration::from_secs(15),
        "popup click took {:?}",
        clicked.elapsed()
    );
    let tabs = c.ok("tabs", json!({ "close": 2 }));
    assert!(
        tabs.starts_with("1. Home") && !tabs.contains("2."),
        "{tabs}"
    );

    // Iframes: both appear nested under their <iframe>, and both can be driven. The
    // cross-origin frame's refs carry its target prefix.
    let outline = c.ok("navigate", json!({ "url": format!("{base}frames") }));
    assert!(
        outline.contains("Iframe \"Same origin\"") || outline.contains("iframe"),
        "{outline}"
    );
    let buttons: Vec<String> = outline
        .lines()
        .filter(|l| l.contains("button \"Inner button\""))
        .map(|l| l[l.find("[").unwrap() + 1..l.find("]").unwrap()].to_owned())
        .collect();
    assert_eq!(
        buttons.len(),
        2,
        "both frames' buttons must be listed:\n{outline}"
    );
    assert!(
        !buttons[0].starts_with('f') && buttons[1].starts_with('f'),
        "{buttons:?}"
    );
    for button in &buttons {
        c.ok("act", json!({ "action": "click", "ref": button }));
    }
    let fields: Vec<String> = outline
        .lines()
        .filter(|l| l.contains("textbox \"Inner field\""))
        .map(|l| l[l.find("[").unwrap() + 1..l.find("]").unwrap()].to_owned())
        .collect();
    let out = c.ok(
        "act",
        json!({ "action": "fill", "ref": fields[1], "text": "cross typed" }),
    );
    assert!(
        !out.contains("Note:"),
        "typing in the cross-origin frame must land: {out}"
    );
    // Checked before scrolling the frame: once scrolled, the outline only summarizes
    // what is above.
    let after = c.ok("observe", json!({}));
    assert!(after.contains("pressed in localhost"), "{after}");
    assert!(after.contains("pressed in 127.0.0.1"), "{after}");
    assert!(after.contains("value=\"cross typed\""), "{after}");
    let deep_buttons: Vec<String> = outline
        .lines()
        .filter(|l| l.contains("button \"Deep button\""))
        .map(|l| l[l.find("[").unwrap() + 1..l.find("]").unwrap()].to_owned())
        .collect();
    assert!(
        outline
            .lines()
            .any(|l| l.contains("Deep button") && l.contains("(below)")),
        "{outline}"
    );
    c.ok("act", json!({ "action": "click", "ref": deep_buttons[1] }));
    let after = c.ok("observe", json!({}));
    assert!(
        after.contains("deep pressed"),
        "scrolling inside a cross-origin frame: {after}"
    );

    handoff(&mut c, base);

    let (err, out) = c.call("act", json!({ "action": "click", "ref": "e999999" }));
    assert!(err || out.contains("observe again"), "{out}");

    c.ok("session_close", json!({}));
    drop(c.stdin);
    c.child.wait().unwrap();
}

/// The user takes over through the live view, types into the focused field, and hands back.
fn handoff(c: &mut Client, base: &str) {
    let outline = c.ok("navigate", json!({ "url": base }));
    c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "textbox \"Name\"") }),
    );
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Type your name, then press Done", "open": false }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();

    let (err, out) = c.call("navigate", json!({ "url": base }));
    assert!(
        err && out.contains("handoff"),
        "agent tools must be blocked during a handoff: {out}"
    );
    let out = c.ok("handoff_status", json!({ "wait_seconds": 1 }));
    assert!(out.contains("still pending"), "{out}");

    let page = http_get(&url);
    assert!(
        page.contains("Type your name"),
        "the live view shows the reason"
    );

    let (mut ws, _) = tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1)))
        .expect("live view socket");
    let mut got_frame = false;
    for _ in 0..50 {
        if let Ok(tungstenite::Message::Binary(jpeg)) = ws.read() {
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "frames are JPEG");
            got_frame = true;
            break;
        }
    }
    assert!(got_frame, "the live view must stream the tab");
    for message in [r#"{"t":"text","text":"Human Typed"}"#, r#"{"t":"done"}"#] {
        ws.send(tungstenite::Message::Text(message.into())).unwrap();
    }

    let out = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(out.contains("is done"), "{out}");
    assert!(
        out.contains("value=\"Human Typed\""),
        "the user's typing reached the page: {out}"
    );
    assert!(
        http_get(&url).contains("expired"),
        "a used link must not work again"
    );
    c.ok("navigate", json!({ "url": base }));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn handoff_completes_when_the_user_is_through() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-mcp-handoff", r#"{"confirm": "off"}"#);
    let port = serve();
    let mut c = Client::spawn("test-mcp-handoff", true);
    let base = format!("http://localhost:{port}/");
    c.ok("navigate", json!({ "url": format!("{base}login") }));
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Sign in to Example", "open": false }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();

    // The user signs in (Enter on the focused button) and lands on a 2FA step: that is
    // a follow-up step of the same task, so the handoff must stay with them.
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    let send = |ws: &mut tungstenite::WebSocket<_>, message: serde_json::Value| {
        ws.send(tungstenite::Message::Text(message.to_string().into()))
            .unwrap();
    };
    let enter = |ws: &mut tungstenite::WebSocket<_>| {
        for kind in ["keyDown", "keyUp"] {
            send(
                ws,
                json!({ "t": "key", "kind": kind, "key": "Enter", "code": "Enter", "keyCode": 13, "modifiers": 0 }),
            );
        }
    };
    enter(&mut ws);
    let status = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(
        status.contains("still pending"),
        "a 2FA step is not done: {status}"
    );

    // They enter the code and get through, never pressing Done.
    send(&mut ws, json!({ "t": "text", "text": "123456" }));
    enter(&mut ws);
    let mut status = String::new();
    for _ in 0..3 {
        status = c.ok("handoff_status", json!({ "wait_seconds": 20 }));
        if !status.contains("still pending") {
            break;
        }
    }
    assert!(
        status.contains("is done"),
        "the handoff must complete by itself: {status}"
    );
    assert!(status.contains("Your feed"), "{status}");
    c.ok("session_close", json!({}));
}

/// Reads text messages from a live view socket until one satisfies `want`.
fn await_message(
    ws: &mut tungstenite::WebSocket<impl Read + Write>,
    want: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..200 {
        if let Ok(tungstenite::Message::Text(text)) = ws.read()
            && let Ok(message) = serde_json::from_str::<Value>(&text)
            && want(&message)
        {
            return message;
        }
    }
    panic!("expected message never arrived");
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn watch_view_follows_the_agent_and_becomes_the_controls() {
    set_policy("test-mcp-watch", r#"{"confirm": "off"}"#);
    let port = serve();
    let mut c = Client::spawn("test-mcp-watch", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let out = c.ok("watch", json!({ "open": false }));
    let url = out
        .split_whitespace()
        .find(|w| w.starts_with("http"))
        .unwrap()
        .to_owned();
    assert!(
        http_get(&url).contains("Watching the agent"),
        "the watch page starts read-only"
    );

    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    let mode = await_message(&mut ws, |m| m["t"] == "mode");
    assert!(
        mode["handoff"].is_null(),
        "watching, not controlling: {mode}"
    );
    let mut frame = false;
    for _ in 0..50 {
        if let Ok(tungstenite::Message::Binary(_)) = ws.read() {
            frame = true;
            break;
        }
    }
    assert!(frame, "the watch view streams the agent's tab");

    // Input while merely watching is ignored.
    let typed = json!({ "t": "text", "text": "should not land" });
    ws.send(tungstenite::Message::Text(typed.to_string().into()))
        .unwrap();

    // A handoff turns the same page into the controls; Done there resolves it.
    c.ok(
        "request_handoff",
        json!({ "reason": "Check this page", "open": false }),
    );
    let mode = await_message(&mut ws, |m| m["t"] == "mode" && !m["handoff"].is_null());
    assert_eq!(mode["handoff"]["kind"], "takeover");
    assert_eq!(mode["handoff"]["reason"], "Check this page");
    ws.send(tungstenite::Message::Text(r#"{"t":"done"}"#.into()))
        .unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(out.contains("is done"), "{out}");
    let mode = await_message(&mut ws, |m| m["t"] == "mode");
    assert!(mode["handoff"].is_null(), "back to watching: {mode}");

    let value = c.ok(
        "evaluate",
        json!({ "expression": "document.querySelector('input').value" }),
    );
    assert!(
        !value.contains("should not land"),
        "watch input must be ignored: {value}"
    );
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn consequential_actions_wait_for_approval() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-mcp-approval", r#"{"confirm": "consequential"}"#);
    let port = serve();
    let mut c = Client::spawn("test-mcp-approval", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/compose") }),
    );

    // Harmless: runs directly.
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Show older messages") }),
    );
    assert!(out.contains("\": done"), "{out}");

    c.ok("act", json!({ "action": "fill", "ref": find_ref(&outline, "Write a message"), "text": "Tuesday works" }));
    let send = find_ref(&outline, "button \"Send\"");
    let thread = || json!({ "expression": "document.getElementById('thread').innerText" });

    // Consequential: waits, and a denial means it never happens.
    let out = c.ok("act", json!({ "action": "click", "ref": send }));
    assert!(out.contains("needs their approval"), "{out}");
    let url = out
        .split_whitespace()
        .find(|w| w.starts_with("http"))
        .unwrap()
        .to_owned();
    assert!(
        http_get(&url).contains("Tuesday works"),
        "the approval page shows what will be sent"
    );
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    ws.send(tungstenite::Message::Text(r#"{"t":"cancel"}"#.into()))
        .unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(out.contains("denied"), "{out}");
    assert!(
        !c.ok("evaluate", thread()).contains("Ada:"),
        "a denied send must not happen"
    );

    // Approved: the server performs exactly the approved click.
    let out = c.ok("act", json!({ "action": "click", "ref": send }));
    let url = out
        .split_whitespace()
        .find(|w| w.starts_with("http"))
        .unwrap()
        .to_owned();
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    ws.send(tungstenite::Message::Text(r#"{"t":"done"}"#.into()))
        .unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(out.contains("approved"), "{out}");
    assert!(
        c.ok("evaluate", thread()).contains("Ada: Tuesday works"),
        "the approved send happens"
    );

    // The user sends it themselves in the approval view (Enter in the field), then
    // approves: the page has moved on, so nothing is clicked again and the agent hears
    // where things are.
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/leave") }),
    );
    c.ok("act", json!({ "action": "fill", "ref": find_ref(&outline, "Your reply"), "text": "See you then" }));
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Send reply") }),
    );
    assert!(out.contains("needs their approval"), "{out}");
    let url = out
        .split_whitespace()
        .find(|w| w.starts_with("http"))
        .unwrap()
        .to_owned();
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    for kind in ["keyDown", "keyUp"] {
        let key = json!({ "t": "key", "kind": kind, "key": "Enter", "code": "Enter", "keyCode": 13, "modifiers": 0 });
        ws.send(tungstenite::Message::Text(key.to_string().into()))
            .unwrap();
    }
    wait_for_request(port, "GET /other?msg=");
    ws.send(tungstenite::Message::Text(r#"{"t":"done"}"#.into()))
        .unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(out.contains("the page changed while they decided"), "{out}");
    assert!(out.contains("/other?msg=See+you+then"), "{out}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn unattended_sessions_never_route_to_a_human() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-mcp-unattended", r#"{"confirm": "consequential"}"#);
    let port = serve();
    let mut c = Client::spawn("test-mcp-unattended", true);
    let page = format!("http://localhost:{port}/compose");
    let outline = c.ok("session_open", json!({ "url": page, "attended": false }));
    c.ok("act", json!({ "action": "fill", "ref": find_ref(&outline, "Write a message"), "text": "On my way" }));
    let send = find_ref(&outline, "button \"Send\"");

    let out = c.ok("act", json!({ "action": "click", "ref": send }));
    assert!(
        out.contains("no user is present"),
        "consequential actions come back to the agent: {out}"
    );
    let (err, out) = c.call(
        "request_handoff",
        json!({ "reason": "help", "open": false }),
    );
    assert!(err && out.contains("unattended"), "{out}");

    // On its own authority (its instructions allow it), the agent may proceed.
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": send, "confirm": true }),
    );
    assert!(out.contains("\": done"), "{out}");
    let thread = c.ok(
        "evaluate",
        json!({ "expression": "document.getElementById('thread').innerText" }),
    );
    assert!(thread.contains("Ada: On my way"), "{thread}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn navigator_types_the_agents_suggested_texts() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-mcp-texts", r#"{"confirm": "off"}"#);
    let port = serve();
    let mut c = Client::spawn("test-mcp-texts", true);
    let shop = format!("http://localhost:{port}/shop");
    let goal = "the product page for the blue teapot";

    // A goal with too few words of its own to search for needs the agent's: handed back.
    let out = c.ok("navigate", json!({ "url": shop, "goal": "the teapot" }));
    assert!(out.contains("Your turn"), "{out}");

    // Without a suggestion, the goal's own words go into the search: those Jev finds
    // a person would search for ("blue teapot"; "product" may stay).
    let out = c.ok("navigate", json!({ "url": shop, "goal": goal }));
    assert!(
        out.contains("blue teapot\" into") && out.contains("Reached"),
        "{out}"
    );

    // With one, the navigator types it into the search itself and goes on.
    let out = c.ok(
        "navigate",
        json!({ "url": shop, "goal": goal, "texts": ["blue teapot"], "digest": true }),
    );
    assert!(
        out.contains("typed \"blue teapot\"") && out.contains("Reached"),
        "{out}"
    );
    assert!(
        out.contains("1.2 litres"),
        "the digest of the product page: {out}"
    );
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn what_the_user_sends_during_a_handoff_is_not_kept() {
    let port = serve();
    let mut c = Client::spawn("test-mcp-privacy", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/signin") }),
    );
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Sign in to Example", "open": false }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();

    // The user submits the form (Enter on the focused button), then hands back.
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    for kind in ["keyDown", "keyUp"] {
        let key = json!({ "t": "key", "kind": kind, "key": "Enter", "code": "Enter", "keyCode": 13, "modifiers": 0 });
        ws.send(tungstenite::Message::Text(key.to_string().into()))
            .unwrap();
    }
    // The agent can't look while the user has the browser; the server can.
    wait_for_request(port, "POST /signin-done");
    ws.send(tungstenite::Message::Text(r#"{"t":"done"}"#.into()))
        .unwrap();
    let status = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(status.contains("done"), "{status}");

    // The sign-in request is in the log, as the user's, without its body.
    let log = c.ok("network_log", json!({}));
    let line = log
        .lines()
        .find(|l| l.contains("POST") && l.contains("/signin-done"))
        .unwrap_or_else(|| panic!("the sign-in request is logged: {log}"));
    assert!(line.contains("(the user's)"), "{line}");
    let id: u64 = line[1..line.find(' ').unwrap()].parse().unwrap();
    let entry = c.ok("network_log", json!({ "id": id }));
    assert!(entry.contains("bodies were not kept"), "{entry}");
    assert!(
        !entry.contains("hunter2"),
        "the password must not be kept: {entry}"
    );

    // Once handed back, the agent's own requests are logged in full again.
    c.ok(
        "evaluate",
        json!({ "expression": "fetch('/after', { method: 'POST', body: 'agent=1' }).then(() => 1)", "world": "main" }),
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let log = c.ok("network_log", json!({}));
    let after = log
        .lines()
        .find(|l| l.contains("/after"))
        .unwrap_or_else(|| panic!("the agent's request is logged: {log}"));
    assert!(!after.contains("(the user's)"), "{after}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_relayed_code_goes_into_its_field() {
    let port = serve();
    let mut c = Client::spawn("test-mcp-code", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/code") }),
    );
    let field = find_ref(&outline, "Verification code");
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Enter the code we texted you", "open": false, "code_field": field }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();
    // The page is the code box, not the browser.
    let page = http_get(&url);
    assert!(
        page.contains("Send code") && page.contains("const KIND = 'code'"),
        "{page}"
    );

    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    ws.send(tungstenite::Message::Text(
        r#"{"t":"code","code":" 482913 "}"#.into(),
    ))
    .unwrap();
    let status = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(
        status.contains("done") && status.contains("entered a code (8 characters)"),
        "{status}"
    );
    let title = c.ok(
        "evaluate",
        json!({ "expression": "document.title", "world": "main" }),
    );
    assert!(
        title.contains("got 482913"),
        "typed into the code field and submitted: {title}"
    );
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn an_error_page_is_loaded_again_once() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-mcp-busy", r#"{"confirm": "off"}"#);
    let port = serve();
    let mut c = Client::spawn("test-mcp-busy", true);
    let out = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/help"), "goal": "the help center" }),
    );
    assert!(
        out.contains("loaded the page again: it was an error page") && out.contains("Reached"),
        "{out}"
    );
    c.ok("session_close", json!({}));
}
