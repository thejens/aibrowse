//! The navigator in a single-page app, where links change the address without loading
//! a new document: a step there can still be gone back on. Ignored by default; run with
//! `make test-live`.

mod common;

use common::{Client, has_jev_key, serve_pages};
use serde_json::json;

/// A price-comparison app: a home page to search from, results, and product views drawn
/// by script under their own addresses. The closer-named result's product is gone; the
/// other's lists prices.
const APP: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>PriceFinder</title>
<body><header><a href="/" data-route>PriceFinder home</a></header><main id="view"></main>
<script>
  const views = {
    '/': () => `<h1>Find the best price</h1><input aria-label="What are you looking for?">
        <p>Compare prices from 1,000 shops.</p>`,
    '/search': () => `<h1>Results for Widget Pro 2</h1><ul>
        <li><a href="/p/1" data-route>Widget Pro 2 USB-C White</a></li>
        <li><a href="/p/2" data-route>Widget Pro 2 (2nd generation)</a></li></ul>`,
    '/p/1': () => `<p>This product is no longer listed.</p>`,
    '/p/2': () => `<h1>Widget Pro 2 USB-C: compare prices</h1><table>
        <tr><th>Shop</th><th>Price</th></tr>
        <tr><td>ShopOne</td><td>2 190 kr</td></tr>
        <tr><td>ShopTwo</td><td>2 249 kr</td></tr>
        <tr><td>ShopThree</td><td>2 399 kr</td></tr></table>`,
  };
  const draw = () => {
    document.getElementById('view').innerHTML = (views[location.pathname] || views['/'])();
    document.title = location.pathname === '/p/2' ? 'Widget Pro 2 prices' : 'PriceFinder';
  };
  document.addEventListener('click', (e) => {
    const link = e.target.closest('a[data-route]');
    if (!link) return;
    e.preventDefault();
    history.pushState({}, '', link.getAttribute('href'));
    draw();
  });
  addEventListener('popstate', draw);
  draw();
</script></body></html>"#;

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn a_wrong_turn_in_a_single_page_app_is_gone_back_on() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    let port = serve_pages(&[("/", APP)]);
    let mut c = Client::spawn("test-spa", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/search"), "observe": false }),
    );
    let out = c.ok(
        "navigate",
        json!({
            "goal": "the price comparison page for Widget Pro 2 USB-C, listing shops and their prices",
            "observe": false,
        }),
    );
    let at = c.ok("evaluate", json!({ "expression": "location.pathname" }));
    assert!(
        at.contains("/p/2") && out.contains("Reached"),
        "ended at {at}:\n{out}"
    );
    c.ok("session_close", json!({}));
}

/// A journey search whose station fields take a station only when one of their
/// suggestions is chosen (Enter keeps the typed text and chooses nothing, as on SJ's),
/// and whose results are cards drawn as buttons.
const SEARCH: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rail</title>
<body><main><h1>Find a train</h1>
<div><label for="from">From</label><input id="from" role="combobox" aria-autocomplete="list" aria-expanded="false" aria-controls="from-list" autocomplete="off">
  <ul id="from-list" role="listbox" aria-label="Stations matching From"></ul></div>
<div><label for="to">To</label><input id="to" role="combobox" aria-autocomplete="list" aria-expanded="false" aria-controls="to-list" autocomplete="off">
  <ul id="to-list" role="listbox" aria-label="Stations matching To"></ul></div>
<button type="button" id="search">Search trains</button><p id="error" role="alert"></p></main>
<script>
  const STATIONS = ['Northtown Central', 'Northtown Harbour', 'Southport Central', 'Southport Airport'];
  for (const id of ['from', 'to']) {
    const input = document.getElementById(id);
    const list = document.getElementById(id + '-list');
    input.addEventListener('input', () => {
      input.dataset.station = '';
      const typed = input.value.trim().toLowerCase();
      const found = typed.length < 2 ? [] : STATIONS.filter((s) => s.toLowerCase().startsWith(typed));
      list.innerHTML = found.map((s) => `<li role="option">${s}</li>`).join('');
      input.setAttribute('aria-expanded', String(found.length > 0));
    });
    list.addEventListener('click', (e) => {
      const option = e.target.closest('[role=option]');
      if (!option) return;
      input.value = option.textContent;
      input.dataset.station = option.textContent;
      list.innerHTML = '';
      input.setAttribute('aria-expanded', 'false');
    });
  }
  document.getElementById('search').addEventListener('click', () => {
    const from = document.getElementById('from').dataset.station;
    const to = document.getElementById('to').dataset.station;
    if (!from || !to) {
      document.getElementById('error').textContent = 'Choose the stations from the lists.';
      return;
    }
    location.href = '/results?from=' + encodeURIComponent(from) + '&to=' + encodeURIComponent(to);
  });
</script></body></html>"#;

const RESULTS: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rail</title>
<body><main><h1>Choose your journey</h1><h2>Matching journeys</h2>
<button><span>Leaves 06:04</span> <span>arrives 09:47</span> <span>from 555 kr</span> <span>3 h 43 min, no changes</span></button>
<button><span>Leaves 07:19</span> <span>arrives 10:47</span> <span>from 612 kr</span> <span>3 h 28 min, no changes</span></button>
<button><span>Leaves 08:04</span> <span>arrives 11:41</span> <span>from 498 kr</span> <span>3 h 37 min, no changes</span></button>
</main></body></html>"#;

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn a_search_form_with_suggestions_is_filled_and_run() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    let port = serve_pages(&[("/", SEARCH), ("/results", RESULTS)]);
    let mut c = Client::spawn("test-spa-form", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/"), "observe": false }),
    );
    let out = c.ok(
        "navigate",
        json!({
            "goal": "the list of trains from Northtown Central to Southport Central, with their times and prices",
            "texts": ["Northtown Central", "Southport Central"],
            "observe": false,
        }),
    );
    let at = c.ok(
        "evaluate",
        json!({ "expression": "location.pathname + location.search" }),
    );
    assert!(
        at.contains("/results?from=Northtown%20Central&to=Southport%20Central")
            && out.contains("Reached"),
        "ended at {at}:\n{out}"
    );
    // Typing into a station field chose its suggestion, as a person does: Enter here
    // chooses nothing.
    assert!(
        out.contains("and chose the suggestion \"Northtown Central\""),
        "{out}"
    );
    // The results are buttons, and read as the page's text.
    let text = c.ok("read", json!({}));
    assert!(
        text.contains("Leaves 06:04") && text.contains("from 498 kr"),
        "{text}"
    );
    c.ok("session_close", json!({}));
}
