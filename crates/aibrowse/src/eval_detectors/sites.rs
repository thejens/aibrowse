//! Public bot-detector pages: visit each once, wait for its asynchronous checks, and
//! extract a structured verdict.
//!
//! Extraction runs in the isolated world (invisible to the page) and reads rendered
//! text and result cells, not page globals, so it survives most redesigns. When a page
//! changes beyond recognition the detector reports "unparsed" with a text excerpt
//! instead of failing the run.

use std::time::Duration;

use aibrowse_browser::Tab;
use aibrowse_perception::Detail;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep};

/// One detector page and how to read it.
pub struct Site {
    pub name: &'static str,
    pub url: &'static str,
    /// The page runs its checks asynchronously; don't conclude before this.
    min_wait: Duration,
    /// Give up waiting for `ready` after this, and report what is there.
    max_wait: Duration,
    /// An isolated-world expression returning [`Extracted`] as JSON.
    extract: &'static str,
    /// What to do on the page before its verdict appears.
    interaction: Interaction,
}

/// Input some detectors need before they judge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interaction {
    /// Passive fingerprinting: a reader's wheel scroll is all.
    Read,
    /// A behavioral test: fill the email and password fields with throwaway values and
    /// submit, through the same humanized paths agents use.
    LoginForm,
}

/// What an extractor returns.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Extracted {
    /// The page has finished its checks.
    ready: bool,
    /// The page's own headline verdict, if it states one.
    headline: Option<String>,
    checks: Vec<Check>,
    /// Scores the page reports (e.g. CreepJS "like headless" percent), for comparison
    /// across runs; the verdict comes from `checks`.
    scores: std::collections::BTreeMap<String, f64>,
    /// Per-component fingerprint hashes (CreepJS), to pinpoint what differs between runs.
    components: std::collections::BTreeMap<String, String>,
    /// The page text, for evidence when parsing fails.
    excerpt: String,
}

/// One named check on a detector page.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Check {
    pub name: String,
    /// "pass", "warn" or "fail".
    pub status: String,
    #[serde(default)]
    pub value: String,
}

/// A detector's verdict on this browser.
#[derive(Debug, Serialize)]
pub struct SiteReport {
    pub name: String,
    pub url: String,
    /// "pass", "warn", "fail", "unparsed" (the page loaded but couldn't be read) or
    /// "error" (it didn't load).
    pub verdict: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub scores: std::collections::BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub components: std::collections::BTreeMap<String, String>,
    /// The checks that didn't pass (all checks are counted in `summary`).
    pub flagged: Vec<Check>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

/// The detectors, in visiting order. Each is visited once per run.
pub fn all() -> Vec<Site> {
    vec![
        Site {
            name: "sannysoft",
            url: "https://bot.sannysoft.com/",
            min_wait: Duration::from_secs(3),
            max_wait: Duration::from_secs(20),
            extract: include_str!("extract/sannysoft.js"),
            interaction: Interaction::Read,
        },
        Site {
            name: "creepjs",
            url: "https://abrahamjuliot.github.io/creepjs/",
            min_wait: Duration::from_secs(6),
            max_wait: Duration::from_secs(40),
            extract: include_str!("extract/creepjs.js"),
            interaction: Interaction::Read,
        },
        Site {
            name: "browserscan",
            url: "https://www.browserscan.net/bot-detection",
            min_wait: Duration::from_secs(5),
            max_wait: Duration::from_secs(30),
            extract: include_str!("extract/browserscan.js"),
            interaction: Interaction::Read,
        },
        Site {
            name: "deviceandbrowserinfo",
            url: "https://deviceandbrowserinfo.com/are_you_a_bot",
            min_wait: Duration::from_secs(4),
            max_wait: Duration::from_secs(30),
            extract: include_str!("extract/deviceandbrowserinfo.js"),
            interaction: Interaction::Read,
        },
        // The same site's behavioral test: it scores the mouse and keyboard input of a
        // login (speed, CDP mouse leaks) on top of the fingerprint signals.
        Site {
            name: "deviceandbrowserinfo-behavior",
            url: "https://deviceandbrowserinfo.com/are_you_a_bot_interactions",
            min_wait: Duration::from_secs(2),
            max_wait: Duration::from_secs(30),
            extract: include_str!("extract/deviceandbrowserinfo.js"),
            interaction: Interaction::LoginForm,
        },
    ]
}

/// Visits `site` in `tab` and reads its verdict. Never fails: problems become the
/// "error" or "unparsed" verdict.
pub async fn visit(tab: &mut Tab, site: &Site) -> SiteReport {
    let mut report = SiteReport {
        name: site.name.into(),
        url: site.url.into(),
        verdict: "error".into(),
        summary: String::new(),
        headline: None,
        scores: Default::default(),
        components: Default::default(),
        flagged: Vec::new(),
        excerpt: None,
    };
    if let Err(err) = tab.navigate(site.url).await {
        report.summary = format!("didn't load: {err}");
        return report;
    }
    let start = Instant::now();
    sleep(site.min_wait).await;
    match site.interaction {
        // A gentle wheel scroll, as a reader would: some detectors only finish (or only
        // score behavior) once there has been input, and lazy sections render on scroll.
        Interaction::Read => {
            let _ = tab.scroll(0.0, 400.0).await;
        }
        Interaction::LoginForm => {
            if let Err(err) = log_in(tab).await {
                report.verdict = "unparsed".into();
                report.summary = format!("couldn't submit the test form: {err:#}");
                return report;
            }
        }
    }
    let mut extracted = Extracted::default();
    loop {
        match tab.evaluate_isolated_raw(site.extract).await {
            Ok(value) => {
                extracted = serde_json::from_value(value).unwrap_or_default();
                if extracted.ready {
                    break;
                }
            }
            Err(err) => extracted.excerpt = format!("extractor failed: {err}"),
        }
        if start.elapsed() >= site.max_wait {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    judge(&mut report, extracted);
    report
}

/// Fills the page's email and password fields and presses its login button.
async fn log_in(tab: &mut Tab) -> anyhow::Result<()> {
    let model = aibrowse_perception::observe(tab, Detail::All, None).await?;
    let find = |role: &str, word: &str| {
        model
            .nodes
            .iter()
            .find(|n| n.role == role && n.name.to_lowercase().contains(word))
            .and_then(|n| n.reference)
            .with_context(|| format!("no {role} named like {word:?}"))
    };
    let (email, password, submit) = (
        find("textbox", "email")?,
        find("textbox", "password")?,
        find("button", "log")?,
    );
    tab.fill(email, "jane.doe@example.com").await?;
    // Satisfies the form's validation (8+ characters, one of > ! ? @ # $ &), which
    // otherwise blocks the submit silently.
    tab.fill(password, "Correct-horse#7").await?;
    tab.click(submit).await?;
    Ok(())
}

/// Turns what the extractor found into a verdict.
fn judge(report: &mut SiteReport, extracted: Extracted) {
    let excerpt = || Some(extracted.excerpt.chars().take(1500).collect::<String>());
    report.headline.clone_from(&extracted.headline);
    report.scores.clone_from(&extracted.scores);
    report.components.clone_from(&extracted.components);
    if !extracted.ready || (extracted.checks.is_empty() && extracted.scores.is_empty()) {
        report.verdict = "unparsed".into();
        report.summary = if extracted.ready {
            "no checks or scores found on the page".into()
        } else {
            "the page never reported finished checks".into()
        };
        report.excerpt = excerpt();
        return;
    }
    let count = |status: &str| {
        extracted
            .checks
            .iter()
            .filter(|c| c.status == status)
            .count()
    };
    let (fails, warns) = (count("fail"), count("warn"));
    report.flagged = extracted
        .checks
        .iter()
        .filter(|c| c.status != "pass")
        .cloned()
        .collect();
    report.verdict = if fails > 0 {
        "fail"
    } else if warns > 0 {
        "warn"
    } else {
        "pass"
    }
    .into();
    let mut parts = Vec::new();
    if !extracted.checks.is_empty() {
        parts.push(format!(
            "{} checks: {} passed, {warns} warn, {fails} failed",
            extracted.checks.len(),
            extracted.checks.len() - warns - fails
        ));
    }
    if !extracted.scores.is_empty() {
        let scores: Vec<String> = extracted
            .scores
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        parts.push(scores.join(", "));
    }
    if !report.flagged.is_empty() {
        let names: Vec<&str> = report.flagged.iter().map(|c| c.name.as_str()).collect();
        parts.push(format!("flagged: {}", names.join(", ")));
    }
    report.summary = parts.join("; ");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> SiteReport {
        SiteReport {
            name: "x".into(),
            url: "https://x/".into(),
            verdict: String::new(),
            summary: String::new(),
            headline: None,
            scores: Default::default(),
            components: Default::default(),
            flagged: Vec::new(),
            excerpt: None,
        }
    }

    fn check(name: &str, status: &str) -> Check {
        Check {
            name: name.into(),
            status: status.into(),
            value: String::new(),
        }
    }

    #[test]
    fn verdict_is_the_worst_check() {
        let mut r = report();
        judge(
            &mut r,
            Extracted {
                ready: true,
                checks: vec![check("a", "pass"), check("b", "warn"), check("c", "fail")],
                ..Extracted::default()
            },
        );
        assert_eq!(r.verdict, "fail");
        assert_eq!(r.flagged.len(), 2);
        assert!(
            r.summary.contains("3 checks: 1 passed, 1 warn, 1 failed"),
            "{}",
            r.summary
        );
    }

    #[test]
    fn scores_alone_pass_and_unreadable_pages_degrade() {
        let mut r = report();
        judge(
            &mut r,
            Extracted {
                ready: true,
                scores: [("like headless".to_owned(), 31.0)].into(),
                ..Extracted::default()
            },
        );
        assert_eq!(r.verdict, "pass");
        assert!(r.summary.contains("like headless 31"), "{}", r.summary);

        let mut r = report();
        judge(
            &mut r,
            Extracted {
                ready: false,
                excerpt: "Something else entirely".into(),
                ..Extracted::default()
            },
        );
        assert_eq!(r.verdict, "unparsed");
        assert_eq!(r.excerpt.as_deref(), Some("Something else entirely"));
    }
}
