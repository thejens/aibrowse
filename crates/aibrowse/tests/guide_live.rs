//! What an agent can learn from the server besides its tools: the guides and the
//! session as MCP resources, and the task prompts. Ignored by default; run with
//! `make test-live`.

mod common;

use common::Client;
use serde_json::json;

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn guides_session_and_prompts_are_served() {
    let mut c = Client::spawn("test-guides", true);

    let listed = c.request("resources/list", json!({}));
    let uris: Vec<&str> = listed["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .collect();
    for uri in [
        "aibrowse://guide/overview",
        "aibrowse://guide/goals",
        "aibrowse://session",
    ] {
        assert!(uris.contains(&uri), "{uri} in {uris:?}");
    }
    let read = c.request("resources/read", json!({ "uri": "aibrowse://guide/goals" }));
    let text = read["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("It is set to"), "{text}");

    // Before the browser starts, the session says which profile it would use; after,
    // the profile and its tabs.
    let before = c.request("resources/read", json!({ "uri": "aibrowse://session" }));
    assert!(
        before["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No browser")
    );
    c.ok(
        "navigate",
        json!({ "url": "about:blank", "observe": false }),
    );
    let after = c.request("resources/read", json!({ "uri": "aibrowse://session" }));
    let session = after["contents"][0]["text"].as_str().unwrap();
    assert!(
        session.contains("Profile: test-guides") && session.contains("about:blank"),
        "{session}"
    );

    let prompts = c.request("prompts/list", json!({}));
    let names: Vec<&str> = prompts["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(
        names.contains(&"compare_prices") && names.contains(&"sign_in"),
        "{names:?}"
    );
    let prompt = c.request(
        "prompts/get",
        json!({ "name": "compare_prices", "arguments": { "product": "Sony WH-1000XM6" } }),
    );
    let text = prompt["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(
        text.contains("survey(") && text.contains("Sony WH-1000XM6"),
        "{text}"
    );
    c.ok("session_close", json!({}));
}

/// Through the reload front, the way clients launch aibrowse: the worker's resources
/// and prompts come through as its tools do.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn the_reload_front_passes_on_guides_and_prompts() {
    let bin = tempfile::tempdir().unwrap();
    let exe = bin.path().join("aibrowse");
    std::fs::copy(env!("CARGO_BIN_EXE_aibrowse"), &exe).unwrap();
    let mut c = Client::spawn_reloading(&exe, "test-guides-reload", true);
    let listed = c.request("resources/list", json!({}));
    assert!(
        listed["resources"]
            .as_array()
            .is_some_and(|r| r.iter().any(|r| r["uri"] == "aibrowse://guide/overview")),
        "{listed}"
    );
    let read = c.request("resources/read", json!({ "uri": "aibrowse://guide/files" }));
    assert!(
        read["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("upload")
    );
    let prompt = c.request(
        "prompts/get",
        json!({ "name": "sign_in", "arguments": { "site": "https://example.com" } }),
    );
    let text = prompt["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("request_handoff"), "{text}");
}
