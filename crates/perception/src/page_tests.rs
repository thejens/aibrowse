//! Page-type and auth hint rules (page.rs).

use super::*;

fn ax(role: &str, name: &str) -> AxNode {
    serde_json::from_value(serde_json::json!({
        "nodeId": "1",
        "role": { "value": role },
        "name": { "value": name },
    }))
    .unwrap()
}

fn input(attrs: &[(&str, &str)]) -> ElementFacts {
    ElementFacts {
        tag: "input".into(),
        attributes: attrs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        ..ElementFacts::default()
    }
}

fn kind(signals: &Signals, url: &str, title: &str, modal: bool) -> Option<&'static str> {
    signals.page_type(url, title, modal).map(|p| p.kind)
}

#[test]
fn classifies_auth_walls() {
    let mut code = Signals::default();
    let otp = input(&[("autocomplete", "one-time-code")]);
    code.add(&ax("textbox", "Code"), Some(&otp), false);
    assert_eq!(
        kind(&code, "https://x.com/", "Verify", false),
        Some("verification")
    );
    assert!(
        code.auth(code.page_type("", "", false).as_ref())
            .unwrap()
            .contains("request_handoff")
    );

    // The first step of a two-step login asks only for the account name.
    let mut first_step = Signals::default();
    let email = input(&[("type", "email")]);
    first_step.add(&ax("textbox", "Email"), Some(&email), false);
    first_step.add(
        &ax("button", "Next"),
        Some(&input(&[("type", "submit")])),
        false,
    );
    assert_eq!(
        kind(&first_step, "https://x.com/signin", "Sign in", false),
        Some("login")
    );
    assert_eq!(
        kind(&first_step, "https://x.com/newsletter", "News", false),
        None
    );

    let challenge = Signals::default();
    assert_eq!(
        kind(&challenge, "https://x.com/", "Just a moment...", false),
        Some("captcha")
    );
}

#[test]
fn cookie_walls_only_when_modal() {
    // A cookie bar beside an unrelated modal (a region picker) isn't a cookie wall.
    let mut bar = Signals::default();
    bar.add(&ax("StaticText", "We use cookies"), None, true);
    bar.add(&ax("button", "Accept"), None, false);
    bar.in_modal = true;
    bar.add(&ax("button", "Stay in United States"), None, false);
    assert_eq!(kind(&bar, "https://x.com/", "Shop", true), None);

    let mut banner = Signals {
        in_modal: true,
        ..Signals::default()
    };
    banner.add(
        &ax("StaticText", "We use cookies to improve your experience"),
        None,
        true,
    );
    banner.add(&ax("button", "Accept all"), None, false);
    assert_eq!(
        kind(&banner, "https://x.com/", "News", true),
        Some("cookie-consent")
    );
    assert_eq!(kind(&banner, "https://x.com/", "News", false), None);
}

#[test]
fn search_results_and_sign_in_state() {
    let mut results = Signals::default();
    for i in 0..12 {
        results.add(&ax("link", &format!("Result {i}")), None, false);
    }
    results.add(&ax("link", "Sign in"), None, false);
    let page = results.page_type(
        "https://duckduckgo.com/?q=rust",
        "rust at DuckDuckGo",
        false,
    );
    assert_eq!(page.as_ref().map(|p| p.kind), Some("search-results"));
    assert_eq!(
        results.auth(page.as_ref()).as_deref(),
        Some("signed out (\"Sign in\" is offered)")
    );
    results.add(&ax("menuitem", "Sign out"), None, false);
    assert!(
        results
            .auth(page.as_ref())
            .unwrap()
            .starts_with("signed in")
    );
}

#[test]
fn hints_in_major_languages() {
    let state = |label: &str| {
        let mut s = Signals::default();
        s.add(&ax("link", label), None, false);
        s.auth(None)
    };
    assert!(state("Cerrar sesión").unwrap().starts_with("signed in"));
    assert!(state("Se connecter").unwrap().starts_with("signed out"));
    assert!(
        state("Mein Konto")
            .unwrap()
            .starts_with("probably signed in")
    );
    assert!(state("登录").unwrap().starts_with("signed out"));
    // Beyond them, no hint rather than a guess.
    assert!(state("Logga in").is_none());
    assert!(error_evidence("Página no encontrada", None).is_some());
    assert!(error_evidence("Seite nicht gefunden | Beispiel", None).is_some());
    assert!(error_evidence("Service unavailable", None).is_some());
}

#[test]
fn errors_need_to_be_the_point_of_the_page() {
    assert!(error_evidence("404 Not Found", None).is_some());
    assert!(error_evidence("Page not found | Example", None).is_some());
    assert!(error_evidence("HTTP 404 - Wikipedia", None).is_none());
    let heading = "Something went wrong".to_owned();
    assert!(error_evidence("Example", Some(&heading)).is_some());
}

#[test]
fn labels_match_whole_words() {
    assert!(labelled("sign in", SIGN_IN));
    assert!(labelled("sign in with google", SIGN_IN));
    assert!(!labelled("logins", SIGN_IN));
    assert!(!labelled("joined", SIGN_UP));
}

#[test]
fn search_queries_come_from_the_url() {
    assert_eq!(
        search_query("https://duckduckgo.com/?q=rust+lang&ia=web").as_deref(),
        Some("rust lang")
    );
    assert_eq!(search_query("https://example.com/?page=2"), None);
    assert_eq!(
        search_query("https://github.com/x/issues?q=is%3Aissue+label%3AE-easy").as_deref(),
        Some("is:issue label:E-easy")
    );
    assert_eq!(percent_decode("caf%C3%A9 %zz"), "café %zz");
}

#[test]
fn a_newsletter_box_is_no_login() {
    // A shop's listing whose footer asks for an email to "Anmelden" (subscribe).
    let mut listing = Signals::default();
    for i in 0..60 {
        listing.add(&ax("link", &format!("Laufschuh {i}")), None, false);
    }
    let email = input(&[("type", "email")]);
    listing.add(&ax("textbox", "E-Mail-Adresse"), Some(&email), false);
    listing.add(
        &ax("button", "Anmelden"),
        Some(&input(&[("type", "submit")])),
        false,
    );
    assert_ne!(
        kind(&listing, "https://x.de/herren/schuhe/", "Schuhe", false),
        Some("login")
    );
    // The same box on a login URL is a login still.
    assert_eq!(
        kind(&listing, "https://x.de/login", "Anmelden", false),
        Some("login")
    );
}

#[test]
fn a_product_page_is_no_listing() {
    let heading = |level: i64, name: &str| -> AxNode {
        serde_json::from_value(serde_json::json!({
            "nodeId": "1",
            "role": { "value": "heading" },
            "name": { "value": name },
            "properties": [{ "name": "level", "value": { "value": level } }],
        }))
        .unwrap()
    };
    let mut product = Signals::default();
    product.add(&heading(1, "POÄNG Armchair"), None, false);
    product.add(&ax("StaticText", "€ 129,00"), None, false);
    product.add(&ax("button", "Add to bag"), None, false);
    for i in 0..40 {
        product.add(&ax("listitem", ""), None, false);
        product.add(&ax("link", &format!("Related {i}")), None, false);
    }
    let page = product.page_type("https://x.com/p/poang", "POÄNG", false);
    assert_eq!(page.as_ref().map(|p| p.kind), Some("product"), "{page:?}");

    // A listing's quick-add buttons, one per item, make it no product page.
    for i in 0..10 {
        product.add(&ax("StaticText", &format!("{i}9,00 €")), None, false);
        product.add(&ax("button", "Add to bag"), None, false);
    }
    assert_ne!(
        kind(&product, "https://x.com/c/chairs", "Chairs", false),
        Some("product")
    );
    assert!(is_price("1 299 kr") && is_price("$1,299.00") && !is_price("Size 42"));
}
