//! `aibrowse eval navigator`: the navigator eval. Every site × every goal
//! from a suite file, each starting from the site's home page, scored against the A4 exit
//! bar (≥80% success, median under 5s).
//!
//! A run counts as a success only when the navigator says it reached the goal *and* the
//! final URL or title carries one of the goal's keywords, so a confident "reached" on the
//! wrong page is caught as a false positive rather than counted.

use aibrowse_frontdoor::{Fetcher, FrontDoors};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use aibrowse_browser::{Browser, LaunchOptions};
use aibrowse_input::Pace;
use aibrowse_navigator::{Jev, Limits, Outcome, Report, Workspace, pursue, pursue_parts};
use aibrowse_perception::{Detail, main_text, observe};

use super::tabs::EvalTabs;
use crate::respond::{describe_parts, describe_report};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

pub(super) const USAGE: &str = "usage: aibrowse eval navigator [--suite FILE] [--sites TEXT] [--goals a,b] [--profile NAME] [--headful] [--kb FILE|off] [--no-texts] [--no-lookup]";

#[derive(Deserialize)]
struct Suite {
    /// Every one of `goals` is pursued on every one of `sites`...
    #[serde(default)]
    goals: BTreeMap<String, Goal>,
    #[serde(default)]
    sites: Vec<String>,
    /// ...and each case on its own site, for goals only one site has.
    #[serde(default)]
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    site: String,
    name: String,
    #[serde(flatten)]
    goal: Goal,
}

#[derive(Deserialize)]
struct Goal {
    goal: String,
    /// Keywords, one of which the final URL or title must contain.
    expect: Vec<String>,
    /// Keywords all of which it must contain besides: a page both sorted and filtered.
    #[serde(default)]
    require: Vec<String>,
    /// What the calling agent would type: offered to the navigator as a suggested text,
    /// and typed by the eval, as the agent would, when the navigator hands back for
    /// text anyway. Without it, a hand-back for text ends the run.
    #[serde(default)]
    query: Option<String>,
    /// URL or title fragments that rule a page out even when it has a keyword (a
    /// search results page whose query holds the keyword is not the article).
    #[serde(default)]
    avoid: Vec<String>,
    /// For a goal that asks for a fact (a price, a date): text one of which the final
    /// page must show, so that reaching the right address without the answer on it
    /// doesn't count.
    #[serde(default)]
    answer: Vec<String>,
    /// Parts done after `goal`, in order, as `navigate(goal, then)` takes them ("open
    /// the filters", then "set the max price"): the run succeeds only when every part
    /// is reached and the last page matches.
    #[serde(default)]
    then: Vec<String>,
    /// Texts the agent offers up front besides `query` (a price, a cell), as
    /// `navigate(texts)` takes them.
    #[serde(default)]
    texts: Vec<String>,
    /// Keys offered with what they do ("Meta+d: fill the selection down").
    #[serde(default)]
    keys: Vec<String>,
}

/// Times the eval plays the agent's part (typing `query`) per run, at most.
const AGENT_TURNS: usize = 2;

/// One run: the navigator's pursuits, with the agent's turns between them.
struct Pursued {
    outcome: Outcome,
    /// Estimated tokens of what the navigate calls returned to the agent.
    returned_tokens: usize,
    steps: Vec<String>,
    trail: Vec<aibrowse_navigator::Decision>,
    decisions: usize,
    elapsed: std::time::Duration,
    agent_turns: usize,
    /// For a goal in parts: the parts pursued (up to the first not reached) and how
    /// many there are.
    parts: Option<(usize, usize)>,
}

impl Pursued {
    /// Adds a pursuit's steps, decisions and time; its outcome becomes the run's.
    fn absorb(&mut self, report: Report) {
        self.decisions += report.decisions;
        self.elapsed += report.elapsed;
        self.steps.extend(
            report
                .steps
                .iter()
                .map(|s| format!("{} ({:.2})", s.action, s.confidence)),
        );
        self.trail.extend(report.trail);
        self.outcome = report.outcome;
    }

    /// The navigator says it reached the goal: every part of it, for a goal in parts.
    fn reached(&self) -> bool {
        matches!(self.outcome, Outcome::Reached)
            && self.parts.is_none_or(|(pursued, all)| pursued == all)
    }
}

/// What navigate(goal) hands the agent besides the report: the page outline (the tool
/// sends what changed; the whole outline is the most it can be).
async fn outline_tokens(tabs: &mut EvalTabs<'_>) -> usize {
    let outline = observe(tabs.tab(), Detail::Interactive, None)
        .await
        .map(|m| m.render())
        .unwrap_or_default();
    super::tokens(&outline)
}

/// Pursues `goal`, and when the navigator hands back for text and the goal says what
/// to type, types it (and Enter) as the agent would, then pursues again.
async fn pursue_with_agent(
    tabs: &mut EvalTabs<'_>,
    jev: &Jev,
    goal: &Goal,
    limits: &Limits,
    no_texts: bool,
) -> Result<Pursued, aibrowse_navigator::JevError> {
    let mut run = Pursued {
        outcome: Outcome::OutOfBudget,
        returned_tokens: 0,
        steps: Vec::new(),
        trail: Vec::new(),
        decisions: 0,
        elapsed: std::time::Duration::ZERO,
        agent_turns: 0,
        parts: None,
    };
    // The query is also offered up front, as an agent would suggest it.
    let texts: Vec<String> = goal
        .query
        .iter()
        .filter(|_| !no_texts)
        .chain(&goal.texts)
        .cloned()
        .collect();
    if !goal.then.is_empty() {
        let goals: Vec<String> = std::iter::once(&goal.goal)
            .chain(&goal.then)
            .cloned()
            .collect();
        let limits = Limits {
            keys: goal.keys.clone(),
            ..limits.clone()
        };
        let parts = pursue_parts(tabs, jev, &goals, &texts, &limits).await?;
        run.returned_tokens =
            super::tokens(&describe_parts(&goals, &parts, &[], false)) + outline_tokens(tabs).await;
        run.parts = Some((parts.len(), goals.len()));
        for (_, report) in parts {
            run.absorb(report);
        }
        return Ok(run);
    }
    loop {
        let report = pursue(tabs, jev, &goal.goal, &texts, limits).await?;
        run.returned_tokens += super::tokens(&describe_report(
            &goal.goal,
            &report,
            &report.tab_notes,
            false,
        )) + outline_tokens(tabs).await;
        run.absorb(report);
        let (
            Outcome::NeedsAgent {
                field: Some((field, label)),
            },
            Some(query),
        ) = (&run.outcome, &goal.query)
        else {
            return Ok(run);
        };
        if run.agent_turns >= AGENT_TURNS {
            return Ok(run);
        }
        run.agent_turns += 1;
        let started = std::time::Instant::now();
        let typed = match tabs.tab().fill(*field, query).await {
            Ok(_) => tabs.tab().press("Enter").await.map(drop),
            Err(err) => Err(err),
        };
        run.elapsed += started.elapsed();
        run.steps.push(format!(
            "[agent] typed {query:?} into {label} and pressed Enter"
        ));
        if let Err(err) = typed {
            run.steps.push(format!("[agent] typing failed: {err}"));
            return Ok(run);
        }
    }
}

#[derive(Serialize)]
struct Run {
    site: String,
    goal: String,
    /// Reached, needs_user, needs_agent, stuck, unsure, consequential, out_of_budget,
    /// or error.
    outcome: String,
    /// The navigator claimed the goal and the page matches it.
    success: bool,
    /// The navigator claimed the goal but the page doesn't match.
    false_positive: bool,
    elapsed_ms: u64,
    steps: usize,
    decisions: usize,
    /// Estimated tokens the agent received from navigate (report and outline), at
    /// most: the token-efficiency side of the eval.
    returned_tokens: usize,
    /// Times the eval typed the goal's query for the navigator, as the agent would.
    #[serde(skip_serializing_if = "is_zero")]
    agent_turns: usize,
    /// For a goal in parts, "pursued/all": how far it got.
    #[serde(skip_serializing_if = "Option::is_none")]
    parts: Option<String>,
    final_url: String,
    title: String,
    /// What the navigator did, in order.
    steps_taken: Vec<String>,
    /// The options it weighed when it stopped short (unsure, stuck), with confidences;
    /// for gave_up, the links explored and why each branch gave up.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    options: Vec<(String, f64)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Each page's decision: options offered, whose turn Jev judged it, and its
    /// best-ranked next steps, for seeing why a run went where it did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    trail: Vec<aibrowse_navigator::Decision>,
}

struct Options {
    suite: PathBuf,
    sites: Option<String>,
    goals: Option<Vec<String>>,
    profile: String,
    headless: bool,
    kb: Option<PathBuf>,
    /// Don't offer a goal's `query` up front: the navigator gets it only by handing back,
    /// as from an agent that passes no `texts`.
    no_texts: bool,
    /// No look-up across the site when its navigation stops short, as when narrowing a
    /// listing with its own filters (the survey's second phase).
    no_lookup: bool,
}

pub(super) async fn run(args: &[String]) -> anyhow::Result<()> {
    let options = parse(args)?;
    let suite: Suite = serde_json::from_slice(
        &std::fs::read(&options.suite)
            .with_context(|| format!("reading {}", options.suite.display()))?,
    )
    .context("parsing the suite")?;
    let jev = Jev::from_env().context("the navigator eval needs a Jev key")?;
    let site_wanted = |site: &str| {
        options
            .sites
            .as_ref()
            .is_none_or(|f| site.contains(f.as_str()))
    };
    let goal_wanted = |name: &str| {
        options
            .goals
            .as_ref()
            .is_none_or(|g| g.iter().any(|n| n == name))
    };
    let mut plan: Vec<(&str, &str, &Goal)> = Vec::new();
    for site in suite.sites.iter().filter(|s| site_wanted(s)) {
        for (name, goal) in suite.goals.iter().filter(|(n, _)| goal_wanted(n)) {
            plan.push((site, name, goal));
        }
    }
    for case in suite
        .cases
        .iter()
        .filter(|c| site_wanted(&c.site) && goal_wanted(&c.name))
    {
        plan.push((&case.site, &case.name, &case.goal));
    }
    if plan.is_empty() {
        bail!("no sites or goals match the filters");
    }

    let browser = Browser::launch(LaunchOptions {
        profile: options.profile.clone(),
        headless: options.headless,
        mute: true,
        ..LaunchOptions::default()
    })
    .await?;
    let mut home = browser.open_tab().await?;
    // Speed is what A4 measures; the eval profile has no account to protect.
    home.set_pace(Pace::Fast);
    let mut tabs = EvalTabs {
        browser: &browser,
        home,
        opened: Vec::new(),
        kb: options.kb.clone(),
        doors: FrontDoors::new(Fetcher::new(
            &browser.user_agent().await?,
            browser
                .profile()
                .egress
                .as_ref()
                .map(|e| e.client_proxy())
                .as_deref(),
        )),
    };
    let limits = Limits {
        lookup: !options.no_lookup,
        ..Limits::default()
    };
    let mut runs = Vec::new();
    for &(site, name, goal) in &plan {
        let mut run = Run {
            site: site.to_owned(),
            goal: name.to_owned(),
            outcome: "error".into(),
            success: false,
            false_positive: false,
            elapsed_ms: 0,
            steps: 0,
            decisions: 0,
            returned_tokens: 0,
            agent_turns: 0,
            parts: None,
            final_url: String::new(),
            title: String::new(),
            steps_taken: Vec::new(),
            options: Vec::new(),
            error: None,
            trail: Vec::new(),
        };
        tabs.close_opened().await;
        match tabs.home.navigate(site).await {
            Err(err) => run.error = Some(format!("home page didn't load: {err}")),
            Ok(_) => {
                match pursue_with_agent(&mut tabs, &jev, goal, &limits, options.no_texts).await {
                    Ok(report) => {
                        let reached = report.reached();
                        run.outcome = outcome_name(&report.outcome).into();
                        run.elapsed_ms = report.elapsed.as_millis() as u64;
                        run.steps = report.steps.len();
                        run.decisions = report.decisions;
                        run.agent_turns = report.agent_turns;
                        run.parts = report
                            .parts
                            .map(|(pursued, all)| format!("{pursued}/{all}"));
                        run.returned_tokens = report.returned_tokens;
                        run.steps_taken = report.steps;
                        run.trail = report.trail;
                        if let Outcome::Unsure { options } | Outcome::Stuck { options } =
                            &report.outcome
                        {
                            run.options.clone_from(options);
                        }
                        if let Outcome::GaveUp { tried } = &report.outcome {
                            run.options = tried
                                .iter()
                                .map(|(label, why)| (format!("{label}: {why}"), 0.0))
                                .collect();
                        }
                        if let Outcome::Looping { steps } = &report.outcome {
                            run.options = steps.iter().map(|s| (s.clone(), 1.0)).collect();
                        }
                        if let Outcome::Consequential { label, .. } = &report.outcome {
                            run.options = vec![(label.clone(), 1.0)];
                        }
                        // Judged on the tab the navigator ended in (it follows new tabs).
                        run.final_url = tabs.tab().url();
                        run.title = tabs.tab().title().await;
                        let shows_answer = goal.answer.is_empty() || {
                            let text = unlinked(&main_text(tabs.tab()).await.unwrap_or_default());
                            goal.answer.iter().any(|a| text.contains(a.as_str()))
                        };
                        let matches = matches_goal(&run.final_url, &run.title, &goal.expect)
                            && goal.require.iter().all(|word| {
                                matches_goal(&run.final_url, &run.title, std::slice::from_ref(word))
                            })
                            && !matches_goal(&run.final_url, &run.title, &goal.avoid)
                            && shows_answer;
                        run.success = reached && matches;
                        run.false_positive = reached && !matches;
                    }
                    Err(err) => run.error = Some(format!("navigator failed: {err}")),
                }
            }
        }
        eprintln!(
            "{} {:<28} {:<8} {:>6.1}s {}",
            if run.success { "✓" } else { "✗" },
            run.site,
            run.goal,
            run.elapsed_ms as f64 / 1000.0,
            run.error.as_deref().unwrap_or(&run.outcome),
        );
        if let Some(parts) = &run.parts {
            eprintln!("    parts pursued: {parts}");
        }
        if !run.success && run.error.is_none() {
            eprintln!("    steps: {:?}", run.steps_taken);
            eprintln!("    at {} — options: {:?}", run.final_url, run.options);
        }
        runs.push(run);
    }
    tabs.close_opened().await;
    drop(tabs);
    browser.close().await?;

    let summary = summarize(&runs);
    println!("{summary}");
    let path = super::report_path("navigator")?;
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({ "summary": summary, "runs": runs }))?,
    )?;
    println!("Report: {}", path.display());
    Ok(())
}

fn parse(args: &[String]) -> anyhow::Result<Options> {
    let mut options = Options {
        suite: PathBuf::from("evals/navigator.json"),
        sites: None,
        goals: None,
        profile: "eval".into(),
        headless: true,
        kb: dirs::home_dir().map(|home| home.join(".aibrowse/evals/kb.sqlite")),
        no_texts: false,
        no_lookup: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--suite" => options.suite = PathBuf::from(value()?),
            "--sites" => options.sites = Some(value()?),
            "--goals" => {
                options.goals = Some(value()?.split(',').map(str::to_owned).collect());
            }
            "--profile" => options.profile = value()?,
            "--headful" => options.headless = false,
            "--no-texts" => options.no_texts = true,
            "--no-lookup" => options.no_lookup = true,
            "--kb" => {
                let path = value()?;
                options.kb = (path != "off").then(|| PathBuf::from(path));
            }
            other => bail!("unknown option {other:?}\n{USAGE}"),
        }
    }
    Ok(options)
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Reached => "reached",
        Outcome::NeedsUser { .. } => "needs_user",
        Outcome::NeedsAgent { .. } => "needs_agent",
        Outcome::Stuck { .. } => "stuck",
        Outcome::Unsure { .. } => "unsure",
        Outcome::Consequential { .. } => "consequential",
        Outcome::OutOfBudget => "out_of_budget",
        Outcome::Looping { .. } => "looping",
        Outcome::DeadEnd => "dead_end",
        Outcome::GaveUp { .. } => "gave_up",
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Markdown as it reads: `[text](target)` is `text`, so that an answer spanning a link
/// ("Apache License, Version 2.0 or MIT") is found.
fn unlinked(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(at) = rest.find("](") {
        let Some(end) = rest[at..].find(')') else {
            break;
        };
        let label = &rest[..at];
        match label.rfind('[') {
            Some(open) => {
                out.push_str(&label[..open]);
                out.push_str(&label[open + 1..]);
            }
            None => out.push_str(label),
        }
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    out
}

fn matches_goal(url: &str, title: &str, expect: &[String]) -> bool {
    let haystack = format!("{url} {title}").to_ascii_lowercase();
    expect
        .iter()
        .any(|word| haystack.contains(&word.to_ascii_lowercase()))
}

/// Success rate, false positives and timing, against the A4 exit bar.
fn summarize(runs: &[Run]) -> String {
    // Runs that failed for infrastructure reasons (Jev down, a site that didn't load) say
    // nothing about the navigator, so they are reported apart and not scored.
    let infra = runs.iter().filter(|r| r.error.is_some()).count();
    let total = (runs.len() - infra).max(1);
    let successes = runs.iter().filter(|r| r.success).count();
    let false_positives = runs.iter().filter(|r| r.false_positive).count();
    let mut times: Vec<u64> = runs
        .iter()
        .filter(|r| r.success)
        .map(|r| r.elapsed_ms)
        .collect();
    times.sort_unstable();
    let median = times.get(times.len() / 2).copied().unwrap_or(0);
    let decisions: usize = runs.iter().map(|r| r.decisions).sum();
    let tokens = super::median(
        runs.iter()
            .filter(|r| r.error.is_none())
            .map(|r| r.returned_tokens),
    );
    let rate = successes as f64 / total as f64;
    let agent_turns: usize = runs.iter().map(|r| r.agent_turns).sum();
    let mut outcomes: BTreeMap<&str, usize> = BTreeMap::new();
    for run in runs {
        *outcomes
            .entry(if run.error.is_some() {
                "error"
            } else {
                run.outcome.as_str()
            })
            .or_default() += 1;
    }
    // Too many unscored runs and the result means nothing either way.
    let pass = infra * 4 <= runs.len()
        && rate >= 0.8
        && Duration::from_millis(median) < Duration::from_secs(5);
    format!(
        "Navigator eval: {successes}/{total} scored runs succeeded ({:.0}%), {false_positives} false \"reached\", median {:.1}s on successes, {:.1} decisions per run, {agent_turns} agent turns in all, ~{tokens} tokens returned per run (median); {infra} runs not scored (infrastructure errors).\nOutcomes: {outcomes:?}\nA4 exit (≥80%, median <5s): {}",
        rate * 100.0,
        median as f64 / 1000.0,
        decisions as f64 / total as f64,
        if pass { "met" } else { "not met" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_needs_the_right_page() {
        let expect = vec!["privacy".to_owned()];
        assert!(matches_goal("https://x.com/legal/privacy", "", &expect));
        assert!(matches_goal("https://x.com/p", "Privacy Notice", &expect));
        assert!(!matches_goal("https://x.com/terms", "Terms", &expect));
    }

    #[test]
    fn answers_are_found_across_links() {
        assert_eq!(
            unlinked(
                "Licensed under either of [Apache License, Version 2.0](https://a/LICENSE) or [MIT license](https://b) at your option."
            ),
            "Licensed under either of Apache License, Version 2.0 or MIT license at your option."
        );
        assert_eq!(unlinked("no links [here]"), "no links [here]");
    }
}
