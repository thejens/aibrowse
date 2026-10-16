//! The results page of a web search: which of its links are results, and how likely
//! each is to fulfil the criterion.

use aibrowse_perception::{Detail, NodeKind, PageModel, observe};
use serde_json::{Value, json};

use super::Engine;
use crate::consent::decline_button;
use crate::jev::Jev;
use crate::pursue::Workspace;
use crate::pursuit::{Link, Opening};

/// Loads `query`'s results page with `engine` in the workspace's tab in front, and
/// returns its results, the likeliest to fulfil `criterion` first. With `here` (the page
/// the requester is on), a result in another language or region than it fits less: an
/// engine localizes by where the browser seems to be, not by the site being read
/// (apple.com's lookup took a Ukrainian coverage page). An empty page is an error: the
/// engine may be asking to prove the user is human.
pub(crate) async fn results_page<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    engine: Engine,
    query: &str,
    criterion: &str,
    here: Option<&str>,
) -> Result<Vec<Link>, String> {
    if engine.resting() {
        return Err(format!(
            "{} answered with a challenge lately; not asked again yet",
            engine.name()
        ));
    }
    let results_url = engine.results_url(query);
    workspace
        .tab()
        .navigate(&results_url)
        .await
        .map_err(|e| format!("could not load {results_url}: {e}"))?;
    let mut model = observe(workspace.tab(), Detail::All, None)
        .await
        .map_err(|e| e.to_string())?;
    if model.page.as_ref().is_some_and(|p| p.kind == "captcha") {
        engine.challenged();
        return Err(format!(
            "{} answered with a challenge instead of results (\"I'm not a robot\")",
            engine.name()
        ));
    }
    // Engines in some regions ask about cookies before showing results.
    if let Ok(Some((button, _))) = decline_button(jev, &model).await {
        let _ = workspace.tab().click(button).await;
        model = observe(workspace.tab(), Detail::All, None)
            .await
            .map_err(|e| e.to_string())?;
    }
    // The results are in the page's main region; the rest is the engine's own. A page
    // without one (DuckDuckGo has none) comes back empty, not as an error, so then the
    // whole page is read.
    let main = observe(workspace.tab(), Detail::All, Some("main"))
        .await
        .ok()
        .filter(|main| main.nodes.iter().any(|n| n.kind == NodeKind::Actionable))
        .unwrap_or(model);
    let found = organic_results(jev, &main, engine, query, criterion, here).await?;
    if found.is_empty() {
        return Err(format!(
            "no results on {}'s page for this query (it may be asking to prove the user is human; observe to see, and request_handoff if so)",
            engine.name()
        ));
    }
    Ok(found)
}

/// How sure Jev must be that a link is a result. An absolute link to another site is
/// one, nearly always: an engine's own links are relative or to its own domains (left
/// out before judging), and Jev, asked whether such a link was "a result", doubted the
/// ones that didn't fit the query (0.28–0.43 for DuckDuckGo's results of a `site:`
/// query), as if asked whether they were good ones; the fit question says that. A
/// relative link (Google's opaque `/goto?url=…`) may be the engine's own, and needs
/// Jev's clear yes.
fn result_bar(label: &str) -> f64 {
    match label.rsplit_once(" -> ") {
        Some((_, target)) if target.starts_with("http://") || target.starts_with("https://") => 0.2,
        _ => 0.5,
    }
}

/// A link as Jev is shown it. A relative target is the engine's own address, opaque for
/// a result (Google's `/goto?url=CAES…`), and it made every result look like a link of
/// the engine's own (0.2–0.45 "a result" on a `site:` query): such a link is shown by
/// its name, which holds the address the result leads to.
fn shown(label: &str) -> &str {
    match label.rsplit_once(" -> ") {
        Some((name, target)) if target.starts_with('/') => name,
        _ => label,
    }
}

/// Links on a results page considered as results, at most: a page of results holds
/// about ten, each with a link or two of the engine's own beside it.
const MAX_CONSIDERED: usize = 30;

/// The results on a results page, the likeliest to fulfil `criterion` first. Which
/// links are results can't be told from their URLs (engines route results through
/// opaque redirects, and translations and "more results" links look the same), so Jev
/// judges every link at once, with how well each result fits, in one request.
async fn organic_results(
    jev: &Jev,
    model: &PageModel,
    engine: Engine,
    query: &str,
    criterion: &str,
    here: Option<&str>,
) -> Result<Vec<Link>, String> {
    let mut considered: Vec<Link> = Vec::new();
    for node in &model.nodes {
        if considered.len() >= MAX_CONSIDERED {
            break;
        }
        if node.kind != NodeKind::Actionable || node.role != "link" || node.name.trim().is_empty() {
            continue;
        }
        let (Some(reference), Some(href)) = (node.reference, node.href.as_deref()) else {
            continue;
        };
        if href.is_empty() || href.starts_with('#') {
            continue;
        }
        // An absolute link to the engine's own domains is never a result. Relative ones
        // may be: Google's results go through opaque `/goto?url=…` redirects.
        if let Ok(url) = url::Url::parse(href)
            && url.host_str().is_some_and(|h| engine.owns(h))
        {
            continue;
        }
        let label = format!("link \"{}\" -> {href}", node.name);
        if considered.iter().any(|l| l.label == label) {
            continue;
        }
        considered.push(Link {
            open: Opening::Click(reference),
            named_for_goal: true,
            identity: label.clone(),
            label,
            confidence: 0.0,
        });
    }
    if considered.is_empty() {
        return Ok(Vec::new());
    }
    let unfit = if here.is_some() {
        "Unlikely: it looks like something else, or is in a different language or region than `here`, the page the requester is on"
    } else {
        "Unlikely: it looks like something else"
    };
    let mut questions = serde_json::Map::new();
    for (i, link) in considered.iter().enumerate() {
        let link = shown(&link.label);
        questions.insert(
            format!("result_{i}"),
            json!({
                "type": "noul",
                "instructions": format!("On this results page for `query`, is {link} one of the search results: a page the engine found and lists as an answer?"),
                "criteria": {
                    "true": "A search result: a page from some website (the one the query names with site:, when it does), listed because it matches the query",
                    "false": "Part of the search engine itself: its navigation, settings, a translation or cached copy of a result, an ad, \"more results from\", related searches"
                }
            }),
        );
        questions.insert(
            format!("fit_{i}"),
            json!({
                "type": "noul",
                "instructions": format!("Judging by its title and address, how likely is {link} to fulfil `criterion`?"),
                "criteria": {
                    "true": "Likely: it looks like what `criterion` asks for",
                    "false": unfit
                }
            }),
        );
    }
    let mut state = json!({ "query": query, "criterion": criterion, "results_page": model.title });
    if let Some(here) = here {
        state["here"] = json!(here);
    }
    let answers = jev
        .ask(&state, &Value::Object(questions))
        .await
        .map_err(|e| format!("could not judge the results: {e}"))?;
    let p = |name: String| answers[name]["noul"].as_f64().unwrap_or(0.0);
    for (i, link) in considered.iter().enumerate() {
        tracing::debug!(
            link = %link.label,
            result = p(format!("result_{i}")),
            fit = p(format!("fit_{i}")),
            "judged a results page link"
        );
    }
    let mut results: Vec<Link> = considered
        .into_iter()
        .enumerate()
        .filter(|(i, link)| p(format!("result_{i}")) >= result_bar(&link.label))
        .map(|(i, mut link)| {
            link.confidence = p(format!("fit_{i}"));
            link
        })
        .collect();
    // Stable: equally fitting results keep the engine's order.
    results.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    Ok(results)
}

/// The name in a link label (`link "Name" -> …`).
fn link_name(label: &str) -> String {
    label
        .strip_prefix("link \"")
        .and_then(|rest| rest.rsplit_once("\" -> "))
        .map_or_else(|| label.to_owned(), |(name, _)| name.to_owned())
}

/// A result's name as the engine lists it, without the address line Google writes
/// into the link ("Title Site https://site.com › path").
pub(super) fn result_name(label: &str) -> String {
    let name = link_name(label);
    match name.find(" https://").or_else(|| name.find(" http://")) {
        Some(at) if at > 0 => name[..at].trim_end().to_owned(),
        _ => name,
    }
}

/// The URL in a link label (`link "…" -> https://…`), for results that never opened.
pub(super) fn link_target(label: &str) -> String {
    label
        .rsplit_once(" -> ")
        .map(|(_, url)| url.to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_to_other_sites_need_less_to_count_as_results() {
        assert_eq!(
            result_bar("link \"Bootfitting\" -> https://skidshop.se/pages/bootfitting"),
            0.2
        );
        assert_eq!(
            result_bar("link \"Bootfitting skidshop.se\" -> /goto?url=CAES"),
            0.5
        );
    }

    #[test]
    fn link_names_come_from_labels() {
        assert_eq!(
            link_name("link \"Rust \"docs\"\" -> https://x.rs/"),
            "Rust \"docs\""
        );
        assert_eq!(link_name("button \"Go\""), "button \"Go\"");
    }

    #[test]
    fn result_names_leave_out_the_address_line() {
        assert_eq!(
            result_name(
                "link \"yaml_serde - Rust Docs.rs https://docs.rs › yaml_serde\" -> /goto?url=CAES"
            ),
            "yaml_serde - Rust Docs.rs"
        );
        assert_eq!(
            result_name("link \"Rust\" -> https://www.rust-lang.org/"),
            "Rust"
        );
    }

    #[test]
    fn link_targets_come_from_labels() {
        assert_eq!(
            link_target("link \"Rust\" -> https://www.rust-lang.org/"),
            "https://www.rust-lang.org/"
        );
    }
}
