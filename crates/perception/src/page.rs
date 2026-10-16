//! Page type and auth hints: a cheap, deterministic classification of the whole page
//! (no model call), so the agent knows at a glance that it is looking at a login wall,
//! a 404 or a captcha, and whether the session is signed in.
//!
//! Signals come from the full accessibility tree, not the focused outline, so a password
//! field below the fold or a "Sign out" item far down still counts. Rules are ordered
//! from the most to the least blocking: a captcha or error page is that, whatever else
//! it contains.

use crate::ax::AxNode;
use crate::forms;
use crate::model::PageType;
use crate::page_words::{
    ABOUT_CONSENT, ACCOUNT, ADD_TO_CART, CHALLENGE, CONSENT, ERROR, SIGN_IN, SIGN_OUT, SIGN_UP,
    VERIFY,
};
use crate::snapshot::ElementFacts;
use crate::text::clean;

/// A page with this many links is about more than signing in (a shop's listing with a
/// newsletter box); a sign-in page has a handful.
const MAX_LOGIN_PAGE_LINKS: usize = 40;

/// What the classifier looks at, gathered in one pass over every frame's tree.
#[derive(Default)]
pub(crate) struct Signals {
    fields: Vec<&'static str>,
    submits: Vec<String>,
    headings: Vec<String>,
    /// Characters of paragraph text, the mark of an article.
    text_chars: usize,
    paragraphs: usize,
    articles: usize,
    /// Rows and list items: the repeated entries of a listing.
    items: usize,
    links: usize,
    /// Top-level headings: a product page has one, its product's name.
    h1: Vec<String>,
    /// Short texts that are a price ("€ 49,99", "1 299 kr").
    prices: usize,
    /// Buttons that put a product into the cart ("Add to bag"), by name.
    add_to_cart: Vec<String>,
    challenge_frame: Option<String>,
    challenge_text: Option<String>,
    /// Set by the traversal while it is inside an open modal dialog: only what a modal
    /// says counts toward a cookie wall (a cookie bar beside an unrelated modal, such as
    /// a store's region picker, isn't one).
    pub(crate) in_modal: bool,
    about_cookies: bool,
    consent_button: Option<String>,
    sign_in: Option<String>,
    sign_out: Option<String>,
    account: Option<String>,
}

impl Signals {
    /// Adds one visible, non-ignored node. `in_paragraph` tells paragraph text apart
    /// from labels and menus.
    pub(crate) fn add(&mut self, ax: &AxNode, fact: Option<&ElementFacts>, in_paragraph: bool) {
        let role = ax.role();
        let name = ax.name().trim();
        let lower = name.to_lowercase();
        if let Some(field) = forms::field(role, name, fact) {
            self.fields.push(field.kind);
        }
        match role {
            "StaticText" => {
                if in_paragraph {
                    self.text_chars += name.chars().count();
                }
                if is_price(name) {
                    self.prices += 1;
                }
                if self.in_modal && phrase(&lower, ABOUT_CONSENT).is_some() {
                    self.about_cookies = true;
                }
                if self.challenge_text.is_none() && name.len() < 200 {
                    self.challenge_text = phrase(&lower, CHALLENGE).map(|_| clean(name, 60));
                }
            }
            "paragraph" => self.paragraphs += 1,
            "article" => self.articles += 1,
            "row" | "listitem" => self.items += 1,
            "heading" if !name.is_empty() => {
                if self.headings.len() < 8 {
                    self.headings.push(clean(name, 60));
                }
                if ax.property("level").and_then(|v| v.as_i64()) == Some(1) {
                    self.h1.push(clean(name, 60));
                }
            }
            "Iframe" if self.challenge_frame.is_none() && phrase(&lower, CHALLENGE).is_some() => {
                self.challenge_frame = Some(clean(name, 60));
            }
            "checkbox" if phrase(&lower, CHALLENGE).is_some() => {
                self.challenge_text = Some(clean(name, 60));
            }
            _ => {}
        }
        if !matches!(role, "button" | "link" | "menuitem") || name.is_empty() {
            return;
        }
        if role == "link" {
            self.links += 1;
        }
        if role == "button" && phrase(&lower, ADD_TO_CART).is_some() {
            self.add_to_cart.push(clean(name, 40));
        }
        if forms::is_submit(role, fact) {
            self.submits.push(clean(name, 40));
            return;
        }
        let short = clean(name, 40);
        if labelled(&lower, SIGN_OUT) {
            self.sign_out.get_or_insert(short);
        } else if labelled(&lower, SIGN_IN) {
            self.sign_in.get_or_insert(short);
        } else if ACCOUNT.contains(&lower.as_str()) {
            self.account.get_or_insert(short);
        } else if self.in_modal && labelled(&lower, CONSENT) {
            self.consent_button.get_or_insert(short);
        }
    }

    /// The page type, if a rule is confident.
    pub(crate) fn page_type(&self, url: &str, title: &str, modal: bool) -> Option<PageType> {
        let found = |kind: &'static str, evidence: Vec<String>| Some(PageType { kind, evidence });
        let title_lower = title.to_lowercase();
        let count = |k: &str| self.fields.iter().filter(|&&f| f == k).count();
        let heading = |words: &[&str]| {
            self.headings
                .iter()
                .find(|h| phrase(&h.to_lowercase(), words).is_some())
                .map(|h| format!("heading {h:?}"))
        };
        let submit = |words: &[&str]| {
            self.submits
                .iter()
                .find(|s| labelled(&s.to_lowercase(), words))
                .map(|s| format!("button {s:?}"))
        };

        if title_lower.starts_with("just a moment") || title_lower.contains("attention required") {
            return found("captcha", vec![format!("title {title:?}")]);
        }
        if let Some(frame) = &self.challenge_frame {
            return found("captcha", vec![format!("challenge frame {frame:?}")]);
        }
        if let Some(text) = &self.challenge_text {
            return found("captcha", vec![format!("{text:?}")]);
        }
        if let Some(evidence) = error_evidence(title, self.headings.first()) {
            return found("error", vec![evidence]);
        }
        if modal
            && self.about_cookies
            && let Some(button) = &self.consent_button
        {
            let evidence = vec!["modal about cookies".into(), format!("button {button:?}")];
            return found("cookie-consent", evidence);
        }
        if count("otp") > 0 {
            return found("verification", vec!["one-time code field".into()]);
        }
        if let Some(h) = heading(VERIFY).filter(|_| !self.fields.is_empty()) {
            return found("verification", vec![h]);
        }
        let passwords = count("password") + count("new-password");
        if count("new-password") > 0 || count("password") > 1 {
            let what = if count("new-password") > 0 {
                "new-password field"
            } else {
                "2 password fields"
            };
            return found("signup", vec![what.into()]);
        }
        if passwords == 1 {
            if let Some(h) = heading(SIGN_UP).or_else(|| submit(SIGN_UP)) {
                return found("signup", vec!["password field".into(), h]);
            }
            let mut evidence = vec!["password field".to_owned()];
            evidence.extend(
                submit(SIGN_IN).or_else(|| self.submits.first().map(|s| format!("button {s:?}"))),
            );
            return found("login", evidence);
        }
        // Two-step logins ask for the account name first, on a page of its own. An email
        // field elsewhere is mostly a newsletter's, whose button may well say "Anmelden"
        // (sign up, in German, as well as sign in): on a page with much else to follow,
        // only a login URL makes the field a login.
        let about_signing_in = self.links < MAX_LOGIN_PAGE_LINKS;
        if count("username") + count("email") > 0
            && let Some(evidence) = submit(SIGN_IN)
                .or_else(|| heading(SIGN_IN))
                .filter(|_| about_signing_in)
                .or_else(|| {
                    path_has(url, &["/login", "/signin", "/sign-in", "/sign_in"])
                        .then(|| "login URL".into())
                })
        {
            return found("login", vec!["account name field".into(), evidence]);
        }
        if count("card-number") > 0 {
            return found("checkout", vec!["card number field".into()]);
        }
        if path_has(url, &["/checkout", "/payment"]) && !self.fields.is_empty() {
            return found("checkout", vec!["checkout URL".into()]);
        }
        if let Some(query) = search_query(url).filter(|_| self.links >= 10) {
            return found(
                "search-results",
                vec![format!("query {query:?}"), format!("{} links", self.links)],
            );
        }
        // One product, named by the one top-level heading, with its price and a button
        // to buy it. A listing's quick-add buttons come by the dozen.
        if let [name] = self.h1.as_slice()
            && self.prices > 0
            && let [button, ..] = self.add_to_cart.as_slice()
            && self.add_to_cart.len() <= 2
        {
            return found(
                "product",
                vec![
                    format!("heading {name:?}"),
                    "a price".into(),
                    format!("button {button:?}"),
                ],
            );
        }
        let mut evidence: Vec<String> = self
            .headings
            .first()
            .map(|h| format!("heading {h:?}"))
            .into_iter()
            .collect();
        if self.text_chars >= 2500 && self.paragraphs >= 5 && self.articles <= 2 {
            evidence.push(format!("{} of text", approx_chars(self.text_chars)));
            return found("article", evidence);
        }
        if self.articles >= 5 {
            return found("listing", vec![format!("{} articles", self.articles)]);
        }
        if self.items >= 15 && self.links >= 30 && self.text_chars < 2500 {
            return found("listing", vec![format!("{} items", self.items)]);
        }
        None
    }

    /// Whether the session looks signed in, or the page needs the user.
    pub(crate) fn auth(&self, page: Option<&PageType>) -> Option<String> {
        if page.is_some_and(PageType::needs_user) {
            return Some(
                "only the user can complete this; request_handoff if they are present, and never ask for credentials or codes in chat".into(),
            );
        }
        if let Some(control) = &self.sign_out {
            Some(format!("signed in ({control:?} is offered)"))
        } else if let Some(control) = &self.sign_in {
            Some(format!("signed out ({control:?} is offered)"))
        } else {
            self.account
                .as_ref()
                .map(|control| format!("probably signed in ({control:?} is offered)"))
        }
    }
}

/// Whether a short text is a price: digits with a currency sign or code ("€ 49,99",
/// "$1,299.00", "1 299 kr", "CHF 20.-").
fn is_price(text: &str) -> bool {
    if text.chars().count() > 24 || !text.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if text.contains(['€', '$', '£', '¥', '₹']) {
        return true;
    }
    let lower = text.to_lowercase();
    lower.split(|c: char| !c.is_alphabetic()).any(|word| {
        matches!(
            word,
            "kr" | "sek" | "nok" | "dkk" | "eur" | "usd" | "gbp" | "chf" | "zł" | "pln"
        )
    })
}

/// Error pages say so up front, in the title or the first heading. Longer titles that
/// merely mention an error ("HTTP 404 - Wikipedia") are articles about it.
fn error_evidence(title: &str, first_heading: Option<&String>) -> Option<String> {
    // Any segment of the title ("Example | Page not found") may carry it.
    let is_error = |text: &str| {
        let lower = text.to_lowercase();
        lower.chars().count() <= 40
            && lower
                .split(['|', '-', '–', '—', ':', '·'])
                .any(|segment| ERROR.iter().any(|w| segment.trim().starts_with(w)))
    };
    if is_error(title) {
        Some(format!("title {title:?}"))
    } else {
        first_heading
            .filter(|h| is_error(h))
            .map(|h| format!("heading {h:?}"))
    }
}

fn phrase<'a>(text: &str, words: &[&'a str]) -> Option<&'a str> {
    words.iter().copied().find(|w| text.contains(w))
}

/// A label that is one of `words`, alone or followed by more ("Sign in with Google"),
/// but not part of a longer word ("Joined", "Logins").
fn labelled(label: &str, words: &[&str]) -> bool {
    words.iter().any(|w| {
        label == *w
            || label
                .strip_prefix(w)
                .is_some_and(|rest| rest.starts_with([' ', '/', '|', '·']))
    })
}

fn path_has(url: &str, parts: &[&str]) -> bool {
    let path = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    parts.iter().any(|p| path.contains(p))
}

/// The search terms in a results URL (`?q=…`, `?query=…`, `/search?…`).
fn search_query(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        let is_search_key = matches!(
            key,
            "q" | "query" | "search" | "search_query" | "keywords" | "k" | "term" | "text"
        );
        let value = percent_decode(&value.replace('+', " "));
        (is_search_key && !value.trim().is_empty()).then(|| clean(&value, 40))
    })
}

/// Decodes `%XX` escapes (as UTF-8), leaving malformed ones as they are.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (b, ..) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn approx_chars(chars: usize) -> String {
    if chars >= 1000 {
        format!("~{}k chars", chars / 1000)
    } else {
        format!("{chars} chars")
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
