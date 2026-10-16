//! `aibrowse eval digest`: what `read(view="digest")` costs and keeps. For each page in
//! the suite: the tokens of the digest against those of `read`'s first page, the
//! page's whole text and the default outline; and how many of the page's must-keep
//! facts (for the case's focus) the digest still has, against `read`'s first page.

use std::path::PathBuf;

use aibrowse_browser::{Browser, LaunchOptions};
use aibrowse_navigator::{DEFAULT_DIGEST_TOKENS, Jev, digest};
use aibrowse_perception::{DEFAULT_PAGE_CHARS, Detail, main_text, observe, read};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

const USAGE: &str = "usage: aibrowse eval digest [--suite FILE] [--profile NAME] [--headful] [--tokens N] [--no-jev]";

#[derive(Deserialize)]
struct Suite {
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct Page {
    url: String,
    focus: Option<String>,
    /// Facts (exact text) a good digest for `focus` must keep.
    keep: Vec<String>,
}

#[derive(Serialize)]
struct Run {
    url: String,
    digest_tokens: usize,
    read_tokens: usize,
    full_tokens: usize,
    outline_tokens: usize,
    /// Must-keep facts present in the digest, and in `read`'s first page.
    kept: usize,
    kept_by_read: usize,
    of: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    digest: String,
}

pub(super) async fn run(args: &[String]) -> anyhow::Result<()> {
    let mut suite_path = PathBuf::from("evals/digest.json");
    let mut profile = "eval-digest".to_owned();
    let mut headless = true;
    let mut budget = DEFAULT_DIGEST_TOKENS;
    let mut use_jev = true;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--suite" => suite_path = PathBuf::from(value()?),
            "--profile" => profile = value()?,
            "--headful" => headless = false,
            "--tokens" => budget = value()?.parse().context("--tokens takes a number")?,
            "--no-jev" => use_jev = false,
            other => bail!("unknown option {other:?}\n{USAGE}"),
        }
    }
    let suite: Suite = serde_json::from_slice(
        &std::fs::read(&suite_path).with_context(|| format!("reading {}", suite_path.display()))?,
    )
    .context("parsing the suite")?;
    let jev = if use_jev {
        Some(Jev::from_env().context("the digest eval uses Jev (or pass --no-jev)")?)
    } else {
        None
    };
    let browser = Browser::launch(LaunchOptions {
        profile,
        headless,
        mute: true,
        ..LaunchOptions::default()
    })
    .await?;
    let mut tab = browser.open_tab().await?;

    let mut runs = Vec::new();
    for page in &suite.pages {
        let mut run = Run {
            url: page.url.clone(),
            digest_tokens: 0,
            read_tokens: 0,
            full_tokens: 0,
            outline_tokens: 0,
            kept: 0,
            kept_by_read: 0,
            of: page.keep.len(),
            error: None,
            digest: String::new(),
        };
        let measured = async {
            tab.navigate(&page.url).await.map_err(|e| e.to_string())?;
            let first_page = read(&mut tab, 0, DEFAULT_PAGE_CHARS)
                .await
                .map_err(|e| e.to_string())?;
            let full = main_text(&mut tab).await.map_err(|e| e.to_string())?;
            let outline = observe(&mut tab, Detail::Interactive, None)
                .await
                .map_err(|e| e.to_string())?
                .render();
            let digest = digest(&mut tab, jev.as_ref(), page.focus.as_deref(), budget).await?;
            Ok::<_, String>((first_page, full, outline, digest))
        };
        match measured.await {
            Ok((first_page, full, outline, digest)) => {
                run.read_tokens = super::tokens(&first_page);
                run.full_tokens = super::tokens(&full);
                run.outline_tokens = super::tokens(&outline);
                run.digest_tokens = super::tokens(&digest);
                run.kept = count_kept(&digest, &page.keep);
                run.kept_by_read = count_kept(&first_page, &page.keep);
                run.digest = digest;
            }
            Err(err) => run.error = Some(err),
        }
        eprintln!(
            "{:<70} digest ~{:>5} | read ~{:>5} | full ~{:>6} | outline ~{:>5} | kept {}/{} (read {}/{}){}",
            run.url.chars().take(70).collect::<String>(),
            run.digest_tokens,
            run.read_tokens,
            run.full_tokens,
            run.outline_tokens,
            run.kept,
            run.of,
            run.kept_by_read,
            run.of,
            run.error
                .as_deref()
                .map(|e| format!(" — {e}"))
                .unwrap_or_default(),
        );
        runs.push(run);
    }
    drop(tab);
    browser.close().await?;

    let ok: Vec<&Run> = runs.iter().filter(|r| r.error.is_none()).collect();
    let kept: usize = ok.iter().map(|r| r.kept).sum();
    let kept_by_read: usize = ok.iter().map(|r| r.kept_by_read).sum();
    let of: usize = ok.iter().map(|r| r.of).sum();
    let summary = format!(
        "Digest eval: median ~{} tokens per digest vs ~{} for read's first page and ~{} for the outline; kept {kept}/{of} must-keep facts (read's first page {kept_by_read}/{of}); {} pages not measured.",
        super::median(ok.iter().map(|r| r.digest_tokens)),
        super::median(ok.iter().map(|r| r.read_tokens)),
        super::median(ok.iter().map(|r| r.outline_tokens)),
        runs.len() - ok.len(),
    );
    println!("{summary}");
    let path = super::report_path("digest")?;
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({ "summary": summary, "runs": runs }))?,
    )?;
    println!("Report: {}", path.display());
    Ok(())
}

/// How many of `facts` appear in `text`, ignoring case and spacing differences.
fn count_kept(text: &str, facts: &[String]) -> usize {
    let squash = |s: &str| {
        s.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let text = squash(text);
    facts.iter().filter(|f| text.contains(&squash(f))).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_are_found_regardless_of_case_and_spacing() {
        let facts = vec!["10 December 1815".to_owned(), "maxsize".to_owned()];
        assert_eq!(count_kept("born 10  december\n1815 in London", &facts), 1);
    }
}
