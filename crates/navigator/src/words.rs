//! Search words taken from the goal itself, for a site search when the agent suggested
//! none, and for the site lookup. Jev can't write text, but it can tell which of the
//! goal's words say what it is about, in any language ("the Microsoft support article on
//! how to change the default web browser in Windows" → "change default web browser
//! windows"); an English filler list stands in when it can't answer. They are offered
//! like the agent's suggestions: Jev still decides whether they fit the field, each is
//! typed once, and the agent gets its turn when they lead nowhere.

use serde_json::{Value, json};

use crate::jev::{Jev, JevError};

/// Words that say what kind of page the goal is, or hold a sentence together, rather
/// than what it is about.
const FILLER: &[&str] = &[
    "a",
    "about",
    "all",
    "an",
    "and",
    "any",
    "are",
    "article",
    "articles",
    "as",
    "at",
    "be",
    "by",
    "can",
    "can't",
    "cannot",
    "center",
    "centre",
    "do",
    "does",
    "explaining",
    "explains",
    "describing",
    "describes",
    "find",
    "for",
    "from",
    "get",
    "guide",
    "help",
    "here",
    "how",
    "i",
    "in",
    "info",
    "information",
    "is",
    "it",
    "its",
    "list",
    "lists",
    "me",
    "my",
    "of",
    "official",
    "on",
    "or",
    "our",
    "page",
    "pages",
    "section",
    "see",
    "show",
    "site",
    "support",
    "take",
    "that",
    "the",
    "their",
    "there",
    "these",
    "this",
    "those",
    "to",
    "we",
    "website",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "with",
    "you",
    "your",
];

/// Parts of a host that name no site: the site's own name in a query only matches
/// everything on it.
const HOST_NOISE: &[&str] = &["www", "com", "org", "net", "co", "uk", "io", "app"];

/// Fewer content words than this make no query worth typing ("the help center or
/// customer support" leaves "customer").
const MIN_WORDS: usize = 2;
const MAX_WORDS: usize = 8;

/// What to try typing into a site search for `goal`: its content words as one query,
/// without the site's own name (`host`), then each name the goal mentions on its own
/// (see `names`). Empty when too little is left. The query comes first: the site lookup
/// (site_search.rs) uses it, and Jev picks whichever fits the field.
fn search_words(goal: &str, host: &str) -> Vec<String> {
    let mut words = content_words(goal, host);
    words.truncate(MAX_WORDS);
    query_and_names(goal, words)
}

/// The query of `words`, when there are enough of them, then the names `goal` mentions.
fn query_and_names(goal: &str, words: Vec<String>) -> Vec<String> {
    let mut texts: Vec<String> = Vec::new();
    if words.len() >= MIN_WORDS {
        texts.push(words.join(" "));
    }
    for name in names(goal) {
        if !texts.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
            texts.push(name);
        }
    }
    texts
}

/// `search_words`, with the goal's words judged by Jev rather than by the English
/// filler list: in any language, the words a person would type into a search box for
/// the goal ("pièces justificatives carte d'identité", not "trouver la liste des pièces
/// justificatives pour faire"). Falls back to the filler list when Jev can't answer.
pub async fn judged_search_words(jev: &Jev, goal: &str, host: &str) -> Vec<String> {
    match subject_words(jev, goal, host).await {
        Ok(mut words) => {
            words.truncate(MAX_WORDS);
            query_and_names(goal, words)
        }
        Err(err) => {
            tracing::debug!(%err, "search words from the filler list");
            search_words(goal, host)
        }
    }
}

/// The goal's words Jev finds a person would search for, in order and once each,
/// without the site's own name.
async fn subject_words(jev: &Jev, goal: &str, host: &str) -> Result<Vec<String>, JevError> {
    let site = site_names(host);
    let lower = goal.to_lowercase();
    let mut words: Vec<&str> = Vec::new();
    for token in tokens(&lower) {
        if !site.contains(&token) && !words.contains(&token) {
            words.push(token);
        }
    }
    words.truncate(MAX_JUDGED_WORDS);
    let mut questions = serde_json::Map::new();
    for (n, word) in words.iter().enumerate() {
        questions.insert(
            format!("w{n}"),
            json!({
                "type": "noul",
                "instructions": format!("Would a person looking for `goal` on a website type the word {word:?} into the site's search box?"),
                "criteria": {
                    "true": "Yes: it names what `goal` is about: a thing, topic, name, place, or what sets it apart",
                    "false": "No: it only holds the sentence together, or says what kind of page or action is wanted (find, page, list, show, information, details)"
                }
            }),
        );
    }
    let answers = jev
        .ask(&json!({ "goal": goal }), &Value::Object(questions))
        .await?;
    Ok(words
        .iter()
        .enumerate()
        .filter(|(n, _)| answers[format!("w{n}")]["noul"].as_f64().unwrap_or(0.0) >= 0.5)
        .map(|(_, w)| (*w).to_owned())
        .collect())
}

/// Words judged per goal, at most: a goal is a sentence, and past this it is a text.
const MAX_JUDGED_WORDS: usize = 24;

/// The parts of `host` that name the site.
fn site_names(host: &str) -> Vec<&str> {
    host.split('.')
        .filter(|part| !HOST_NOISE.contains(part))
        .collect()
}

/// The words saying what `goal` is about, lowercased, in order and once each: without
/// the words that only hold the sentence together or say what kind of page it is, and
/// without the site's own name (`host`).
pub fn content_words(goal: &str, host: &str) -> Vec<String> {
    let site = site_names(host);
    let lower = goal.to_lowercase();
    let tokens = tokens(&lower);
    let mut words: Vec<String> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let token = token.strip_suffix("'s").unwrap_or(token);
        // "sign in", "log in": the particle is part of what the page is about.
        let particle =
            token == "in" && i > 0 && matches!(tokens[i - 1], "sign" | "log" | "check" | "opt");
        if (FILLER.contains(&token) && !particle) || site.contains(&token) {
            continue;
        }
        if !words.iter().any(|w| w == token) {
            words.push(token.to_owned());
        }
    }
    words
}

/// The goal's words: letters and digits, with the `'`, `-`, `_` and `.` inside a word
/// kept (`can't`, `serde_json`, `node.js`, `3.13`).
fn tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '-' | '_' | '.')))
        .map(|t| t.trim_matches(['\'', '-', '_', '.']))
        .filter(|t| !t.is_empty())
        .collect()
}

/// Names in the goal, which a site search finds better alone than among the goal's other
/// words ("the latest version of the serde_json crate": "serde_json", not "latest
/// version serde_json crate", which matches nothing): quoted phrases, and words written
/// like identifiers (`serde_json`, `node.js`, `macOS`, `python3`). Capitalized words
/// aren't names here: in English titles and in German nouns they are too common.
fn names(goal: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        let name = name.trim();
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    };
    for (open, close) in [('"', '"'), ('\u{201c}', '\u{201d}'), ('\u{ab}', '\u{bb}')] {
        let mut rest = goal;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len_utf8()..];
            let Some(end) = after.find(close) else { break };
            push(&after[..end]);
            rest = &after[end + close.len_utf8()..];
        }
    }
    for token in tokens(goal) {
        if identifier_like(token) {
            push(token);
        }
    }
    names
}

/// Written like a name in code or a product name: joined by `_` or `.`, letters and
/// digits mixed, or a capital after a small letter (`macOS`, `PowerShell`). Plain
/// numbers ("2024", "3.13") are left to the query.
fn identifier_like(token: &str) -> bool {
    let letters = token.chars().any(char::is_alphabetic);
    let digits = token.chars().any(|c| c.is_ascii_digit());
    let joined = token.contains(['_', '.']);
    let inner_capital = token
        .chars()
        .zip(token.chars().skip(1))
        .any(|(a, b)| a.is_lowercase() && b.is_uppercase());
    letters && (joined || digits || inner_capital)
}

/// The numbers in a goal or criteria ("under 5 000 kr, 28 inch wheels" → "5000", "28"), as texts
/// a price field or slider can be set to.
pub(crate) fn numbers_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || (c == '.' && !current.is_empty()) {
            current.push(c);
        } else if matches!(c, ' ' | '\u{a0}')
            && !current.is_empty()
            && chars.peek().is_some_and(char::is_ascii_digit)
        {
            // "5 000": a thousands separator, not a gap between numbers.
        } else if !current.is_empty() {
            out.push(
                std::mem::take(&mut current)
                    .trim_end_matches('.')
                    .to_owned(),
            );
        }
    }
    if !current.is_empty() {
        out.push(current.trim_end_matches('.').to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_the_goal_is_about() {
        assert_eq!(
            search_words(
                "the Microsoft support article on how to change the default web browser in Windows",
                "support.microsoft.com"
            )
            .first()
            .map(String::as_str),
            Some("change default web browser windows")
        );
        assert_eq!(
            search_words(
                "the Microsoft support article on how to recover a Microsoft account you can't sign in to",
                "www.microsoft.com"
            )
            .first()
            .map(String::as_str),
            Some("recover account sign in")
        );
        assert_eq!(
            search_words(
                "Chase's page on reporting a lost or stolen card",
                "www.chase.com"
            )
            .first()
            .map(String::as_str),
            Some("reporting lost stolen card")
        );
    }

    #[test]
    fn too_little_left_is_no_query() {
        assert!(search_words("the help center or customer support", "vercel.com").is_empty());
        assert_eq!(
            search_words("the privacy policy", "github.com"),
            ["privacy policy"]
        );
    }

    #[test]
    fn names_are_offered_alone() {
        assert_eq!(
            search_words(
                "the latest version number of the serde_json crate",
                "crates.io"
            ),
            ["latest version number serde_json crate", "serde_json"]
        );
        assert_eq!(
            search_words("the \"Getting started\" guide for node.js", "nodejs.org"),
            ["getting started node.js", "Getting started", "node.js"]
        );
        assert_eq!(
            search_words("how to install macOS updates", "support.apple.com"),
            ["install macos updates", "macOS"]
        );
        // A plain number is part of the query, not a name.
        assert_eq!(
            search_words("the Python 3.13 release schedule", "python.org"),
            ["3.13 release schedule"]
        );
    }
}
