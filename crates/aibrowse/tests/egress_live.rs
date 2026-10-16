//! A profile pinned to an egress browses only through it: pages come through the exit,
//! and with the exit down the profile doesn't browse at all.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use common::{Client, set_policy};
use serde_json::json;

/// A plain HTTP proxy that answers every request itself and remembers what it was
/// asked for: the exit, as far as the browser can tell.
fn exit() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    return;
                }
                log.lock().unwrap().push(request.trim().to_owned());
                let body =
                    "<!doctype html><title>Through the exit</title><h1>Through the exit</h1>";
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (port, seen)
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_pinned_profile_browses_only_through_its_exit() {
    let profile = "test-egress";
    let (port, seen) = exit();
    set_policy(
        profile,
        &format!(r#"{{"confirm": "off", "egress": "http://127.0.0.1:{port}"}}"#),
    );
    let mut c = Client::spawn(profile, true);
    let page = c.ok("navigate", json!({ "url": "http://pinned.example/" }));
    assert!(page.contains("Through the exit"), "{page}");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with("GET http://pinned.example/")),
        "{:?}",
        seen.lock().unwrap()
    );
    c.ok("session_close", json!({}));

    // The exit goes away: the profile doesn't browse from the machine's own address.
    let dead = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    set_policy(
        profile,
        &format!(r#"{{"confirm": "off", "egress": "http://127.0.0.1:{dead}"}}"#),
    );
    let mut c = Client::spawn(profile, true);
    let (failed, refused) = c.call("navigate", json!({ "url": "http://pinned.example/" }));
    assert!(
        failed && refused.contains("not browsing from another address"),
        "{refused}"
    );
}
