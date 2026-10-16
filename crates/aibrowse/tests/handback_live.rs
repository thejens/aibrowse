//! After the user hands back, the handoff page goes on showing the agent at work,
//! read-only: frames keep coming as the agent browses, and nothing the page sends
//! reaches the browser any more.

mod common;

use std::time::{Duration, Instant};

use common::{Client, serve_pages};
use serde_json::json;
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

const FIRST: &str = r#"<!doctype html><title>First</title><h1>First</h1>
<input id="field" aria-label="Note" autofocus>"#;
const SECOND: &str = r#"<!doctype html><title>Second</title><h1>Second</h1>
<input id="field" aria-label="Note" autofocus>"#;
const PAGES: &[(&str, &str)] = &[("/", FIRST), ("/second", SECOND)];

/// The next message matching `wanted` within `timeout`.
fn wait_for(ws: &mut Socket, timeout: Duration, wanted: impl Fn(&Message) -> bool) -> Message {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match ws.read() {
            Ok(message) if wanted(&message) => return message,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => panic!("live view socket: {err}"),
        }
    }
    panic!("no matching message within {timeout:?}");
}

/// Reads until nothing has arrived for one read timeout (500 ms), or for 5 s at most
/// (a blinking caret repaints all the time).
fn drain(ws: &mut Socket) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match ws.read() {
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(err) => panic!("live view socket: {err}"),
        }
    }
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn the_page_shows_the_agent_after_handing_back() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-handback", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Have a look", "open": false }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
    }
    wait_for(&mut ws, Duration::from_secs(15), |m| {
        matches!(m, Message::Binary(_))
    });

    ws.send(Message::Text(r#"{"t":"done"}"#.into())).unwrap();
    let resolved = wait_for(
        &mut ws,
        Duration::from_secs(10),
        |m| matches!(m, Message::Text(t) if t.contains("resolved")),
    );
    assert!(
        resolved.to_text().unwrap().contains(r#""watching":300"#),
        "{resolved:?}"
    );
    let out = c.ok("handoff_status", json!({ "wait_seconds": 5 }));
    assert!(out.contains("done"), "{out}");

    // The agent goes on browsing; the page keeps showing it. The page is static, so it
    // repaints only while loading, which may be over before navigate returns: let the
    // stream go quiet first, then any frame comes from the navigation.
    drain(&mut ws);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/second"), "observe": false }),
    );
    wait_for(&mut ws, Duration::from_secs(15), |m| {
        matches!(m, Message::Binary(_))
    });

    // Read-only: what the page sends now is dropped.
    ws.send(Message::Text(r#"{"t":"text","text":"late"}"#.into()))
        .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let value = c.ok(
        "evaluate",
        json!({ "expression": "document.getElementById('field').value" }),
    );
    assert!(!value.contains("late"), "input after handing back: {value}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_reload_in_the_live_view_reloads_the_agents_page() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-live-reload", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    c.ok(
        "evaluate",
        json!({ "expression": "document.body.dataset.mark = 'old'" }),
    );
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Have a look", "open": false }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
    }
    wait_for(&mut ws, Duration::from_secs(15), |m| {
        matches!(m, Message::Binary(_))
    });
    ws.send(Message::Text(r#"{"t":"reload"}"#.into())).unwrap();
    // A reload repaints the page; once that is over, hand back and look.
    wait_for(&mut ws, Duration::from_secs(10), |m| {
        matches!(m, Message::Binary(_))
    });
    drain(&mut ws);
    ws.send(Message::Text(r#"{"t":"done"}"#.into())).unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 5 }));
    assert!(out.contains("done"), "{out}");
    let mark = c.ok(
        "evaluate",
        json!({ "expression": "document.body.dataset.mark || 'fresh'" }),
    );
    assert!(
        mark.contains("fresh"),
        "the agent's page wasn't reloaded: {mark}"
    );
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn frames_fit_the_viewers_picture() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-live-view-size", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Have a look", "open": false }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
    }
    wait_for(&mut ws, Duration::from_secs(15), |m| {
        matches!(m, Message::Binary(_))
    });
    // A phone-sized picture; then something to repaint (typing in the focused field).
    ws.send(Message::Text(r#"{"t":"view","w":480,"h":360}"#.into()))
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    drain(&mut ws);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut widths = Vec::new();
    while Instant::now() < deadline && widths.len() < 3 {
        ws.send(Message::Text(r#"{"t":"text","text":"a"}"#.into()))
            .unwrap();
        if let Message::Binary(jpeg) = wait_for(&mut ws, Duration::from_secs(5), |m| {
            matches!(m, Message::Binary(_))
        }) {
            let picture = image::load_from_memory(&jpeg).unwrap();
            widths.push(picture.width());
        }
    }
    assert!(
        !widths.is_empty() && widths.iter().all(|w| *w <= 480),
        "frame widths {widths:?}"
    );
    ws.send(Message::Text(r#"{"t":"done"}"#.into())).unwrap();
    c.ok("session_close", json!({}));
}

/// A second handoff on the same tab, after the first was handed back and its live view
/// (still showing the agent at work) was closed: the new live view gets the picture.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_second_handoff_gets_the_picture_after_the_first_view_closed() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-handoff-twice", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let connect = |url: &str| {
        let (mut ws, _) =
            tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
        if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
        }
        ws
    };
    let link = |out: &str| {
        out.lines()
            .find(|l| l.starts_with("http"))
            .expect("live view url")
            .to_owned()
    };
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "First", "open": false }),
    );
    let mut first = connect(&link(&out));
    wait_for(&mut first, Duration::from_secs(15), |m| {
        matches!(m, Message::Binary(_))
    });
    first.send(Message::Text(r#"{"t":"done"}"#.into())).unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 5 }));
    assert!(out.contains("done"), "{out}");
    // The user closes the first live view, which was still showing the agent at work.
    drain(&mut first);
    drop(first);
    std::thread::sleep(Duration::from_secs(1));

    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Second", "open": false }),
    );
    let mut second = connect(&link(&out));
    wait_for(&mut second, Duration::from_secs(15), |m| {
        matches!(m, Message::Binary(_))
    });
    // And it goes on getting frames as the page changes.
    drain(&mut second);
    second
        .send(Message::Text(r#"{"t":"text","text":"x"}"#.into()))
        .unwrap();
    wait_for(&mut second, Duration::from_secs(10), |m| {
        matches!(m, Message::Binary(_))
    });
    second
        .send(Message::Text(r#"{"t":"done"}"#.into()))
        .unwrap();
    c.ok("session_close", json!({}));
}
