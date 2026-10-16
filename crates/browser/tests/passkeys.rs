//! Passkey requests in headless Chrome: with no window there is no passkey dialog to
//! answer, so a sign-in page that asks for one (as Google's does) must be told no at
//! once, the way a person declining would, so it offers another way in. Ignored by
//! default; run with `make test-live`.

mod common;

use std::time::{Duration, Instant};

use aibrowse_browser::{Browser, LaunchOptions, World};

// Asks for a passkey on load, as sign-in challenge pages do, and records how it ends.
const PAGE: &str = r##"<!doctype html>
<title>passkey</title>
<p id="outcome">pending</p>
<script>
  navigator.credentials.get({ publicKey: {
    challenge: new Uint8Array(32), rpId: 'localhost', userVerification: 'required', timeout: 60000,
  } }).then(() => { document.getElementById('outcome').textContent = 'signed in'; },
            (e) => { document.getElementById('outcome').textContent = e.name; });
</script>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn headless_passkey_requests_are_declined() {
    let port = common::serve(PAGE).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-passkeys".into(),
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

    let started = Instant::now();
    let outcome = loop {
        let outcome = tab
            .evaluate(
                "document.getElementById('outcome').textContent",
                World::Isolated,
            )
            .await
            .expect("read outcome");
        if outcome != "pending" || started.elapsed() > Duration::from_secs(10) {
            break outcome;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(outcome, "NotAllowedError");
    browser.close().await.expect("close");
}
