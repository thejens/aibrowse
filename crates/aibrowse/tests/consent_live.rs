//! Cookie consent against a real Chrome: refusing non-essential cookies is never held
//! for approval, and the navigator gets past a site that confirms the refusal in a
//! dialog of its own.

mod common;

use common::{Client, find_ref, has_jev_key, serve_pages, set_policy};
use serde_json::json;

/// A banner whose refusal is confirmed in a one-button modal before the page reloads,
/// as notion.site does. The terms link is only reachable once the dialog is closed.
const SITE: &str = r#"<!doctype html><title>Acme Workspace</title><h1>Acme Workspace</h1>
<main><p>Notes, docs and projects in one place.</p><a href="/terms">Terms of Service</a></main>
<div id="banner"><p>We use cookies to run this site and, with your consent, to measure it.
<a href="/cookies">Cookie Notice</a></p>
<button onclick="banner.remove()">Accept all</button>
<button onclick="banner.remove(); ack.showModal()">Reject all</button></div>
<dialog id="ack" aria-label="Acme will now reload to apply your cookie preferences. Click okay to continue.">
<p>Acme will now reload to apply your cookie preferences. Click okay to continue.</p>
<button onclick="location.reload()">OK</button></dialog>
<script>if (sessionStorage.refused) banner.remove();
document.querySelector('#banner button:last-child').addEventListener('click', () => sessionStorage.refused = 1);</script>"#;
const TERMS: &str = r#"<!doctype html><title>Terms of Service - Acme</title><h1>Terms of Service</h1>
<main><p>These terms govern your use of Acme Workspace. By using the service you agree to
them. 1. Accounts. 2. Acceptable use. 3. Payment. 4. Termination.</p></main>"#;
const PAGES: &[(&str, &str)] = &[("/", SITE), ("/terms", TERMS)];

/// A Swedish banner, as sl.se shows it: both choices start with "Godkänn" (accept), and
/// the refusal is "Godkänn nödvändiga kakor", accepting only the necessary ones.
const SWEDISH: &str = r#"<!doctype html><html lang="sv"><meta charset="utf-8"><title>Resor | Exempel</title>
<section aria-label="Kakbanner" id="banner"><h2>Vi använder kakor</h2>
<p>Vissa kakor är nödvändiga för att webbplatsen ska fungera och är därför alltid påslagna.</p>
<button onclick="banner.remove()">Godkänn alla kakor</button>
<button onclick="banner.remove(); document.title = 'Bara nödvändiga'">Godkänn nödvändiga kakor</button>
<a href="/installningar">Kakinställningar</a></section>
<main><h1>Resor</h1><a href="/villkor">Villkor för resor</a></main>"#;
const VILLKOR: &str = r#"<!doctype html><html lang="sv"><meta charset="utf-8"><title>Villkor | Exempel</title>
<main><h1>Villkor för resor</h1><p>Dessa villkor gäller för alla resor med våra bussar och tåg.</p></main>"#;
const SWEDISH_PAGES: &[(&str, &str)] = &[("/", SWEDISH), ("/villkor", VILLKOR)];

/// A pay-or-consent wall: accepting tracking or subscribing are the only ways past it.
const PAYWALL: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Daily News</title>
<main><h1>Daily News</h1><a href="/technology">Technology</a></main>
<div role="dialog" aria-modal="true" aria-label="Your privacy" id="wall"
  style="position:fixed;inset:0;background:#fff">
<h2>Accept all cookies or subscribe to reject</h2>
<p>Accept cookies and tracking to keep reading for free, or reject all and subscribe for an ad-free read.</p>
<button onclick="wall.remove()">Accept all</button>
<button onclick="location.href='/subscribe'">Reject all and subscribe</button></div>"#;
const SUBSCRIBE: &str =
    r#"<!doctype html><title>Subscribe | Daily News</title><h1>Choose a plan</h1>"#;
const PAYWALL_PAGES: &[(&str, &str)] = &[("/subscribe", SUBSCRIBE), ("/", PAYWALL)];

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn refusing_cookies_needs_no_approval() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-consent-gate", r#"{"confirm": "consequential"}"#);
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-consent-gate", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let reject = find_ref(&outline, "button \"Reject all\"");
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": reject, "observe": true }),
    );
    assert!(
        out.contains("\": done") && out.contains("cookie preferences"),
        "the refusal runs without asking, and its dialog opens: {out}"
    );
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn navigator_closes_the_dialog_that_confirms_a_refusal() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-consent-nav", r#"{"confirm": "off"}"#);
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-consent-nav", true);
    let out = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/"), "goal": "the terms of service" }),
    );
    assert!(
        out.contains("declined optional cookies (button \"Reject all\")")
            && out.contains("closed the notice confirming it (OK)")
            && out.contains("Reached")
            && out.contains("/terms"),
        "{out}"
    );
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn consent_is_understood_in_any_language() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    // Agreements asked about: accepting cookies is held for approval, as committing.
    set_policy(
        "test-consent-sv",
        r#"{"confirm": "consequential", "agreements": "ask"}"#,
    );
    let port = serve_pages(SWEDISH_PAGES);
    let mut c = Client::spawn("test-consent-sv", true);
    let out = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/"), "goal": "the terms for travelling" }),
    );
    assert!(
        out.contains("declined optional cookies (button \"Godkänn nödvändiga kakor\")")
            && out.contains("Reached")
            && out.contains("/villkor"),
        "{out}"
    );

    // The agent declining by hand isn't held for approval either; accepting would be.
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/?again") }),
    );
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Godkänn nödvändiga kakor") }),
    );
    assert!(out.contains("\": done"), "{out}");
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/?once-more") }),
    );
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Godkänn alla kakor") }),
    );
    assert!(out.contains("needs their approval"), "{out}");
    c.ok("session_close", json!({}));

    // By default agreements are accepted: the same click goes through unasked.
    set_policy("test-consent-sv", r#"{"confirm": "consequential"}"#);
    let mut c = Client::spawn("test-consent-sv", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/?accepting") }),
    );
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Godkänn alla kakor") }),
    );
    assert!(out.contains("\": done"), "{out}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn a_pay_or_consent_wall_is_never_settled_by_subscribing() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-consent-paywall", r#"{"confirm": "off"}"#);
    let port = serve_pages(PAYWALL_PAGES);
    let mut c = Client::spawn("test-consent-paywall", true);
    let out = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/"), "goal": "the Technology section" }),
    );
    assert!(!out.contains("declined optional cookies"), "{out}");
    assert!(
        !out.contains("/subscribe"),
        "subscribed on the user's behalf: {out}"
    );
    c.ok("session_close", json!({}));
}

/// A prompt whose first layer can only accept or open settings, as Hemnet's does; the
/// settings decline. The listing link is behind the prompt.
const LAYERED: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Homes</title>
<main><h1>Homes for sale</h1><a href="/listings">Apartments for sale</a></main>
<div role="dialog" aria-modal="true" id="consent" aria-label="Privacy" style="position:fixed;inset:0;background:#fff">
  <div id="first"><h2>We value your privacy</h2><p>We and our partners use cookies.</p>
    <button onclick="first.hidden = true; second.hidden = false">Cookie settings</button>
    <button onclick="consent.remove(); document.title = 'Accepted'">Accept all</button></div>
  <div id="second" hidden><h2>Cookie settings</h2>
    <label><input type="checkbox" role="switch"> Marketing</label>
    <label><input type="checkbox" role="switch"> Statistics</label>
    <button onclick="consent.remove(); document.title = 'Chosen'">Save settings</button>
    <button onclick="consent.remove(); document.title = 'Declined'">Deny all</button>
    <button onclick="consent.remove(); document.title = 'Accepted'">Accept all</button></div>
</div>"#;
const LISTINGS: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Apartments for sale</title>
<main><h1>Apartments for sale</h1><p>174 apartments, from 2 950 000 kr.</p></main>"#;

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn a_prompt_without_a_refusal_up_front_is_declined_in_its_settings() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-consent-layered", r#"{"confirm": "consequential"}"#);
    let port = serve_pages(&[("/", LAYERED), ("/listings", LISTINGS)]);
    let mut c = Client::spawn("test-consent-layered", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/"), "observe": false }),
    );
    let declined = c.ok("evaluate", json!({ "expression": "document.title" }));
    assert!(declined.contains("Homes"), "{declined}");
    let out = c.ok(
        "navigate",
        json!({ "goal": "the list of apartments for sale", "observe": false }),
    );
    assert!(
        out.contains("opened the cookie settings to decline there") && out.contains("Reached"),
        "{out}"
    );
    // Declined, never accepted: the listing page has its own title, and the home page
    // was left with the prompt's choice "Declined" (seen in the steps, not the title).
    assert!(out.contains("Deny all"), "{out}");
    assert!(!out.contains("Accept all\""), "{out}");
    c.ok("session_close", json!({}));
}
