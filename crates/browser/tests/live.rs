//! Live tests against a real Chrome. Ignored by default; run with `make test-live`.

mod common;

use std::time::Duration;

use aibrowse_browser::{Browser, LaunchOptions};
use common::{backend_id, page_text};

const PAGE: &str = r#"<!doctype html>
<title>live</title>
<style>body { height: 3000px } #far { position: absolute; top: 2200px }</style>
<input id="name">
<button id="go" onclick="this.textContent = 'clicked:' + event.isTrusted">Go</button>
<button id="far" onclick="this.textContent = 'far clicked'">Far</button>
<script>
  window.moves = 0;
  addEventListener('mousemove', () => window.moves++);
</script>"#;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn humanized_input_reaches_the_page_as_trusted_events() {
    for headless in common::modes() {
        input_round_trip(headless).await;
    }
}

async fn input_round_trip(headless: bool) {
    let browser = Browser::launch(LaunchOptions {
        profile: "test-live".into(),
        headless,
        mute: true,
        ..LaunchOptions::default()
    })
    .await
    .expect("launch");
    let mut tab = browser.open_tab().await.expect("tab");

    let url = format!("data:text/html,{}", PAGE.replace('#', "%23"));
    let nav = tab.navigate(&url).await.expect("navigate");
    assert!(nav.navigated && nav.settled, "{nav:?}");

    let go = backend_id(&tab, "#go").await;
    let click = tab.click(go).await.expect("click");
    assert!(click.settled, "{click:?}");
    assert_eq!(page_text(&tab, "#go", "textContent").await, "clicked:true");
    let moves: u32 = page_text(&tab, "body", "ownerDocument.defaultView.moves")
        .await
        .parse()
        .unwrap();
    assert!(
        moves > 3,
        "the cursor must travel to the button, saw {moves} mousemoves"
    );

    let input = backend_id(&tab, "#name").await;
    let fill = tab.fill(input, "Ada Lovelace!").await.expect("fill");
    assert!(fill.notes.is_empty(), "{fill:?}");
    assert_eq!(page_text(&tab, "#name", "value").await, "Ada Lovelace!");

    let far = backend_id(&tab, "#far").await;
    tab.click(far).await.expect("click far");
    assert_eq!(page_text(&tab, "#far", "textContent").await, "far clicked");
    assert!(
        tab.viewport().await.unwrap().scroll_y > 1000.0,
        "must have wheel-scrolled down"
    );

    let shot = tab.screenshot().await.expect("screenshot");
    let viewport = tab.viewport().await.unwrap();
    assert_eq!(
        common::jpeg_size(&shot),
        (viewport.width as u32, viewport.height as u32),
        "headless={headless}: screenshot must match the CSS viewport"
    );

    let webdriver = page_text(
        &tab,
        "body",
        "ownerDocument.defaultView.navigator.webdriver",
    )
    .await;
    println!("navigator.webdriver = {webdriver}");

    browser.close().await.expect("close");
}

const LINKS: &str = r#"<!doctype html>
<title>links</title>
<a id="other" href="/?other">Other</a>
<script>
  window.clicks = [];
  addEventListener('click', (e) => window.clicks.push(
    `${e.isTrusted}:${e.metaKey || e.ctrlKey}`));
</script>"#;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_modifier_click_opens_a_link_in_a_background_tab() {
    let port = common::serve(LINKS).await;
    for headless in common::modes() {
        let browser = Browser::launch(LaunchOptions {
            profile: "test-background-tabs".into(),
            headless,
            mute: true,
            ..LaunchOptions::default()
        })
        .await
        .expect("launch");
        let mut tab = browser.open_tab().await.expect("tab");
        let home = format!("http://localhost:{port}/");
        tab.navigate(&home).await.expect("navigate");

        let link = backend_id(&tab, "#other").await;
        let mark = browser.opened_mark();
        let opened = tab.open_in_background(link).await.expect("open");
        assert!(
            !opened.result.navigated,
            "the page itself stays: {opened:?}"
        );
        assert_eq!(opened.url, format!("{home}?other"));
        assert_eq!(tab.url(), home);
        assert_eq!(
            page_text(&tab, "body", "ownerDocument.defaultView.clicks.join()").await,
            "true:true",
            "headless={headless}: one trusted click with Cmd/Ctrl held"
        );

        let mut popup = browser
            .take_opened(mark, &opened.url, Duration::from_secs(2))
            .await
            .expect("attach")
            .unwrap_or_else(|| panic!("headless={headless}: no background tab"));
        assert!(popup.wait_settled(Duration::from_secs(10)).await);
        assert_eq!(popup.url(), format!("{home}?other"));
        browser.close().await.expect("close");
    }
}

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn navigating_to_the_same_address_loads_it_again() {
    let port = common::serve(PAGE).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-same-address".into(),
        headless: true,
        mute: true,
        ..LaunchOptions::default()
    })
    .await
    .expect("launch");
    let mut tab = browser.open_tab().await.expect("tab");
    // With a fragment, as Google Sheets' addresses have (`…/edit#gid=0`): Chrome alone
    // would only jump to it, and the page would stay as it was.
    let url = format!("http://localhost:{port}/#part");
    tab.navigate(&url).await.expect("navigate");
    tab.evaluate_isolated_raw("document.body.dataset.mark = 'old'")
        .await
        .expect("mark");
    let again = tab.navigate(&url).await.expect("navigate again");
    assert!(again.navigated, "{again:?}");
    assert_eq!(
        page_text(&tab, "body", "dataset.mark || 'fresh'").await,
        "fresh"
    );
    browser.close().await.expect("close");
}

// A page that moves under the hand: while the cursor is on its way, a panel above the
// button opens and pushes it down, and a decoy takes its old place.
const SHIFTING: &str = r#"<!doctype html>
<title>shifting</title>
<div id="panel" style="height:0"></div>
<div style="position:relative">
  <button id="decoy" style="position:absolute;top:200px;left:300px;width:160px;height:40px"
    onclick="this.textContent = 'decoy clicked'">Decoy</button>
  <button id="target" style="position:absolute;top:0;left:300px;width:160px;height:40px"
    onclick="this.textContent = 'target clicked'">Target</button>
</div>
<script>
  // Moves shortly after the cursor sets off, on its own timing, as a side panel
  // finishing its close does: the hand is still on its way.
  // The hand's travel to the button starts after a pause in the cursor's movement.
  let last = 0;
  let armed = false;
  addEventListener('mousemove', () => {
    const now = performance.now();
    const travelling = last > 0 && now - last > 150;
    last = now;
    if (armed || !travelling) return;
    armed = true;
    setTimeout(() => {
      document.getElementById('panel').style.height = '200px';
      document.getElementById('decoy').style.top = '-200px';
    }, 100);
  });
</script>"#;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_click_follows_an_element_the_page_moves() {
    let port = common::serve(SHIFTING).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-shifting".into(),
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
    let target = backend_id(&tab, "#target").await;
    tab.click(target).await.expect("click");
    assert_eq!(
        page_text(&tab, "#target", "textContent").await,
        "target clicked"
    );
    assert_eq!(page_text(&tab, "#decoy", "textContent").await, "Decoy");
    browser.close().await.expect("close");
}

// An app's fixed layout, as Excel for the web's: the toolbar and the grid fill the
// window, nothing scrolls, and a helper field sits outside the visible area.
const FIXED_APP: &str = r#"<!doctype html>
<title>fixed app</title>
<style>html, body { margin: 0; height: 100%; overflow: hidden }
  #app { position: relative; height: 100vh; overflow: hidden }
  #toolbar { height: 60px; background: #ddd } #helper { position: absolute; top: -200px }</style>
<div id="app"><div id="toolbar"><input id="name" aria-label="Name Box" value="A1"></div>
  <textarea id="helper" aria-label="helper"></textarea><div style="height: 2000px">grid</div></div>"#;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_fixed_layout_is_never_scrolled_to_reach_a_hidden_field() {
    let port = common::serve(FIXED_APP).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-fixed-app".into(),
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
    let helper = backend_id(&tab, "#helper").await;
    let err = tab.fill(helper, "1").await.expect_err("out of reach");
    assert!(err.to_string().contains("doesn't scroll"), "{err}");
    // The layout is where it was: the toolbar still at the top.
    assert_eq!(page_text(&tab, "#app", "scrollTop").await, "0");
    assert_eq!(
        page_text(&tab, "#toolbar", "getBoundingClientRect().top").await,
        "0"
    );
    browser.close().await.expect("close");
}

/// A date field that opens its picker on focus and takes no typing (Trenitalia's), and
/// a code editor where Enter adds a line.
const FIELDS: &str = r#"<!doctype html><title>fields</title>
<input id="date" placeholder="Date" onfocus="document.getElementById('picker').focus()"
  onkeydown="event.preventDefault()">
<div id="picker" tabindex="0">October 2026</div>
<div id="editor" contenteditable="true" style="min-height: 2em"></div>
<select id="max"><option value="">No max</option><option value="150000">£150,000</option>
<option value="200000">£200,000</option></select>"#;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_field_that_takes_no_typing_says_so_and_enter_adds_no_line() {
    let port = common::serve(FIELDS).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-fields".into(),
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
    let done = tab.fill(date, "20/11/2026").await.expect("fill");
    assert!(
        done.notes
            .iter()
            .any(|n| n == aibrowse_browser::FIELD_STAYED_EMPTY),
        "{:?}",
        done.notes
    );

    let editor = backend_id(&tab, "#editor").await;
    tab.fill(editor, "\\d{3}").await.expect("fill");
    let done = tab.submit_field().await.expect("enter");
    assert!(
        done.notes.first().is_some_and(|n| n.contains("taken back")),
        "{:?}",
        done.notes
    );
    assert_eq!(page_text(&tab, "#editor", "innerText").await, "\\d{3}");

    // A native dropdown is set from the text: the option its digits name.
    let max = backend_id(&tab, "#max").await;
    tab.fill(max, "200000").await.expect("fill a select");
    assert_eq!(page_text(&tab, "#max", "value").await, "200000");
    browser.close().await.expect("close");
}
