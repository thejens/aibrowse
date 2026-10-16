//! Tools a page registers through a `navigator.modelContext` polyfill (as sites did
//! before browsers had WebMCP) are listed and callable like native ones. Ignored by
//! default; run with `make test-live`.

mod common;

use aibrowse_browser::{Browser, LaunchOptions};
use serde_json::json;

const PAGE: &str = r##"<!doctype html>
<title>polyfilled</title>
<script>
  // A polyfill of the kind early adopters shipped: its own registry, its own methods.
  const tools = new Map();
  navigator.modelContext = {
    _tools: tools,
    registerTool(tool) { tools.set(tool.name, tool); },
    listTools() { return [...tools.values()].map(({ name, description, inputSchema }) => ({ name, description, inputSchema })); },
  };
  navigator.modelContext.registerTool({
    name: 'lookup',
    description: 'Look an item up in the catalog',
    inputSchema: { type: 'object', properties: { item: { type: 'string' } }, required: ['item'] },
    execute: async ({ item }) => ({ content: [{ type: 'text', text: item + ': 3 in stock' }] }),
  });
</script>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn polyfilled_tools_are_bridged() {
    let port = common::serve(PAGE).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-webmcp-polyfill".into(),
        headless: true,
        mute: true,
        ..LaunchOptions::default()
    })
    .await
    .expect("launch");
    let mut tab = browser.open_tab().await.expect("tab");
    tab.navigate(&format!("http://localhost:{port}/"))
        .await
        .expect("navigate");
    assert!(tab.sync_polyfill_tools().await, "nothing was bridged");
    let tools = tab.webmcp_tools();
    let lookup = tools
        .iter()
        .find(|t| t.name == "lookup")
        .unwrap_or_else(|| panic!("not listed: {tools:?}"));
    assert!(lookup.bridged);
    assert_eq!(lookup.description, "Look an item up in the catalog");
    // A second sync changes nothing.
    assert!(!tab.sync_polyfill_tools().await);

    let done = tab
        .call_webmcp_tool("lookup", json!({ "item": "red mug" }))
        .await
        .expect("call");
    let output = done.output.expect("output").to_string();
    assert!(output.contains("red mug: 3 in stock"), "{output}");
    browser.close().await.expect("close");
}
