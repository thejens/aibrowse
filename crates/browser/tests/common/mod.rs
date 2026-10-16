//! Shared helpers for live tests.
// Each test binary compiles this module separately and uses a different subset of it.
#![allow(dead_code)]

use aibrowse_browser::{ElementRef, Tab};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The modes live tests cover: headful (exercises real windows and HiDPI) and headless.
/// `AIBROWSE_TEST_HEADLESS=1` restricts to headless for machines without a display.
pub fn modes() -> Vec<bool> {
    if std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1") {
        vec![true]
    } else {
        vec![false, true]
    }
}

/// Serves `body` as HTML on http://localhost:<port>/ (a secure context, unlike data:
/// URLs, so APIs such as navigator.userAgentData are available).
pub async fn serve(body: &'static str) -> u16 {
    serve_with_script(body, "").await
}

/// Like [`serve`], plus `script` as JavaScript at /script.js (e.g. a service worker,
/// which must be same-origin and served with a JavaScript type).
pub async fn serve_with_script(body: &'static str, script: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let (kind, body) = if buf.starts_with(b"GET /script.js ") {
                    ("text/javascript", script)
                } else {
                    ("text/html", body)
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    port
}

/// (width, height) of a base64 JPEG, read from its start-of-frame marker.
pub fn jpeg_size(base64_data: &str) -> (u32, u32) {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .unwrap();
    let mut i = 2;
    while i + 9 < bytes.len() {
        assert_eq!(bytes[i], 0xFF, "malformed JPEG");
        let marker = bytes[i + 1];
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if (0xC0..=0xC2).contains(&marker) {
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]);
            return (u32::from(w), u32::from(h));
        }
        i += 2 + len;
    }
    panic!("no start-of-frame marker in JPEG");
}

/// The element matching `selector` on the page.
pub async fn backend_id(tab: &Tab, selector: &str) -> ElementRef {
    let session = tab.session();
    let doc = session.call("DOM.getDocument", json!({})).await.unwrap();
    let root = doc["root"]["nodeId"].as_i64().unwrap();
    let found = session
        .call(
            "DOM.querySelector",
            json!({ "nodeId": root, "selector": selector }),
        )
        .await
        .unwrap();
    let node = session
        .call("DOM.describeNode", json!({ "nodeId": found["nodeId"] }))
        .await
        .unwrap();
    ElementRef::new(0, node["node"]["backendNodeId"].as_i64().unwrap())
}

/// A property of the element matching `selector`, read in the main world.
pub async fn page_text(tab: &Tab, selector: &str, prop: &str) -> String {
    // Test-only main-world read; production code never evaluates in the main world.
    let response = tab
        .session()
        .call(
            "Runtime.evaluate",
            json!({ "expression": format!("String(document.querySelector('{selector}').{prop})"), "returnByValue": true }),
        )
        .await
        .unwrap();
    response["result"]["value"].as_str().unwrap().to_owned()
}
