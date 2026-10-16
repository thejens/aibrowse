//! What a search's branch leaves out on a listing (fixtures/html/paging.html): the pages
//! after it, never its items, and without touching the page. Ignored by default; run
//! with `make test-live`.

mod common;

use common::{Client, serve_pages};
use serde_json::json;

const PAGING: &str = include_str!("../../../fixtures/html/paging.html");
/// The script a search's branches run on every page (crates/navigator/src/fanout.rs).
const PAGING_LINKS: &str = include_str!("../../navigator/src/paging_links.js");

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_listings_next_pages_are_found_and_its_items_are_not() {
    let port = serve_pages(&[("/", PAGING)]);
    let mut c = Client::spawn("test-search-paging", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let found = c.ok("evaluate", json!({ "expression": PAGING_LINKS }));
    // The links that page through `sida`, the pager's by its class, and rel=next.
    for paging in ["?sida=2", "?sida=3", "?offset=older", "/arkiv/sida-tva"] {
        assert!(found.contains(paging), "{paging} in {found}");
    }
    for item in ["/notice/481", "/notice/480", "?kategori=3"] {
        assert!(!found.contains(item), "{item} in {found}");
    }
    // Read only: the page still shows all of it.
    let after = c.ok("observe", json!({ "detail": "all" }));
    for shown in ["Nästa sida", "Äldre", "Fler", "Kött"] {
        assert!(after.contains(shown), "{shown} in {after}");
    }
    c.ok("session_close", json!({}));
}
