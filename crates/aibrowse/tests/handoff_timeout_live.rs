//! A handoff that gives up on an absent user: the person is told how long they have,
//! and the agent is told they didn't respond (not that they declined), so it carries on.

mod common;

use std::time::{Duration, Instant};

use common::{Client, http_get, serve_pages};
use serde_json::json;

const PAGE: &str = r#"<!doctype html><title>Shop</title><h1>Shop</h1>"#;

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_handoff_gives_up_on_an_absent_user() {
    let port = serve_pages(&[("/", PAGE)]);
    let mut c = Client::spawn("test-handoff-timeout", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let out = c.ok(
        "request_handoff",
        json!({
            "reason": "Please set the price filter to at most 5000 kr, then press Done",
            "give_up_after_minutes": 1,
            "open": false,
        }),
    );
    assert!(out.contains("after 1 minutes without them"), "{out}");
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();
    // The person sees the request and how long they have.
    let page = http_get(&url);
    assert!(page.contains("Please set the price filter"), "{page}");
    assert!(
        page.contains("carry on without this after 1 minutes"),
        "{page}"
    );

    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        let status = c.ok("handoff_status", json!({ "wait_seconds": 55 }));
        if !status.contains("still pending") || Instant::now() > deadline {
            break status;
        }
    };
    assert!(status.contains("didn't respond in time"), "{status}");
    c.ok("session_close", json!({}));
}
