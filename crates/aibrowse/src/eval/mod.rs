//! `aibrowse eval <suite>`: the navigator eval (navigator.rs), the search eval
//! (search.rs), the survey eval (survey.rs) and the detector harness (eval_detectors).

mod digest;
mod navigator;
mod search;
mod survey;
mod tabs;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    match args.split_first().map(|(a, r)| (a.as_str(), r)) {
        Some(("navigator", rest)) => navigator::run(rest).await,
        Some(("search", rest)) => search::run(rest).await,
        Some(("survey", rest)) => survey::run(rest).await,
        Some(("digest", rest)) => digest::run(rest).await,
        Some(("detectors", rest)) => crate::eval_detectors::run(rest).await,
        _ => {
            bail!(
                "{}\n       aibrowse eval search [--suite FILE] [--queries a,b] [--profile NAME] [--headful]\n       aibrowse eval survey [--suite FILE] [--surveys a,b] [--profile NAME] [--headful]\n       aibrowse eval digest [--suite FILE] [--profile NAME] [--headful] [--tokens N] [--no-jev]\n       aibrowse eval detectors [--headful] [--profile NAME] [--local]",
                navigator::USAGE
            )
        }
    }
}

/// Where a report of `kind` goes: `~/.aibrowse/evals/<kind>-<unix seconds>.json`.
fn report_path(kind: &str) -> anyhow::Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("no home directory")?
        .join(".aibrowse/evals");
    std::fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(dir.join(format!("{kind}-{stamp}.json")))
}

/// A rough token count for text an agent receives: about four characters per token
/// for English prose and markup. Nothing tokenizes locally; this is for comparing
/// runs and tools, not for billing.
fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// The middle value (the upper one of an even count), or 0 for none.
fn median(values: impl Iterator<Item = usize>) -> usize {
    let mut values: Vec<usize> = values.collect();
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_and_token_estimates() {
        assert_eq!(median([3, 1, 2].into_iter()), 2);
        assert_eq!(median(std::iter::empty()), 0);
        assert_eq!(tokens("abcd"), 1);
        assert_eq!(tokens("abcde"), 2);
    }
}
