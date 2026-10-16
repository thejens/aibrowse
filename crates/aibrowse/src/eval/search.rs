//! `aibrowse eval search`: the search tool against queries whose good answers are known
//! (official docs and project pages). A query is found when any page the search returns
//! as fulfilling its criterion is one of the expected ones; other pages it returns may
//! qualify too, so they are reported, not counted against it.

use std::path::PathBuf;
use std::time::Instant;

use aibrowse_input::Pace;
use aibrowse_navigator::{Engine, Jev, search};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::search_tools::describe;
use crate::session::Live;

const USAGE: &str =
    "usage: aibrowse eval search [--suite FILE] [--queries a,b] [--profile NAME] [--headful]";

#[derive(Deserialize)]
struct Suite {
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Query {
    name: String,
    query: String,
    criterion: String,
    /// URL fragments of pages known to fulfil the criterion; a hit on any is a find.
    expect: Vec<String>,
    #[serde(default = "default_pages")]
    pages: usize,
}

fn default_pages() -> usize {
    6
}

#[derive(Serialize)]
struct Run {
    name: String,
    found: bool,
    elapsed_ms: u64,
    decisions: usize,
    /// Results on the results page, and how many were opened.
    results: usize,
    opened: usize,
    /// Pages returned as fulfilling the criterion.
    hits: Vec<String>,
    /// Estimated tokens of what the tool returned to the agent.
    returned_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(super) async fn run(args: &[String]) -> anyhow::Result<()> {
    let mut suite_path = PathBuf::from("evals/search.json");
    let mut only: Option<Vec<String>> = None;
    let mut profile = "eval-search".to_owned();
    let mut headless = true;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--suite" => suite_path = PathBuf::from(value()?),
            "--queries" => only = Some(value()?.split(',').map(str::to_owned).collect()),
            "--profile" => profile = value()?,
            "--headful" => headless = false,
            other => bail!("unknown option {other:?}\n{USAGE}"),
        }
    }
    let suite: Suite = serde_json::from_slice(
        &std::fs::read(&suite_path).with_context(|| format!("reading {}", suite_path.display()))?,
    )
    .context("parsing the suite")?;
    let queries: Vec<&Query> = suite
        .queries
        .iter()
        .filter(|q| only.as_ref().is_none_or(|names| names.contains(&q.name)))
        .collect();
    if queries.is_empty() {
        bail!("no queries match the filter");
    }
    let jev = Jev::from_env().context("the search eval needs a Jev key")?;
    let mut live = Live::start(&profile, headless, Pace::Fast, false, true)
        .await
        .context("starting the browser")?;

    let mut runs = Vec::new();
    for query in queries {
        let started = Instant::now();
        let mut run = Run {
            name: query.name.clone(),
            found: false,
            elapsed_ms: 0,
            decisions: 0,
            results: 0,
            opened: 0,
            hits: Vec::new(),
            returned_tokens: 0,
            error: None,
        };
        let engine = Engine::Google;
        match search(
            &mut live,
            &jev,
            &query.query,
            &query.criterion,
            engine,
            query.pages,
        )
        .await
        {
            Ok(report) => {
                run.decisions = report.decisions;
                run.results = report.results;
                run.opened = report.hits.len() + report.misses.len();
                run.hits = report.hits.iter().map(|h| h.url.clone()).collect();
                run.found = run.hits.iter().any(|url| expected(url, &query.expect));
                run.returned_tokens = super::tokens(&describe(&query.criterion, &report));
            }
            Err(err) => run.error = Some(err),
        }
        run.elapsed_ms = started.elapsed().as_millis() as u64;
        eprintln!(
            "{} {:<24} {:>6.1}s {} of {} opened qualify, ~{} tokens{}",
            if run.found { "✓" } else { "✗" },
            run.name,
            run.elapsed_ms as f64 / 1000.0,
            run.hits.len(),
            run.opened,
            run.returned_tokens,
            run.error
                .as_deref()
                .map(|e| format!(" — {e}"))
                .unwrap_or_default(),
        );
        if !run.found {
            eprintln!("    hits: {:?}", run.hits);
        }
        runs.push(run);
    }
    let _ = live.browser.close().await;

    let summary = summarize(&runs);
    println!("{summary}");
    let path = super::report_path("search")?;
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({ "summary": summary, "runs": runs }))?,
    )?;
    println!("Report: {}", path.display());
    Ok(())
}

fn expected(url: &str, expect: &[String]) -> bool {
    let url = url.to_ascii_lowercase();
    expect.iter().any(|e| url.contains(&e.to_ascii_lowercase()))
}

fn summarize(runs: &[Run]) -> String {
    let errors = runs.iter().filter(|r| r.error.is_some()).count();
    let scored = (runs.len() - errors).max(1);
    let found = runs.iter().filter(|r| r.found).count();
    let ok = || runs.iter().filter(|r| r.error.is_none());
    let time = super::median(ok().map(|r| r.elapsed_ms as usize));
    let tokens = super::median(ok().map(|r| r.returned_tokens));
    let hits: usize = ok().map(|r| r.hits.len()).sum();
    let opened: usize = ok().map(|r| r.opened).sum();
    format!(
        "Search eval: {found}/{scored} queries found an expected page ({:.0}%); {hits} of {opened} opened pages qualified; median {:.1}s and ~{tokens} tokens per search; {errors} not scored (errors).",
        found as f64 / scored as f64 * 100.0,
        time as f64 / 1000.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_pages_match_by_url_fragment() {
        let expect = vec!["docs.rs/reqwest".to_owned()];
        assert!(expected("https://docs.rs/reqwest/latest/reqwest/", &expect));
        assert!(!expected("https://hyper.rs/", &expect));
    }
}
