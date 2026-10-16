//! Driving a browser this process didn't launch, over its DevTools WebSocket, as a
//! hosted browser is driven: here a Chrome started on its own with
//! `--remote-debugging-port`. Ignored by default; run with `make test-live`.

mod common;

use std::time::Duration;

use aibrowse_browser::{Browser, LaunchOptions, find_chrome};
use common::{backend_id, page_text};

const PAGE: &str = r##"<!doctype html>
<title>remote</title>
<button id="go" onclick="this.textContent = 'clicked'">Go</button>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_remote_browser_is_driven_over_its_websocket() {
    let port = common::serve(PAGE).await;
    let data = tempfile::tempdir().expect("user data dir");
    let mut chrome = tokio::process::Command::new(find_chrome().expect("chrome"))
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--mute-audio",
            &format!("--user-data-dir={}", data.path().display()),
            "about:blank",
        ])
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start chrome");
    // Chrome writes its port and browser path to DevToolsActivePort once it listens.
    let active = data.path().join("DevToolsActivePort");
    let mut lines = Vec::new();
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&active)
            && text.lines().count() >= 2
        {
            lines = text.lines().map(str::to_owned).collect();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(lines.len() >= 2, "Chrome never reported its DevTools port");
    let url = format!("ws://127.0.0.1:{}{}", lines[0], lines[1]);

    let browser = Browser::launch(LaunchOptions {
        profile: "test-remote".into(),
        headless: true,
        mute: true,
        remote: Some(url),
        ..LaunchOptions::default()
    })
    .await
    .expect("attach");
    assert!(browser.is_remote());
    let mut tab = browser.open_tab().await.expect("tab");
    tab.navigate(&format!("http://localhost:{port}/"))
        .await
        .expect("navigate");
    let go = backend_id(&tab, "#go").await;
    tab.click(go).await.expect("click");
    assert_eq!(page_text(&tab, "#go", "textContent").await, "clicked");

    // Closing the session leaves the remote browser running: its owner ends it.
    browser.close().await.expect("close");
    assert!(
        chrome.try_wait().expect("status").is_none(),
        "the remote Chrome was closed"
    );
    chrome.kill().await.expect("stop chrome");
}
