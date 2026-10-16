//! End-to-end: calling tools a page offers to agents (WebMCP) through `act call_tool`,
//! gated like the clicks they replace. Ignored by default; run with `make test-live`.

use std::io::{Read, Write};
use std::net::TcpListener;

use serde_json::json;

mod common;
use common::{Client, set_policy};

/// A shop that offers a read-only search tool and a consequential order tool.
const SHOP: &str = r#"<!doctype html><title>Shop</title><h1>Shop</h1><p id="orders">no orders</p>
<script>
const items = ['red mug', 'blue mug', 'green teapot'];
document.modelContext?.registerTool({ name: 'search', description: 'Search the catalog',
  annotations: { readOnlyHint: true },
  inputSchema: { type: 'object', properties: { query: { type: 'string' } }, required: ['query'] },
  execute: async ({ query }) => ({ content: [{ type: 'text',
    text: JSON.stringify(items.filter((i) => i.includes(query))) }] }) });
document.modelContext?.registerTool({ name: 'place_order', description: 'Order an item and pay',
  annotations: { consequentialHint: true },
  inputSchema: { type: 'object', properties: { item: { type: 'string' } }, required: ['item'] },
  execute: async ({ item }) => {
    document.getElementById('orders').textContent = 'ordered ' + item;
    return { content: [{ type: 'text', text: 'order placed for ' + item }] };
  } });
</script>"#;

fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{SHOP}",
                    SHOP.len()
                );
            });
        }
    });
    port
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn page_tools_are_callable_and_gated() {
    set_policy("test-webmcp", r#"{"confirm": "consequential"}"#);
    let port = serve();
    let mut c = Client::spawn("test-webmcp", true);
    let outline = c.ok(
        "session_open",
        json!({ "url": format!("http://localhost:{port}/"), "attended": false }),
    );
    assert!(
        outline.contains("WebMCP tools this page offers") && outline.contains("place_order"),
        "{outline}"
    );
    let info = c.ok("site_info", json!({}));
    assert!(
        info.contains("[read-only]") && info.contains("[consequential]"),
        "{info}"
    );

    // A tool the page marks read-only runs directly, and its output comes back labelled.
    let out = c.ok(
        "act",
        json!({ "action": "call_tool", "tool": "search", "args": { "query": "mug" } }),
    );
    assert!(out.contains("call tool \"search\""), "{out}");
    assert!(
        out.contains("Tool output (written by the page") && out.contains("blue mug"),
        "{out}"
    );
    assert!(!out.contains("teapot"), "{out}");

    // Unknown tools say what the page does offer.
    let out = c.ok("act", json!({ "action": "call_tool", "tool": "nope" }));
    assert!(
        out.contains("Error: the page offers no tool \"nope\"; it has: search, place_order"),
        "{out}"
    );

    // A consequential tool needs the agent's explicit authority when nobody is present.
    let order =
        json!({ "action": "call_tool", "tool": "place_order", "args": { "item": "red mug" } });
    let out = c.ok("act", order.clone());
    assert!(out.contains("no user is present"), "{out}");
    let orders = json!({ "expression": "document.getElementById('orders').textContent" });
    assert_eq!(c.ok("evaluate", orders.clone()), "no orders");

    let mut confirmed = order;
    confirmed["confirm"] = json!(true);
    let out = c.ok("act", confirmed);
    assert!(out.contains("order placed for red mug"), "{out}");
    assert_eq!(c.ok("evaluate", orders), "ordered red mug");
    c.ok("session_close", json!({}));
}
