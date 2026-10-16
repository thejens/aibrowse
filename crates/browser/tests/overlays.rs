//! Clicks where the topmost node is a pseudo-element, a common pattern in real sites:
//! buttons that extend their hit area with `::after`, and cards made clickable by a
//! "stretched link" whose `::after` covers the whole card. Ignored by default; run with
//! `make test-live`.

mod common;

use aibrowse_browser::{Browser, LaunchOptions, World};
use common::{backend_id, page_text};

const PAGE: &str = r##"<!doctype html>
<title>overlays</title>
<style>
  button { font-size: 18px }
  #own { position: relative; margin: 40px }
  #own::after { content: ''; position: absolute; inset: -12px }
  .card { position: relative; width: 420px; padding: 24px; margin: 40px; border: 1px solid #ccc }
  .card a::after { content: ''; position: absolute; inset: 0; z-index: 1 }
  #raised { position: relative; z-index: 2 }
</style>
<button id="own" onclick="this.textContent = 'own clicked'">Own hit area</button>
<div class="card"><a href="#card-a">Card A</a> <button id="raised" onclick="this.textContent = 'raised clicked'">Raised</button></div>
<div class="card"><a href="#card-b">Card B</a> <button id="under" onclick="this.textContent = 'under clicked'">Under</button></div>
<div id="host" style="margin: 40px"></div>
<script>
  // A button in an open shadow root, its hit area extended by its own ::before (Otto's
  // filter buttons): a point there resolves to the shadow host from the document.
  const root = document.getElementById('host').attachShadow({ mode: 'open' });
  root.innerHTML = `<style>
    button { position: relative; font-size: 18px }
    button::before { content: ''; position: absolute; inset: -12px }
  </style><button onclick="this.textContent = 'shadow clicked'">More filters</button>`;
</script>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn pseudo_element_hits_are_attributed_to_their_element() {
    let port = common::serve(PAGE).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-overlays".into(),
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

    // The button's own ::after belongs to it: the click goes through.
    let own = backend_id(&tab, "#own").await;
    tab.click(own).await.expect("click own");
    assert_eq!(page_text(&tab, "#own", "textContent").await, "own clicked");

    // A stretched link's ::after covers the card, but this button is raised above it.
    let raised = backend_id(&tab, "#raised").await;
    tab.click(raised).await.expect("click raised");
    assert_eq!(
        page_text(&tab, "#raised", "textContent").await,
        "raised clicked"
    );

    // Inside a shadow root, the button's own ::before belongs to it as well.
    let shadow = shadow_button(&tab).await;
    tab.click(shadow).await.expect("click in a shadow root");
    assert_eq!(
        page_text(
            &tab,
            "#host",
            "shadowRoot.querySelector('button').textContent"
        )
        .await,
        "shadow clicked"
    );

    // Here the overlay really is on top: a person's click would open the card's link.
    let under = backend_id(&tab, "#under").await;
    let err = tab.click(under).await.expect_err("covered");
    assert!(err.to_string().contains("covered"), "{err}");
    assert_eq!(page_text(&tab, "#under", "textContent").await, "Under");

    browser.close().await.expect("close");
}

/// The button in `#host`'s shadow root.
async fn shadow_button(tab: &aibrowse_browser::Tab) -> aibrowse_browser::ElementRef {
    let session = tab.session();
    let found = session
        .call(
            "Runtime.evaluate",
            serde_json::json!({ "expression": "document.getElementById('host').shadowRoot.querySelector('button')" }),
        )
        .await
        .unwrap();
    let node = session
        .call(
            "DOM.describeNode",
            serde_json::json!({ "objectId": found["result"]["objectId"] }),
        )
        .await
        .unwrap();
    aibrowse_browser::ElementRef::new(0, node["node"]["backendNodeId"].as_i64().unwrap())
}

/// A search box a language picker is drawn over (who.int), and one under a modal.
const FIELDS: &str = r##"<!doctype html>
<title>covered fields</title>
<style>
  .bar { position: relative; height: 48px; width: 400px }
  .bar input, .bar select { position: absolute; top: 0; right: 0; width: 200px; height: 48px }
  #modal { position: fixed; top: 100px; left: 0; right: 0; height: 120px; background: rgba(0,0,0,.4) }
</style>
<div class="bar">
  <input id="search" type="search" placeholder="Search">
  <select id="language"><option>Select language</option><option>Français</option></select>
</div>
<input id="behind" placeholder="Behind the dialog" style="margin-top: 80px">
<div id="modal" role="dialog" aria-modal="true"><button>Accept</button></div>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_field_under_another_control_is_typed_into_from_the_keyboard() {
    let port = common::serve(FIELDS).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-covered-fields".into(),
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

    let search = backend_id(&tab, "#search").await;
    let done = tab
        .fill(search, "malaria")
        .await
        .expect("fill under a select");
    assert_eq!(page_text(&tab, "#search", "value").await, "malaria");
    assert!(
        done.notes
            .iter()
            .any(|n| n.contains("focused from the keyboard")),
        "{:?}",
        done.notes
    );
    assert_eq!(
        page_text(&tab, "#language", "value").await,
        "Select language",
        "the picker on top is left alone"
    );

    // A modal over a field is the page asking for something first.
    let behind = backend_id(&tab, "#behind").await;
    let err = tab.fill(behind, "x").await.expect_err("under a modal");
    assert!(err.to_string().contains("covered"), "{err}");
    assert_eq!(page_text(&tab, "#behind", "value").await, "");

    browser.close().await.expect("close");
}

/// A ref that names the page's own frame (a ref from the previous page, renumbered in
/// the new document) is refused as stale instead of clicking the middle of the page.
#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_ref_to_the_document_itself_is_stale() {
    let port = common::serve(
        r#"<!doctype html><title>root</title><body onclick="document.title = 'clicked'"><p>Text</p></body>"#,
    )
    .await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-stale-root".into(),
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
    for selector in ["html", "body"] {
        let root = backend_id(&tab, selector).await;
        let err = tab.click(root).await.expect_err("the document's frame");
        assert!(err.to_string().contains("observe again"), "{err}");
    }
    assert_eq!(page_text(&tab, "title", "textContent").await, "root");
    browser.close().await.expect("close");
}

/// A date button under its own read-only twin (FlixBus), and a button under a modal's
/// backdrop, which is `aria-hidden` as well (MUI's).
const TWINS: &str = r##"<!doctype html>
<title>twins</title>
<style>
  .when { position: relative; width: 280px; height: 36px; margin: 40px }
  .when button { position: absolute; inset: 0 }
  .when .field { position: absolute; inset: 1px }
  .when input { width: 100%; height: 100%; box-sizing: border-box }
  #backdrop { position: fixed; inset: 0; background: rgba(0,0,0,.4) }
</style>
<div class="when">
  <button id="date" type="button" aria-expanded="false">Departure Today</button>
  <div class="field">
    <label for="date-input"><span aria-hidden="true">Departure</span></label>
    <input id="date-input" readonly aria-hidden="true" tabindex="-1" value="Today">
  </div>
</div>
<button id="behind" style="margin: 40px">Behind the backdrop</button>
<div id="backdrop" aria-hidden="true" hidden></div>
<script>
  // The widget opens its calendar for a click anywhere on it, as FlixBus's does.
  document.querySelector('.when').addEventListener('click', () => {
    document.getElementById('date').setAttribute('aria-expanded', 'true');
  });
  document.getElementById('behind').addEventListener('click', (e) => { e.target.textContent = 'clicked'; });
</script>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_button_under_its_own_inert_twin_is_clicked_through_it() {
    let port = common::serve(TWINS).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-twins".into(),
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

    let date = backend_id(&tab, "#date").await;
    tab.click(date).await.expect("click under the twin");
    assert_eq!(
        page_text(&tab, "#date", "getAttribute('aria-expanded')").await,
        "true"
    );

    // A backdrop is aria-hidden too, but it is the page asking for something first.
    tab.evaluate(
        "document.getElementById('backdrop').hidden = false",
        World::Isolated,
    )
    .await
    .expect("show the backdrop");
    let behind = backend_id(&tab, "#behind").await;
    let err = tab.click(behind).await.expect_err("under a backdrop");
    assert!(err.to_string().contains("covered"), "{err}");
    assert_eq!(
        page_text(&tab, "#behind", "textContent").await,
        "Behind the backdrop"
    );

    browser.close().await.expect("close");
}
