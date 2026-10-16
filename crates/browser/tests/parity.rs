//! Headful and headless must be indistinguishable to a page, and stay so across
//! relaunches of the same profile. Ignored by default; run with `make test-live`.

mod common;

use aibrowse_browser::{Browser, LaunchOptions};
use serde_json::{Value, json};

/// Everything a fingerprinting script commonly reads, from the page and from a worker.
const FINGERPRINT: &str = r#"(async () => {
  const gl = document.createElement('canvas').getContext('webgl');
  const dbg = gl && gl.getExtension('WEBGL_debug_renderer_info');
  const hints = await navigator.userAgentData.getHighEntropyValues(
    ['architecture', 'bitness', 'fullVersionList', 'model', 'platformVersion', 'uaFullVersion', 'wow64']);
  // Media features CreepJS hashes; headless renders without a real display behind it.
  // (dynamic-range is left out: on HDR panels it is a known, unfixable gap, see
  // Display::media_features.)
  const media = Object.fromEntries([
    'color-gamut: srgb', 'color-gamut: p3', 'color-gamut: rec2020',
    'prefers-color-scheme: dark', 'prefers-reduced-motion: reduce',
    'prefers-contrast: more', 'prefers-reduced-transparency: reduce', 'forced-colors: active',
    'inverted-colors: inverted', 'monochrome', 'hover: hover', 'any-hover: hover',
    'pointer: fine', 'any-pointer: fine', 'display-mode: browser', 'update: fast',
  ].map((q) => [q, matchMedia(`(${q})`).matches]));
  const worker = await new Promise((resolve) => {
    const src = 'navigator.userAgentData.getHighEntropyValues(["fullVersionList"])' +
      '.then(h => postMessage([navigator.userAgent, h.fullVersionList]))';
    const w = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
    w.onmessage = (e) => resolve(e.data);
  });
  // Service workers are browser-level targets, not under the page: they need their own
  // identity override.
  await navigator.serviceWorker.register('/script.js');
  const sw = (await navigator.serviceWorker.ready).active;
  const serviceWorker = await new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = (e) => resolve(e.data);
    sw.postMessage('probe', [channel.port2]);
  });
  return {
    userAgent: navigator.userAgent, hints, worker, serviceWorker, media, platform: navigator.platform,
    webdriver: navigator.webdriver, languages: navigator.languages,
    plugins: navigator.plugins.length, mimeTypes: navigator.mimeTypes.length,
    hardwareConcurrency: navigator.hardwareConcurrency, deviceMemory: navigator.deviceMemory,
    maxTouchPoints: navigator.maxTouchPoints, pdfViewer: navigator.pdfViewerEnabled,
    screen: [screen.width, screen.height, screen.availWidth, screen.availHeight, screen.colorDepth],
    inner: [innerWidth, innerHeight], outer: [outerWidth, outerHeight], position: [screenX, screenY],
    devicePixelRatio, hasFocus: document.hasFocus(), visibility: document.visibilityState,
    notification: Notification.permission,
    permission: (await navigator.permissions.query({ name: 'notifications' })).state,
    webgl: dbg && [gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL), gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)],
    chrome: [typeof window.chrome, !!(window.chrome && window.chrome.app)],
    timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  };
})()"#;

const PAGE: &str = "<!doctype html><title>parity</title><h1>Parity</h1>";

/// A service worker that answers with what it sees of the browser's identity.
const SERVICE_WORKER: &str = r#"
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('message', async (e) => {
  const h = await navigator.userAgentData.getHighEntropyValues(
    ['fullVersionList', 'platformVersion', 'uaFullVersion']);
  e.ports[0].postMessage([navigator.userAgent, h.platformVersion, h.uaFullVersion, h.fullVersionList]);
});
"#;

async fn fingerprint(headless: bool, port: u16) -> (Value, (u32, u32), (f64, f64)) {
    let browser = Browser::launch(LaunchOptions {
        profile: "test-parity".into(),
        headless,
        mute: true,
        ..LaunchOptions::default()
    })
    .await
    .expect("launch");
    let mut tab = browser.open_tab().await.expect("tab");
    tab.navigate(&format!("http://localhost:{port}/"))
        .await
        .expect("navigate");
    // Test-only main-world evaluation; production code never runs script in the page.
    let response = tab
        .session()
        .call(
            "Runtime.evaluate",
            json!({ "expression": FINGERPRINT, "returnByValue": true, "awaitPromise": true }),
        )
        .await
        .expect("evaluate");
    let print = response["result"]["value"].clone();
    assert!(print.is_object(), "fingerprint script failed: {response}");
    let shot = common::jpeg_size(&tab.screenshot().await.expect("screenshot"));
    let viewport = tab.viewport().await.expect("viewport");
    browser.close().await.expect("close");
    (print, shot, (viewport.width, viewport.height))
}

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn headless_is_indistinguishable_from_headful() {
    let port = common::serve_with_script(PAGE, SERVICE_WORKER).await;
    let mut prints = Vec::new();
    // Each mode twice: the second launch reuses the profile, which once broke geometry.
    for headless in common::modes().into_iter().flat_map(|m| [m, m]) {
        let (print, shot, viewport) = fingerprint(headless, port).await;

        assert_eq!(print["webdriver"], false);
        let ua = print["userAgent"].as_str().unwrap();
        assert!(ua.contains(" Chrome/") && !ua.contains("Headless"), "{ua}");
        assert_eq!(
            print["worker"][0], print["userAgent"],
            "workers must report the same UA"
        );
        assert!(
            print["hints"]["fullVersionList"]
                .as_array()
                .is_some_and(|l| !l.is_empty()),
            "client hints must be complete: {}",
            print["hints"]
        );
        assert_eq!(print["worker"][1], print["hints"]["fullVersionList"]);
        assert_eq!(print["visibility"], "visible");
        assert_eq!(print["hasFocus"], true);
        let num = |v: &Value, i: usize| v[i].as_f64().unwrap();
        let (screen, outer, inner, pos) = (
            &print["screen"],
            &print["outer"],
            &print["inner"],
            &print["position"],
        );
        assert!(
            num(outer, 1) > num(inner, 1),
            "browser UI must take space: {outer} vs {inner}"
        );
        assert!(
            num(pos, 0) + num(outer, 0) <= num(screen, 0)
                && num(pos, 1) + num(outer, 1) <= num(screen, 1),
            "window {pos}+{outer} must lie on screen {screen}"
        );
        assert_eq!(
            (f64::from(shot.0), f64::from(shot.1)),
            viewport,
            "screenshots are in CSS pixels, matching the Page Model"
        );
        prints.push((headless, print));
    }
    let (_, reference) = &prints[0];
    for (headless, print) in &prints[1..] {
        assert_eq!(
            print, reference,
            "headless={headless} differs from the first headful run"
        );
    }
}
