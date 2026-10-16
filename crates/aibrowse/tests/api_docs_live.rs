//! Documented APIs in `api_list`, against a real Chrome: a site's OpenAPI descriptions,
//! found at the conventional path of the origin its page calls and where its own page
//! loaded one (JSON or YAML), merged with the calls the page was seen making.

mod common;

use common::{Client, serve_pages, set_policy};
use serde_json::json;

/// The app calls its API, and loads descriptions from unconventional addresses, as
/// Swagger UI and Redoc pages do.
const APP: &str = r#"<!doctype html><title>Pet Store</title><h1>Pet Store</h1><ul id="pets"></ul>
<script>
fetch('/api/pets/8f3a9c2e41').then(r => r.json()).then(p => pets.innerHTML = '<li>' + p.name + '</li>');
fetch('/internal/spec.json').then(r => r.json());
fetch('/internal/reports.yaml').then(r => r.text());
</script>"#;
const PET: &str = r#"{"id": "8f3a9c2e41", "name": "Rex", "kind": "dog"}"#;
const OWNER: &str = r#"{"name": "Ada", "since": 2019}"#;
const CONVENTIONAL: &str = r#"{"openapi": "3.0.3", "info": {"title": "Pets"},
  "servers": [{"url": "/api"}],
  "paths": {
    "/pets/{petId}": {"get": {"summary": "Show a pet", "parameters": [{"name": "petId", "in": "path", "required": true}]}},
    "/pets/{petId}/owner": {"get": {"summary": "Who owns a pet"}},
    "/pets": {"post": {"summary": "Add a pet", "requestBody": {"content": {"application/json": {}}}}}
  }}"#;
const LOADED: &str = r#"{"swagger": "2.0", "basePath": "/stats",
  "paths": {"/visits": {"get": {"summary": "Visits per day", "parameters": [{"name": "days", "in": "query"}]}}}}"#;
const LOADED_YAML: &str = "swagger: 2.0
basePath: /reports
paths:
  /monthly:
    get:
      summary: Monthly report
      responses:
        200: { description: The report }
";
const PAGES: &[(&str, &str)] = &[
    ("/app", APP),
    ("/api/pets/8f3a9c2e41/owner", OWNER),
    ("/api/pets/8f3a9c2e41", PET),
    ("/openapi.json", CONVENTIONAL),
    ("/internal/spec.json", LOADED),
    ("/internal/reports.yaml", LOADED_YAML),
];

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn api_list_shows_what_the_site_documents() {
    set_policy("test-api-docs", r#"{"confirm": "off"}"#);
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-api-docs", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/app") }),
    );
    let listed = c.ok("api_list", json!({}));
    let line = |needle: &str| {
        listed
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no {needle:?} in {listed}"))
            .to_owned()
    };
    let seen = line("/api/pets/{");
    assert!(
        seen.contains("1× 200") && seen.contains("Show a pet"),
        "{seen}"
    );
    assert!(line("POST /api/pets").contains("documented, not called yet: Add a pet"));
    assert!(
        line("/stats/visits").contains("Visits per day"),
        "from the page's own description"
    );
    assert!(
        line("/reports/monthly").contains("Monthly report"),
        "from the page's own YAML description"
    );

    let add = line("POST /api/pets")
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let detail = c.ok("api_list", json!({ "endpoint": add }));
    assert!(
        detail.contains("Takes a body: application/json"),
        "{detail}"
    );

    // A documented endpoint the page never called is callable all the same.
    let owner = line("/owner").split_whitespace().next().unwrap().to_owned();
    let out = c.ok(
        "api_call",
        json!({ "endpoint": owner, "path": { "petId": "8f3a9c2e41" } }),
    );
    assert!(out.starts_with("200") && out.contains("Ada"), "{out}");
    c.ok("session_close", json!({}));
}

/// The API lives on another origin than the docs (127.0.0.1 vs localhost) and sends no
/// CORS headers, so the docs page may not call it: the call goes out from that origin.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_documented_api_on_another_origin_is_called_from_there() {
    set_policy("test-api-docs-cors", r#"{"confirm": "off"}"#);
    let api = serve_pages(&[
        (
            "/stock/blue-teapot",
            r#"{"sku": "blue-teapot", "in_stock": 7}"#,
        ),
        ("/robots.txt", "User-agent: *"),
    ]);
    let spec: &'static str = Box::leak(
        format!(
            r#"{{"openapi": "3.0.3", "servers": [{{"url": "http://127.0.0.1:{api}"}}],
  "paths": {{"/stock/{{sku}}": {{"get": {{"summary": "Stock for a product",
  "parameters": [{{"name": "sku", "in": "path", "required": true}}]}}}}}}}}"#
        )
        .into_boxed_str(),
    );
    let docs: &'static [(&'static str, &'static str)] = Box::leak(Box::new([
        (
            "/docs",
            r#"<!doctype html><title>Stock API docs</title><h1>Stock API</h1>
<script>fetch('/docs/spec.json').then(r => r.json());</script>"#,
        ),
        ("/docs/spec.json", spec),
    ]));
    let port = serve_pages(docs);
    let mut c = Client::spawn("test-api-docs-cors", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/docs") }),
    );
    let listed = c.ok("api_list", json!({}));
    let stock = listed
        .lines()
        .find(|l| l.trim_start().starts_with('a') && l.contains("/stock/{"))
        .unwrap_or_else(|| panic!("no stock endpoint in {listed}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let out = c.ok(
        "api_call",
        json!({ "endpoint": stock, "path": { "sku": "blue-teapot" } }),
    );
    assert!(
        out.contains("sent from a tab on http://127.0.0.1") && out.contains("\"in_stock\":7"),
        "{out}"
    );
    c.ok("session_close", json!({}));
}
