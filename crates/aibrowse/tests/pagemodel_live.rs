//! End to end with Page Model v1 on a login page: the page type and auth lines, the
//! form signature, region filtering, and set-of-marks screenshots. Ignored by default;
//! run with `make test-live`.

use std::io::{Read, Write};
use std::net::TcpListener;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

mod common;
use common::{Client, find_ref, set_policy};

const LOGIN: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Sign in · Example</title>
<style>body { font: 16px sans-serif; margin: 0 } header, main, footer { padding: 16px 24px }
label, input, button { display: block; margin: 6px 0 } input { width: 260px }</style>
<header><a href="/">Example</a><nav aria-label="Primary"><a href="/pricing">Pricing</a></nav></header>
<main>
  <h1>Sign in to Example</h1>
  <form aria-label="Sign in" action="/session" method="post">
    <label for="email">Email</label><input id="email" type="email" autocomplete="username" required>
    <label for="pw">Password</label><input id="pw" type="password" autocomplete="current-password" required>
    <label><input type="checkbox" name="remember"> Remember me</label>
    <a href="/reset">Forgot password?</a>
    <button>Sign in</button>
  </form>
</main>
<footer><a href="/privacy">Privacy</a> <a href="/terms">Terms</a></footer>"#;

/// A plan comparison built from divs with ARIA table roles, where "included" is an icon
/// without any text, as on many pricing pages.
const COMPARE: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Plans</title>
<main><h1>Plans</h1>
<div role="table">
  <div role="row"><div role="columnheader">Feature</div><div role="columnheader">Free</div><div role="columnheader">Pro</div></div>
  <div role="row"><div role="cell">Seats</div><div role="cell">1</div><div role="cell">Unlimited</div></div>
  <div role="row"><div role="cell">Exports</div>
    <div role="cell"><svg aria-hidden="true"><path d="M1 1L9 9"/></svg></div>
    <div role="cell"><svg aria-hidden="true"><path d="M1 5L4 8L9 2"/></svg></div></div>
  <div role="row"><div role="cell">Audit log</div>
    <div role="cell"><svg aria-hidden="true"><path d="M1 1L9 9"/></svg></div>
    <div role="cell"><svg aria-hidden="true"><path d="M1 5L4 8L9 2"/></svg></div></div>
</div></main>"#;

/// An SPA screen: a toast that is gone within 100 ms, an error that stays, and a link
/// repeated at the other end of the screen (one outline line, two marks).
const APP: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>App</title>
<style>body { font: 16px sans-serif; margin: 0 } .far { position: absolute; left: 700px; top: 420px; font-size: 28px }</style>
<main><h1>App</h1>
<a href="/home">Home</a>
<a class="far" href="/home">Back home</a>
<p><button type="button" id="refresh">Refresh</button> <button type="button" id="check">Check</button></p>
<div id="toast" aria-live="polite" style="position: fixed; right: 12px; bottom: 12px"></div>
<div id="errors"></div>
</main>
<script>
document.getElementById('refresh').onclick = () => setTimeout(() => {
  const toast = document.getElementById('toast');
  toast.textContent = 'Everything is up to date';
  setTimeout(() => { toast.textContent = ''; }, 100);
}, 150);
document.getElementById('check').onclick = () => {
  document.getElementById('errors').innerHTML = '<div role="alert">Name is required</div>';
};
</script>"#;

/// Forecast cards, as weather sites build them: each day's content inside blocks, the
/// conditions only as images, a heading opening an item, an item with nothing in it.
const FORECAST: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Forecast</title>
<main><h2>Next days <a href="/map" role="button">in Stockholm</a> <button>Copy link</button></h2><ol>
<li><div><a href="/day/1"><h3>Friday 9 Oct.</h3></a></div>
  <ol><li><div><img src="/rain.svg" alt="Night: heavy rain"></div></li>
      <li><div><img src="/sun.svg" alt=""></div></li>
      <li><div><img src="/cloud.svg" alt="Evening: cloudy"></div></li></ol>
  <div><span>Max/min:</span> <span>13°/7°</span></div></li>
<li><h3>Saturday 10 Oct.</h3><div>Max/min: 9°/6°</div></li>
</ol><div><p>Amounts scale with the servings.</p><ul><li>2 eggs</li><li>1 cup flour</li></ul></div></main>"#;

/// A registration form as form builders make them: labels beside fields without `for`,
/// a checkbox whose value isn't its name, and a date picker of clickable grid cells.
const REGISTER: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Register</title>
<main><form onsubmit="event.preventDefault()">
<div class="row"><div class="col"><label>Subjects</label></div>
  <div class="col"><div><div><input role="combobox" aria-autocomplete="list" aria-expanded="false"></div></div></div></div>
<div class="row"><div class="col"><span>Date of birth</span></div>
  <div class="col"><input id="dob" value="08 Oct 2026"></div></div>
<div class="row"><input type="checkbox" id="h3" value="3"><label for="h3">Music</label></div>
<div role="grid" aria-label="December 1990"><div role="row">
  <div role="gridcell" tabindex="-1" style="cursor:pointer;display:inline-block;width:30px;height:30px"
    aria-label="Choose Monday, December 10th, 1990" onclick="dob.value = '10 Dec 1990'">10</div>
</div></div>
<button>Submit</button></form>
<div aria-label="Cell reference"><input value="A1"></div>
<div aria-label="Two fields"><input value="x1"><input value="x2"></div>
<div style="position:relative"><input type="checkbox" id="langs" role="button" aria-label="12 languages"
    aria-description="Go to an article in another language"
    style="position:absolute;opacity:0;width:120px;height:30px;margin:0">
  <label for="langs" aria-hidden="true" style="display:inline-block;width:120px;height:30px">12 languages</label></div>
<input name="website" aria-label="Website" style="opacity:0;width:100px;height:20px">
<div><div role="toolbar"><button>Add Sheet</button></div>
  <div role="listbox" aria-label="Sum"><div role="option" aria-selected="true">Sum: 116</div></div></div></main>"#;

/// A support nag fixed over the page, as news sites show: what covers the link is its
/// text, and its own button collapses it.
const NAG: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>News</title>
<main><a href="/tech" style="display:block;margin:200px">Tech</a></main>
<div id="nag" style="position:fixed;inset:0;background:rgba(255,255,255,.9)">
<p style="margin:180px auto;width:400px;height:200px">Who pays for the journalism we all depend on?</p>
<button onclick="nag.remove()" style="position:absolute;right:10px;top:10px">Collapse banner</button></div>"#;

/// Web components, as MDN builds its compatibility tables: a table two open shadow
/// roots deep, and a card whose slot shows the light text assigned to it while the
/// unassigned light text doesn't render.
const COMPONENT: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Docs</title>
<main><h2>Browser compatibility</h2><compat-lazy></compat-lazy>
<info-card><span slot="title">Baseline 2023</span><span>never shown</span></info-card></main>
<script>
customElements.define('compat-table', class extends HTMLElement {
  connectedCallback() { this.attachShadow({ mode: 'open' }).innerHTML =
    '<style>td{padding:2px}</style><table><tr><th></th><th>Chrome</th><th>Firefox</th></tr>' +
    '<tr><th>:has()</th><td>105</td><td>121</td></tr></table>'; }
});
customElements.define('compat-lazy', class extends HTMLElement {
  connectedCallback() { this.attachShadow({ mode: 'open' }).innerHTML = '<compat-table></compat-table>'; }
});
customElements.define('info-card', class extends HTMLElement {
  connectedCallback() { this.attachShadow({ mode: 'open' }).innerHTML =
    '<p>Status: <slot name="title">unknown</slot></p><p><slot name="note">No notes.</slot></p>'; }
});
</script>"#;

/// A long page whose lower sections skip rendering until near the viewport
/// (`content-visibility: auto`), as Vercel's home page does with its footer.
const SKIPPED: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Home</title>
<main><h1>Welcome</h1><div style="height:4000px">Hero</div></main>
<div style="content-visibility:auto;contain-intrinsic-size:auto 600px"><footer>
<nav aria-label="Directory"><h2>Company</h2><ul><li><a href="/about">About</a></li>
<li><a href="/help">Help</a></li><li><a href="/secret" style="display:none">Secret</a></li></ul></nav>
</footer></div>"#;

/// A help page's FAQ in the shapes sites build: an accessible accordion (heading around
/// the toggle), Chase's (heading inside the toggle, a region labelled by it), a closed
/// <details>; beside them a hidden tab panel and a share menu, which are not content.
const FAQ: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Card help</title>
<main><h1>Card help</h1>
<h3><button aria-expanded="false" aria-controls="a1">How do I reset my PIN?</button></h3>
<div id="a1" hidden><p>Call 555-0100 to reset it.</p></div>
<section><button aria-expanded="false" id="q2"><h3>What do I do if my card is stolen?</h3></button>
<section role="region" aria-labelledby="q2" style="display:none"><p>Call us right away at 555-0199.</p></section></section>
<details><summary>Fees</summary><p>No monthly fee.</p></details>
<div role="tablist"><button role="tab" aria-selected="true" aria-controls="m">Monthly</button>
<button role="tab" aria-selected="false" aria-controls="y">Yearly</button></div>
<div role="tabpanel" id="m"><p>$10 a month</p></div><div role="tabpanel" id="y" hidden><p>$100 a year</p></div>
<button aria-expanded="false" aria-haspopup="menu" aria-controls="share">Share</button>
<ul id="share" role="menu" hidden><li role="menuitem">Copy link</li></ul></main>"#;

/// A shop without a main region under a cookie consent popup with collapsed details
/// (cykelkraft.se's).
const CONSENT: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Hybrid bikes</title>
<div style="position:fixed;inset:0"><div role="dialog" aria-modal="true"><p>You are in control of your data.</p>
<button aria-expanded="false" aria-controls="details">Show details</button>
<div id="details" hidden><table><tr><td>Cookie</td><td>_ga</td></tr></table></div>
<button>Accept</button><button role="alert" aria-label="Reject all">Reject all</button></div></div>
<h1>Hybrid bikes</h1><ul><li>City Hybrid 7 – 6 999 kr</li><li>Trail Hybrid 3 – 8 495 kr</li></ul>"#;

/// A status page with a CAPTCHA challenge loaded far below the page for its subscribe
/// form (githubstatus.com's reCAPTCHA): a modal no person sees.
const UNSEEN_MODAL: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Status</title>
<h1>All systems operational</h1><a href="/history">Incident history</a>
<div style="position:absolute;top:4000px"><div role="dialog" aria-modal="true">
<button>Get a new challenge</button><button>VERIFY</button></div></div>"#;

fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let page = if buf[..n].starts_with(b"GET /compare") {
                    COMPARE
                } else if buf[..n].starts_with(b"GET /unseen-modal") {
                    UNSEEN_MODAL
                } else if buf[..n].starts_with(b"GET /nag") {
                    NAG
                } else if buf[..n].starts_with(b"GET /register") {
                    REGISTER
                } else if buf[..n].starts_with(b"GET /component") {
                    COMPONENT
                } else if buf[..n].starts_with(b"GET /skipped") {
                    SKIPPED
                } else if buf[..n].starts_with(b"GET /help") {
                    "<!doctype html><title>Help center</title><h1>Help center</h1>"
                } else if buf[..n].starts_with(b"GET /consent") {
                    CONSENT
                } else if buf[..n].starts_with(b"GET /faq") {
                    FAQ
                } else if buf[..n].starts_with(b"GET /forecast") {
                    FORECAST
                } else if buf[..n].starts_with(b"GET /app") {
                    APP
                } else {
                    LOGIN
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
                    page.len()
                );
            });
        }
    });
    port
}

/// The decoded image of a tool result that has exactly one image block.
fn image_of(result: &Value) -> image::RgbImage {
    let images: Vec<&Value> = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["type"] == "image")
        .collect();
    assert_eq!(images.len(), 1, "{result}");
    assert_eq!(images[0]["mimeType"], "image/jpeg");
    let bytes = STANDARD
        .decode(images[0]["data"].as_str().unwrap())
        .unwrap();
    image::load_from_memory(&bytes).unwrap().to_rgb8()
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn login_page_model_regions_and_marks() {
    let port = serve();
    let headless = std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1");
    let mut c = Client::spawn("test-pagemodel", headless);
    let outline = c.ok(
        "session_open",
        json!({ "url": format!("http://localhost:{port}/login"), "attended": false }),
    );

    // Page type and auth hint, from the password field and the submit button.
    assert!(
        outline.contains("page: login (password field, button \"Sign in\")"),
        "{outline}"
    );
    assert!(
        outline.contains("auth: only the user can complete this"),
        "{outline}"
    );
    assert!(
        outline.contains("regions: banner, main, contentinfo"),
        "{outline}"
    );

    // One signature line for the form, instead of a line per field.
    let form = outline
        .lines()
        .find(|l| l.contains("form \"Sign in\" (login) → fill("))
        .unwrap_or_else(|| panic!("no form signature in:\n{outline}"));
    for part in [
        "username: \"Email\" [e",
        "password: [e",
        "required",
        "checkbox: \"Remember me\" [e",
        ") submit: button \"Sign in\" [e",
    ] {
        assert!(form.contains(part), "{part} not in {form}");
    }
    assert!(!outline.contains("textbox \"Email\""), "{outline}");
    let email = form
        .split("username: \"Email\" [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap()
        .to_owned();

    // Filling a field shows up in the signature; a password never does.
    let out = c.ok(
        "act",
        json!({ "action": "fill", "ref": email, "text": "ada@example.com", "observe": true }),
    );
    assert!(out.contains("value=\"ada@example.com\""), "{out}");
    let password = form
        .split("password: [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap()
        .to_owned();
    let out = c.ok(
        "act",
        json!({ "action": "fill", "ref": password, "text": "hunter2-secret", "observe": true }),
    );
    assert!(!out.contains("hunter2"), "{out}");

    // A region: only the footer, and nothing else.
    let footer = c.ok("observe", json!({ "region": "footer" }));
    assert!(footer.contains("region: footer"), "{footer}");
    assert!(
        footer.contains("- contentinfo") && footer.contains("link \"Privacy\""),
        "{footer}"
    );
    assert!(
        !footer.contains("fill(") && !footer.contains("Pricing"),
        "{footer}"
    );
    let missing = c.ok("observe", json!({ "region": "dialog" }));
    assert!(
        missing.contains("no region matches \"dialog\""),
        "{missing}"
    );

    // Set-of-marks: the same viewport, with boxes and labels drawn on it.
    let plain = image_of(&c.request(
        "tools/call",
        json!({ "name": "screenshot", "arguments": {} }),
    ));
    let marked = image_of(&c.request(
        "tools/call",
        json!({ "name": "observe", "arguments": { "screenshot": true, "marks": true } }),
    ));
    let via_screenshot = image_of(&c.request(
        "tools/call",
        json!({ "name": "screenshot", "arguments": { "marks": true } }),
    ));
    assert_eq!(plain.dimensions(), marked.dimensions());
    // JPEG re-encoding moves channels by a few levels; marks change pixels completely.
    let changed = |other: &image::RgbImage| {
        plain
            .pixels()
            .zip(other.pixels())
            .filter(|(a, b)| a.0.iter().zip(b.0).any(|(x, y)| x.abs_diff(y) > 100))
            .count()
    };
    assert!(changed(&marked) > 1000, "marked: {}", changed(&marked));
    assert!(
        changed(&via_screenshot) > 1000,
        "via screenshot: {}",
        changed(&via_screenshot)
    );

    c.ok("session_close", json!({}));
}

/// The ref on the outline line containing `needle`.
fn ref_on(outline: &str, needle: &str) -> String {
    let line = outline
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no {needle:?} in:\n{outline}"));
    line.split(" [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap()
        .to_owned()
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn toasts_alerts_and_marks_beyond_the_outline() {
    let port = serve();
    let headless = std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1");
    let mut c = Client::spawn("test-pagemodel-app", headless);
    let outline = c.ok(
        "session_open",
        json!({ "url": format!("http://localhost:{port}/app"), "attended": false }),
    );
    // The repeated link is listed once.
    assert_eq!(outline.matches("-> ").count(), 0, "{outline}");
    assert_eq!(
        outline.lines().filter(|l| l.contains("link \"")).count(),
        1,
        "{outline}"
    );

    // A toast gone long before the page settles is still reported with the action.
    let refresh = ref_on(&outline, "button \"Refresh\"");
    let out = c.ok("act", json!({ "action": "click", "ref": refresh }));
    assert!(
        out.contains("the page announced (live region, page text): \"Everything is up to date\""),
        "{out}"
    );

    // An error that stays is part of the outline, so the diff after the action shows it.
    let check = ref_on(&outline, "button \"Check\"");
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": check, "observe": true }),
    );
    assert!(out.contains("Name is required"), "{out}");
    let observed = c.ok("observe", json!({}));
    assert!(
        observed.contains("- alert \"Name is required\""),
        "{observed}"
    );

    // Marks cover the repeated link the outline merged away: its box changes too.
    let plain = image_of(&c.request(
        "tools/call",
        json!({ "name": "screenshot", "arguments": {} }),
    ));
    let marked = image_of(&c.request(
        "tools/call",
        json!({ "name": "screenshot", "arguments": { "marks": true } }),
    ));
    let changed_near_far_link = (680..900)
        .flat_map(|x| (395..470).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let (a, b) = (plain.get_pixel(x, y), marked.get_pixel(x, y));
            a.0.iter().zip(b.0).any(|(p, q)| p.abs_diff(q) > 100)
        })
        .count();
    assert!(
        changed_near_far_link > 100,
        "repeated link unmarked: {changed_near_far_link} pixels changed"
    );

    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn aria_tables_read_as_tables() {
    let port = serve();
    let mut c = Client::spawn("test-read", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/compare"), "observe": false }),
    );
    let text = c.ok("read", json!({}));
    assert!(text.contains("| Feature | Free | Pro |"), "{text}");
    assert!(text.contains("| Seats | 1 | Unlimited |"), "{text}");
    // The same icon gets the same token, so "included" can be told from "not".
    assert!(text.contains("| Exports | ◆1 | ◆2 |"), "{text}");
    assert!(text.contains("| Audit log | ◆1 | ◆2 |"), "{text}");
    assert!(text.contains("◆1 ×2, ◆2 ×2"), "{text}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn list_items_keep_their_content() {
    let port = serve();
    let mut c = Client::spawn("test-read-lists", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/forecast"), "observe": false }),
    );
    let text = c.ok("read", json!({}));
    // Numbers stay on their item's line, and an item with nothing to say is left out.
    assert!(
        text.contains("## Next days in Stockholm\n"),
        "buttons inside a heading go, button-styled links stay: {text}"
    );
    assert!(text.contains("1. [Friday 9 Oct.](http"), "{text}");
    assert!(text.contains("\n  1. [image: Night: heavy rain]"), "{text}");
    assert!(text.contains("\n  3. [image: Evening: cloudy]"), "{text}");
    assert!(text.contains("2. Saturday 10 Oct."), "{text}");
    // A list starts apart from the paragraph before it.
    assert!(
        text.contains("Amounts scale with the servings.\n\n- 2 eggs\n- 1 cup flour"),
        "{text}"
    );
    // What follows a nested list stays in its item, under its day.
    assert!(
        text.contains("3. [image: Evening: cloudy]\n\n  Max/min: 13°/7°\n2. Saturday"),
        "{text}"
    );
    assert!(
        !text
            .lines()
            .any(|l| l.trim().trim_end_matches('.').parse::<u32>().is_ok()),
        "a bare list number: {text}"
    );

    // In a digest, an image that is all there is on its line is content.
    let digest = c.ok("read", json!({ "view": "digest" }));
    assert!(digest.contains("  1. Night: heavy rain"), "{digest}");
    assert!(digest.contains("Max/min: 13°/7°"), "{digest}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn fields_are_named_as_people_read_them() {
    set_policy("test-form-names", r#"{"confirm": "off"}"#);
    let port = serve();
    let mut c = Client::spawn("test-form-names", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/register") }),
    );
    // Labels beside fields name them; a field's value is not its name.
    assert!(outline.contains("combobox: \"Subjects\""), "{outline}");
    assert!(
        outline.contains("text: \"Date of birth\"") && outline.contains("value=\"08 Oct 2026\""),
        "{outline}"
    );
    // A wrapper's label names the one field inside it, not two.
    assert!(
        outline.contains("textbox \"Cell reference\"") && outline.contains("value=\"A1\""),
        "{outline}"
    );
    assert!(!outline.contains("\"Two fields\" ["), "{outline}");
    // A transparent toggle over its styled label is the button people see, described
    // by its author; a transparent text field (a trap for bots) stays out.
    assert!(
        outline.contains("button \"12 languages\" [e")
            && outline.contains("(Go to an article in another language)"),
        "{outline}"
    );
    assert!(!outline.contains("\"Website\""), "{outline}");
    // A toolbar's button is not the submit of a status box beside it.
    assert!(
        !outline.contains("submit: button \"Add Sheet\""),
        "{outline}"
    );
    // A date picker's days are clickable, under their full names.
    let day = find_ref(&outline, "Choose Monday, December 10th, 1990");
    c.ok("act", json!({ "action": "click", "ref": day }));
    let value = c.ok("evaluate", json!({ "expression": "dob.value" }));
    assert!(value.contains("10 Dec 1990"), "{value}");
    // An action is reported by the element's name, not its value.
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Music") }),
    );
    assert!(out.contains("click input \"Music\""), "{out}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn pdfs_read_as_their_text() {
    let port = common::serve_pages(&[("/report.pdf", include_str!("common/report.pdf"))]);
    let mut c = Client::spawn("test-read-pdf", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/report.pdf") }),
    );
    assert!(outline.contains("page: pdf"), "{outline}");
    let text = c.ok("read", json!({}));
    assert!(text.contains("(PDF, 2 pages)"), "{text}");
    assert!(
        text.contains("## Page 2") && text.contains("Quarterly revenue was 42 million kronor."),
        "{text}"
    );
    let digest = c.ok("read", json!({ "view": "digest" }));
    assert!(digest.contains("42 million"), "{digest}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_covered_click_names_what_dismisses_the_overlay() {
    let port = serve();
    let mut c = Client::spawn("test-overlay", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/nag") }),
    );
    let (ok, failed) = c.call(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "link \"Tech\"") }),
    );
    assert!(!ok, "a covered click fails: {failed}");
    assert!(
        failed.contains("part of an overlay with button \"Collapse banner\""),
        "{failed}"
    );
    c.ok("session_close", json!({}));
}

/// `read` follows what renders: into open shadow roots, and through slots.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn read_sees_inside_web_components() {
    let port = serve();
    let mut c = Client::spawn("test-read-components", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/component"), "observe": false }),
    );
    let text = c.ok("read", json!({}));
    assert!(
        text.contains("|  | Chrome | Firefox |") && text.contains("| :has() | 105 | 121 |"),
        "{text}"
    );
    assert!(text.contains("Status: Baseline 2023"), "{text}");
    assert!(text.contains("No notes."), "a slot's fallback: {text}");
    assert!(
        !text.contains("never shown"),
        "unassigned light text: {text}"
    );
    assert!(!text.contains("padding"), "shadow styles: {text}");
    c.ok("session_close", json!({}));
}

/// Sections that skip rendering until scrolled near are on the page all the same.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn sections_not_yet_rendered_are_in_the_outline() {
    let port = serve();
    let mut c = Client::spawn("test-skipped-sections", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/skipped") }),
    );
    let all = c.ok("observe", json!({ "detail": "all" }));
    assert!(all.contains("link \"Help\""), "{outline}\n{all}");
    assert!(all.contains("(below)"), "{all}");
    assert!(!all.contains("Secret"), "display:none stays out: {all}");
    let help = all
        .lines()
        .find(|l| l.contains("link \"Help\""))
        .and_then(|l| l.split('[').nth(1))
        .and_then(|r| r.split(']').next())
        .unwrap()
        .to_owned();
    let done = c.ok("act", json!({ "action": "click", "ref": help }));
    assert!(done.contains("/help"), "{done}");
    c.ok("session_close", json!({}));
}

/// Collapsed answers are read under their questions; other hidden parts are not.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn read_includes_collapsed_answers() {
    let port = serve();
    let mut c = Client::spawn("test-read-faq", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/faq"), "observe": false }),
    );
    let text = c.ok("read", json!({}));
    assert!(
        text.contains("### How do I reset my PIN?\n\nCall 555-0100 to reset it."),
        "{text}"
    );
    assert!(
        text.contains("### What do I do if my card is stolen?\n\nCall us right away at 555-0199."),
        "{text}"
    );
    assert!(
        text.contains("Fees") && text.contains("No monthly fee."),
        "{text}"
    );
    assert!(text.contains("$10 a month"), "{text}");
    assert!(!text.contains("$100 a year"), "a hidden tab panel: {text}");
    assert!(
        !text.contains("Share") && !text.contains("Copy link"),
        "a menu: {text}"
    );
    c.ok("session_close", json!({}));
}

/// A modal nothing of which is in view hides nothing: the page is outlined as usual.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_modal_out_of_view_hides_nothing() {
    let port = serve();
    let mut c = Client::spawn("test-unseen-modal", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/unseen-modal") }),
    );
    assert!(!outline.contains("a modal dialog is open"), "{outline}");
    assert!(outline.contains("Incident history"), "{outline}");
    c.ok("session_close", json!({}));
}

/// A modal popup over the page (a cookie consent) isn't read as the page's text, nor
/// are the collapsed details inside it.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn read_leaves_out_modal_popups() {
    let port = serve();
    let mut c = Client::spawn("test-read-consent", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/consent"), "observe": false }),
    );
    let text = c.ok("read", json!({}));
    assert!(text.contains("City Hybrid 7 – 6 999 kr"), "{text}");
    assert!(!text.contains("in control of your data"), "{text}");
    assert!(!text.contains("_ga"), "{text}");
    c.ok("session_close", json!({}));
}

/// A native button whose ARIA role says otherwise (Cookie Information's reject button
/// is `<button role="alert">`) is still a button to press.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_button_with_another_role_is_still_a_button() {
    let port = serve();
    let mut c = Client::spawn("test-native-roles", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/consent") }),
    );
    assert!(outline.contains("button \"Reject all\" [e"), "{outline}");
    c.ok("session_close", json!({}));
}
