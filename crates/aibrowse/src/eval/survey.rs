//! `aibrowse eval survey`: the survey tool on shops whose relevant pages are known, in
//! several countries and kinds of goods. A site is found when its navigator reached the
//! goal on a page whose URL or title names it, and the quoted text carries the facts
//! asked for (a currency sign: a price came back).

use std::path::PathBuf;
use std::time::Instant;

use aibrowse_input::Pace;
use aibrowse_navigator::{Ask, Finding, Jev, survey};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::session::Live;
use crate::survey_tools::describe;

const USAGE: &str =
    "usage: aibrowse eval survey [--suite FILE] [--surveys a,b] [--profile NAME] [--headful]";
/// Text per site, as an agent would ask for it by default.
const EXCERPT_TOKENS: usize = 250;

#[derive(Deserialize)]
struct Suite {
    surveys: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    goal: String,
    #[serde(default)]
    texts: Vec<String>,
    focus: Option<String>,
    /// Criteria to narrow each listing to with its own filters.
    filters: Option<String>,
    /// With filters: the most any price quoted from a narrowed listing may be.
    max_price: Option<f64>,
    sites: Vec<Site>,
}

#[derive(Deserialize)]
struct Site {
    url: String,
    /// Fragments of the URL or title of a page that answers; any one will do.
    expect: Vec<String>,
    /// What the quoted text must contain, every one.
    #[serde(default)]
    facts: Vec<String>,
}

#[derive(Serialize)]
struct Run {
    name: String,
    elapsed_ms: u64,
    decisions: usize,
    sites: Vec<SiteRun>,
    returned_tokens: usize,
}

#[derive(Serialize)]
struct SiteRun {
    url: String,
    found: bool,
    reached: bool,
    final_url: String,
    why: String,
    missing_facts: Vec<String>,
    /// With filters: whether they were set, why not, and the prices quoted after.
    #[serde(skip_serializing_if = "Option::is_none")]
    filtered: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    prices: Vec<f64>,
}

pub(super) async fn run(args: &[String]) -> anyhow::Result<()> {
    let mut suite_path = PathBuf::from("evals/survey.json");
    let mut only: Option<Vec<String>> = None;
    let mut profile = "eval-survey".to_owned();
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
            "--surveys" => only = Some(value()?.split(',').map(str::to_owned).collect()),
            "--profile" => profile = value()?,
            "--headful" => headless = false,
            other => bail!("unknown option {other:?}\n{USAGE}"),
        }
    }
    let suite: Suite = serde_json::from_slice(
        &std::fs::read(&suite_path).with_context(|| format!("reading {}", suite_path.display()))?,
    )
    .context("parsing the suite")?;
    let cases: Vec<&Case> = suite
        .surveys
        .iter()
        .filter(|c| only.as_ref().is_none_or(|names| names.contains(&c.name)))
        .collect();
    if cases.is_empty() {
        bail!("no surveys match the filter");
    }
    let jev = Jev::from_env().context("the survey eval needs a Jev key")?;
    let mut live = Live::start(&profile, headless, Pace::Fast, false, true)
        .await
        .context("starting the browser")?;

    let mut runs = Vec::new();
    for case in cases {
        let started = Instant::now();
        let urls: Vec<String> = case.sites.iter().map(|s| s.url.clone()).collect();
        let report = survey(
            &mut live,
            &jev,
            &urls,
            Ask {
                goal: &case.goal,
                texts: &case.texts,
                filters: case.filters.as_deref(),
                focus: case.focus.as_deref(),
                excerpt_tokens: EXCERPT_TOKENS,
            },
        )
        .await;
        let sites: Vec<SiteRun> = case
            .sites
            .iter()
            .map(|site| {
                let finding = report.findings.iter().find(|f| f.site == site.url);
                score(site, finding, case.max_price)
            })
            .collect();
        let run = Run {
            name: case.name.clone(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            decisions: report.decisions,
            returned_tokens: super::tokens(&describe(&case.goal, urls.len(), &report)),
            sites,
        };
        let found = run.sites.iter().filter(|s| s.found).count();
        eprintln!(
            "{:<24} {found}/{} sites {:>6.1}s ~{} tokens",
            run.name,
            run.sites.len(),
            run.elapsed_ms as f64 / 1000.0,
            run.returned_tokens,
        );
        for site in run.sites.iter().filter(|s| !s.found) {
            eprintln!(
                "    ✗ {} → {} ({}{})",
                site.url,
                site.final_url,
                if site.reached { "reached" } else { &site.why },
                if site.missing_facts.is_empty() {
                    String::new()
                } else {
                    format!("; missing {:?}", site.missing_facts)
                },
            );
            if let Some(filtered) = &site.filtered {
                eprintln!(
                    "        filters {filtered}; prices quoted {:?}",
                    site.prices
                );
            }
        }
        runs.push(run);
    }
    let _ = live.browser.close().await;

    let summary = summarize(&runs);
    println!("{summary}");
    let path = super::report_path("survey")?;
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({ "summary": summary, "runs": runs }))?,
    )?;
    println!("Report: {}", path.display());
    Ok(())
}

/// Whether `finding` answers for `site`: reached, on a page named for it, with the facts;
/// with `max_price`, also narrowed by its filters, the median price quoted within it
/// (the excerpt also quotes the filter panel's own bands, "$100.00 to $199.99", and
/// what items cost before, "was £599").
fn score(site: &Site, finding: Option<&Finding>, max_price: Option<f64>) -> SiteRun {
    let Some(finding) = finding else {
        return SiteRun {
            url: site.url.clone(),
            found: false,
            reached: false,
            final_url: String::new(),
            why: "not surveyed".into(),
            missing_facts: site.facts.clone(),
            filtered: None,
            prices: Vec::new(),
        };
    };
    let named = format!("{} {}", finding.url, finding.title).to_lowercase();
    let on_page = site
        .expect
        .iter()
        .any(|e| named.contains(&e.to_lowercase()));
    let text = finding.excerpt.to_lowercase();
    let missing_facts: Vec<String> = site
        .facts
        .iter()
        .filter(|f| !text.contains(&f.to_lowercase()))
        .cloned()
        .collect();
    let prices = if max_price.is_some() {
        prices_in(&finding.excerpt)
    } else {
        Vec::new()
    };
    let within_price = match max_price {
        None => true,
        Some(max) => {
            let mut sorted = prices.clone();
            sorted.sort_by(f64::total_cmp);
            let median = sorted.get(sorted.len() / 2).copied();
            finding.filtered.as_ref().is_some_and(|f| f.done) && median.is_some_and(|m| m <= max)
        }
    };
    SiteRun {
        url: site.url.clone(),
        found: finding.reached && on_page && missing_facts.is_empty() && within_price,
        reached: finding.reached,
        final_url: finding.url.clone(),
        why: finding.why.clone(),
        missing_facts,
        filtered: finding.filtered.as_ref().map(|f| {
            if f.done {
                format!("set: {}", f.steps.join(" → "))
            } else {
                format!("not set ({}): {}", f.why, f.steps.join(" → "))
            }
        }),
        prices,
    }
}

/// Prices in quoted text: numbers next to a currency ("6 999 kr", "4049:-", "€1,299",
/// "379,99 €", "$129.99"). A space, dot or comma before exactly three digits groups
/// thousands; a comma or dot before two digits is the decimal mark. Below 10, a number
/// is no price (a rating, a size).
fn prices_in(text: &str) -> Vec<f64> {
    let chars: Vec<char> = text.chars().collect();
    let digits_at = |i: usize| chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut whole = String::new();
        let mut cents = String::new();
        loop {
            let run = digits_at(i);
            whole.extend(&chars[i..i + run]);
            i += run;
            let Some(&sep) = chars.get(i) else { break };
            let next = if i + 1 < chars.len() {
                digits_at(i + 1)
            } else {
                0
            };
            if matches!(sep, ' ' | '\u{a0}' | '.' | ',') && next == 3 {
                i += 1;
            } else if matches!(sep, ',' | '.') && next == 2 {
                cents.extend(&chars[i + 1..i + 3]);
                i += 3;
                break;
            } else {
                break;
            }
        }
        let before: String = chars[start.saturating_sub(2)..start].iter().collect();
        let after: String = chars[i..(i + 4).min(chars.len())].iter().collect();
        let currency = |s: &str| {
            let s = s.trim().to_lowercase();
            s.starts_with("kr")
                || s.starts_with(":-")
                || s.starts_with("sek")
                || s.starts_with('€')
                || s.starts_with('£')
                || s.starts_with('$')
                || s.ends_with('€')
                || s.ends_with('£')
                || s.ends_with('$')
        };
        if (currency(&after) || currency(&before))
            && let Ok(n) =
                format!("{whole}.{}", if cents.is_empty() { "0" } else { &cents }).parse::<f64>()
            && n >= 10.0
        {
            out.push(n);
        }
    }
    out
}

fn summarize(runs: &[Run]) -> String {
    let sites: usize = runs.iter().map(|r| r.sites.len()).sum();
    let found: usize = runs
        .iter()
        .map(|r| r.sites.iter().filter(|s| s.found).count())
        .sum();
    let reached: usize = runs
        .iter()
        .map(|r| r.sites.iter().filter(|s| s.reached).count())
        .sum();
    let time = super::median(runs.iter().map(|r| r.elapsed_ms as usize));
    let tokens = super::median(runs.iter().map(|r| r.returned_tokens));
    format!(
        "Survey eval: {found}/{sites} sites answered ({:.0}%), {reached} reached; median {:.1}s and ~{tokens} tokens per survey of {} surveys.",
        found as f64 / sites.max(1) as f64 * 100.0,
        time as f64 / 1000.0,
        runs.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(url: &str, excerpt: &str) -> Finding {
        Finding {
            site: "https://shop.example".into(),
            url: url.into(),
            title: "Hybrid bikes".into(),
            reached: true,
            why: String::new(),
            excerpt: excerpt.into(),
            steps: Vec::new(),
            filtered: None,
            filters_offered: Vec::new(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn a_site_answers_with_its_page_and_facts() {
        let site = Site {
            url: "https://shop.example".into(),
            expect: vec!["hybrid".into()],
            facts: vec!["kr".into()],
        };
        let page = finding("https://shop.example/c/1", "City 6 999 kr");
        assert!(score(&site, Some(&page), None).found);
        let no_price = score(
            &site,
            Some(&finding("https://shop.example/c/1", "City bikes")),
            None,
        );
        assert!(!no_price.found);
        assert_eq!(no_price.missing_facts, ["kr"]);
        assert!(!score(&site, None, None).found);
        // Narrowed to under 5000: filtered, and every price quoted within it.
        assert!(
            !score(&site, Some(&page), Some(5000.0)).found,
            "not filtered"
        );
    }

    #[test]
    fn prices_are_numbers_beside_a_currency() {
        assert_eq!(
            prices_in("City 6 999 kr, Trail 4049:-, Pro €1,299 and $129.99, 28 inch, 4.5/5"),
            [6999.0, 4049.0, 1299.0, 129.99]
        );
        assert_eq!(prices_in("379,99 € statt 1.299,00 €"), [379.99, 1299.0]);
    }
}
