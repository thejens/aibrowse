//! Surveying sites (docs/design.md §5.5, "Surveys"): the same goal pursued on each of
//! several sites side by side, as a person compares shops by opening each in a tab and
//! looking for the same thing. The agent names the sites (from its own knowledge or a
//! search); each gets a full navigator of its own in a background tab (its menus, its
//! search, fan-outs, the site lookup), and what each page says about the goal comes
//! back, so the agent compares facts rather than reading every page.
//!
//! The navigators think at once and act one at a time: the browser has one hand, which
//! brings a tab in front for its input and is free while pages load (`tab::Hand`).

use std::time::{Duration, Instant};

use aibrowse_browser::{Browser, Tab};
use aibrowse_input::Pace;
use aibrowse_perception::{Detail, observe, settled_main_text};
use futures_util::future::join_all;
use serde_json::{Map, Value, json};

use crate::candidates::candidates;
use crate::digest::passages_for;
use crate::jev::Jev;
use crate::pursue::{Limits, Outcome, Workspace, pursue};
use crate::words::{judged_search_words, numbers_in};

/// Sites surveyed at most in one call: each is a tab acting in turn with the others.
pub const MAX_SITES: usize = 10;
/// What each site's navigator may spend. Its own budget, as for `navigate(goal)`, but
/// shorter: the agent chose these sites expecting the answer on them.
const SITE_STEPS: usize = 8;
const SITE_TIME: Duration = Duration::from_secs(60);
/// What reading and judging a site's page may take once its navigator stopped, per
/// part (the filters' check, the controls offered, the read, the excerpt).
const FINISH_TIME: Duration = Duration::from_secs(20);

/// What narrowing a listing with its filters may spend: each criterion is often a
/// dialog to open, an option to choose and a button to apply, three steps.
const FILTER_STEPS: usize = 12;
const FILTER_TIME: Duration = Duration::from_secs(90);
/// The whole survey, a site lookup included: past this, a site's navigator is stopped
/// where it is and its page read as it stands.
const SURVEY_DEADLINE: Duration = Duration::from_secs(120);
/// How long a site's first page may take to load before its navigator looks anyway.
const LOAD_WAIT: Duration = Duration::from_secs(5);

/// What one site showed.
#[derive(Debug, Clone)]
pub struct Finding {
    /// The address the survey started from.
    pub site: String,
    /// Where it ended.
    pub url: String,
    pub title: String,
    pub reached: bool,
    /// Why it stopped, when it didn't reach the goal.
    pub why: String,
    /// What the last page says about the goal (empty when there was no page).
    pub excerpt: String,
    pub steps: Vec<String>,
    /// With filters asked for: whether the page's own filters were set to them, how,
    /// and why not when they weren't.
    pub filtered: Option<Filtering>,
    /// On a page reached: the controls that filter or sort its listing, as labelled in
    /// the outline (`button "Pris" collapsed`, `slider "Maximum price" (value: 7999)`),
    /// so the agent can say how to narrow it on its next call.
    pub filters_offered: Vec<String>,
    /// Read-only tools the last page offers agents (WebMCP), by name: a store's own
    /// catalog search answers in structured form (Shopify stores offer
    /// `search_catalog`, `get_product`), worth the agent's call where the page fell
    /// short.
    pub tools: Vec<String>,
}

/// How a listing's filters were set (the survey's second phase).
#[derive(Debug, Clone)]
pub struct Filtering {
    pub done: bool,
    /// How sure Jev was, looking at the page afterwards, that its listing is narrowed
    /// to the criteria by its filters.
    pub verdict: Option<f64>,
    pub why: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SurveyReport {
    pub findings: Vec<Finding>,
    pub decisions: usize,
    pub elapsed: Duration,
}

/// One site's tabs: the one the survey opened, plus any its clicks opened, the last in
/// front.
struct SiteTabs<'b> {
    browser: &'b Browser,
    tabs: Vec<Tab>,
    pace: Pace,
}

impl Workspace for SiteTabs<'_> {
    fn tab(&mut self) -> &mut Tab {
        self.tabs.last_mut().expect("a site has a tab")
    }

    fn browser(&self) -> Option<&Browser> {
        Some(self.browser)
    }

    async fn follow_new_tabs(&mut self) -> Vec<String> {
        let opener = self.tab().target_id().to_owned();
        let popups = self.browser.take_popups(&opener).await.unwrap_or_default();
        let notes = vec!["the page opened a new tab".to_owned(); popups.len()];
        for mut popup in popups {
            popup.set_pace(self.pace);
            self.tabs.push(popup);
        }
        notes
    }

    async fn back_to(&mut self, opener: &str) -> bool {
        if self.tabs.len() < 2 || self.tabs[self.tabs.len() - 2].target_id() != opener {
            return false;
        }
        let front = self.tabs.pop().expect("checked above");
        let _ = self.browser.close_tab(front.target_id()).await;
        true
    }

    async fn adopt(&mut self, tab: Tab) -> Vec<String> {
        self.tabs.push(tab);
        Vec::new()
    }
}

/// What a survey asks of each site.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    /// What to find on each site: a page, a listing, a price.
    pub goal: &'a str,
    /// What the navigators may type into a site's own search; without them, the goal's
    /// own words.
    pub texts: &'a [String],
    /// Criteria to narrow a listing to with the page's own filters, in plain words
    /// ("under 5000 kr, hydraulic disc brakes"): a second phase on the page reached.
    pub filters: Option<&'a str>,
    /// What to quote from each page (the goal, and the filters, when `None`).
    pub focus: Option<&'a str>,
    pub excerpt_tokens: usize,
}

/// Pursues the ask's goal on each of `sites` side by side and returns, per site in the
/// order given, where it ended and what that page says about the focus. With filters,
/// a listing reached is then narrowed with its own filters, as a person shopping does:
/// first the category, then the filter section. Every tab it opened is closed and the
/// tab in front is in front again.
pub async fn survey<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    sites: &[String],
    ask: Ask<'_>,
) -> SurveyReport {
    let Ask {
        goal,
        texts,
        filters,
        focus,
        excerpt_tokens,
    } = ask;
    let started = Instant::now();
    let sites: Vec<&String> = sites.iter().take(MAX_SITES).collect();
    let pace = workspace.tab().pace();
    let home = workspace.tab().target_id().to_owned();
    let Some(browser) = workspace.browser() else {
        return SurveyReport {
            findings: sites
                .iter()
                .map(|site| unopened(site, "no browser to open tabs in"))
                .collect(),
            decisions: 0,
            elapsed: started.elapsed(),
        };
    };
    let own_words;
    let texts = if texts.is_empty() {
        own_words = judged_search_words(jev, goal, "").await;
        &own_words[..]
    } else {
        texts
    };
    let limits = Limits {
        max_steps: SITE_STEPS,
        max_time: SITE_TIME,
        agreements: workspace.agreements(),
        ..Limits::default()
    };
    let focus_words;
    let focus = match (focus, filters) {
        (Some(focus), _) => focus,
        (None, Some(filters)) => {
            focus_words = format!("{goal}, {filters}");
            &focus_words
        }
        (None, None) => goal,
    };
    // The second phase works on the page the first reached: no lookup across the site,
    // and the criteria's numbers (a price limit) as what it may set fields and sliders
    // to.
    let filter_goal = filters.map(|filters| {
        format!(
            "{goal}, narrowed with this page's own filters (and sorting, if asked) to: {filters}"
        )
    });
    let filter_texts: Vec<String> = filters
        .map(|filters| {
            numbers_in(filters)
                .into_iter()
                .chain(texts.iter().cloned())
                .collect()
        })
        .unwrap_or_default();
    let filter_limits = Limits {
        max_steps: FILTER_STEPS,
        max_time: FILTER_TIME,
        lookup: false,
        agreements: workspace.agreements(),
        ..Limits::default()
    };
    let (filter_goal, filter_texts, filter_limits) =
        (filter_goal.as_deref(), &filter_texts[..], &filter_limits);

    let mut findings: Vec<Finding> = Vec::new();
    let mut opened: Vec<(&String, SiteTabs)> = Vec::new();
    for site in &sites {
        match browser.new_background_tab(site).await {
            Ok(mut tab) => {
                tab.set_pace(pace);
                opened.push((
                    site,
                    SiteTabs {
                        browser,
                        tabs: vec![tab],
                        pace,
                    },
                ));
            }
            Err(err) => findings.push(unopened(site, &format!("could not open: {err}"))),
        }
    }

    let limits = &limits;
    let runs = join_all(opened.iter_mut().map(|(_, tabs)| async move {
        tabs.tab().wait_settled(LOAD_WAIT).await;
        let mut filtering = None;
        let mut found = None;
        // Past the deadline, what the site's navigator found still counts: only the
        // filtering after it is cut short (its own budget usually ends it first).
        let deadline = if filter_goal.is_some() {
            SURVEY_DEADLINE + FILTER_TIME
        } else {
            SURVEY_DEADLINE
        };
        let _ = tokio::time::timeout(deadline, async {
            let report = pursue(tabs, jev, goal, texts, limits).await;
            let reached = report
                .as_ref()
                .is_ok_and(|r| matches!(r.outcome, Outcome::Reached));
            found = Some(report);
            if let Some(filter_goal) = filter_goal
                && reached
            {
                filtering = Some(pursue(tabs, jev, filter_goal, filter_texts, filter_limits).await);
            }
        })
        .await;
        (found.ok_or(()), filtering)
    }))
    .await;

    // Each site's page is read and judged side by side too: one by one, a survey of ten
    // shops ran minutes past its deadline after the navigators had stopped.
    let finished = join_all(opened.into_iter().zip(runs).map(
        |((site, mut tabs), (run, filtering))| async move {
            let mut decisions = 0;
            // Reached, or stopped for time somewhere: the page may say what was looked for.
            // One the navigator found leads nowhere (a home page with nothing about it) is
            // only noise to quote.
            let (reached, unfinished, why, steps) = match run {
                Ok(Ok(report)) => {
                    decisions += report.decisions;
                    (
                        matches!(report.outcome, Outcome::Reached),
                        matches!(report.outcome, Outcome::OutOfBudget),
                        report.outcome.why(),
                        report.steps.into_iter().map(|s| s.action).collect(),
                    )
                }
                Ok(Err(err)) => (false, false, format!("could not decide: {err}"), Vec::new()),
                Err(_) => (false, true, "out of time".to_owned(), Vec::new()),
            };
            let mut filtered = filtering.map(|run| match run {
                Ok(report) => {
                    decisions += report.decisions;
                    let done = matches!(report.outcome, Outcome::Reached);
                    Filtering {
                        done,
                        verdict: None,
                        why: if done {
                            String::new()
                        } else {
                            report.outcome.why()
                        },
                        steps: report.steps.into_iter().map(|s| s.action).collect(),
                    }
                }
                Err(err) => Filtering {
                    done: false,
                    verdict: None,
                    why: format!("could not decide: {err}"),
                    steps: Vec::new(),
                },
            });
            // Whether the filters are set is judged from the page as it ended up, whatever
            // the navigator's outcome: it decides by what a page says, and a listing's text
            // says nothing of its active filters, so it stopped "unsure" on listings already
            // narrowed (`f.price.max=5000`).
            if let (Some(filtering), Some(filters)) = (filtered.as_mut(), filters) {
                let verdict = tokio::time::timeout(FINISH_TIME, filters_match(tabs.tab(), jev, filters))
                    .await
                    .ok()
                    .flatten();
                filtering.verdict = verdict;
                if verdict.is_some_and(|v| v >= FILTERS_MATCH) {
                    filtering.done = true;
                    filtering.why.clear();
                }
            }
            let tab = tabs.tab();
            let tools = tab
                .webmcp_tools()
                .into_iter()
                .filter(|t| t.read_only)
                .map(|t| t.name)
                .collect();
            let title = tab.title().await;
            let url = tab.url();
            let moved = url.trim_end_matches('/') != site.trim_end_matches('/');
            // Each with its own bound: a heavy page's read hung past the deadline, and
            // the survey took 400 s.
            let filters_offered = if reached {
                tokio::time::timeout(FINISH_TIME, filter_controls(tab, jev))
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let excerpt = if reached || (unfinished && moved) {
                let read = tokio::time::timeout(FINISH_TIME, settled_main_text(tab)).await;
                let text = match read {
                    Ok(Ok(text)) => text,
                    Ok(Err(e)) => {
                        tracing::warn!(url = %tab.url(), error = %e, "read failed: page taken as empty");
                        String::new()
                    }
                    Err(_) => {
                        tracing::warn!(url = %tab.url(), "read timed out: page taken as empty");
                        String::new()
                    }
                };
                tokio::time::timeout(
                    FINISH_TIME,
                    passages_for(&text, Some(jev), focus, &title, excerpt_tokens),
                )
                .await
                .unwrap_or_default()
            } else {
                String::new()
            };
            for tab in &tabs.tabs {
                let _ = browser.close_tab(tab.target_id()).await;
            }
            (
                Finding {
                    site: site.clone(),
                    url,
                    title,
                    reached,
                    why: if reached { String::new() } else { why },
                    excerpt,
                    steps,
                    filtered,
                    filters_offered,
                    tools,
                },
                decisions,
            )
        },
    ))
    .await;
    let mut decisions = 0;
    for (finding, spent) in finished {
        decisions += spent;
        findings.push(finding);
    }
    let _ = browser.activate_tab(&home).await;
    findings.sort_by_key(|f| sites.iter().position(|s| **s == f.site));
    SurveyReport {
        findings,
        decisions,
        elapsed: started.elapsed(),
    }
}

/// A site whose tab never opened.
fn unopened(site: &str, why: &str) -> Finding {
    Finding {
        site: site.to_owned(),
        url: String::new(),
        title: String::new(),
        reached: false,
        why: why.to_owned(),
        excerpt: String::new(),
        steps: Vec::new(),
        filtered: None,
        filters_offered: Vec::new(),
        tools: Vec::new(),
    }
}

/// How sure Jev must be that a listing is narrowed to the criteria.
const FILTERS_MATCH: f64 = 0.6;
/// Characters of the outline Jev reads to judge that.
const VERDICT_OUTLINE_CHARS: usize = 8000;

/// How likely the listing on the page in `tab` is narrowed to `criteria` by its own
/// filters, judged from its outline (active filter chips, checked options, slider
/// values, a count of results) and its address (`?price=from_169_to_1500`).
async fn filters_match(tab: &mut Tab, jev: &Jev, criteria: &str) -> Option<f64> {
    let model = observe(tab, Detail::All, None).await.ok()?;
    let outline: String = model.render().chars().take(VERDICT_OUTLINE_CHARS).collect();
    let state = json!({ "criteria": criteria, "url": model.url, "page": outline });
    let questions = json!({
        "narrowed": {
            "type": "noul",
            "instructions": "Is the listing on this page narrowed by its own filters to `criteria`? Judge by what shows as applied (filter chips, checked options, slider or field values, the count of results) and by the address.",
            "criteria": {
                "true": "Yes: filters matching `criteria` are applied (a price limit at or near the one asked for counts)",
                "false": "No: the filters for `criteria` are not applied, or others are"
            }
        }
    });
    let answers = jev.ask(&state, &questions).await.ok()?;
    answers["narrowed"]["noul"].as_f64()
}

/// Controls a listing is filtered or sorted with, at most.
const MAX_FILTERS: usize = 15;
/// Controls judged, at most: those a page lists first are its own (facets sit above
/// or beside the results).
const MAX_JUDGED_CONTROLS: usize = 50;
/// How sure Jev must be that a control filters or sorts the listing.
const FILTER_MIN: f64 = 0.6;

/// The controls on the page in `tab` that filter or sort its listing, judged by Jev in
/// one request (in any language: "Pris", "Varumärke", "Sortera efter"). Links are left
/// out: a listing has many, and its facets are buttons, boxes, fields and sliders.
async fn filter_controls(tab: &mut Tab, jev: &Jev) -> Vec<String> {
    let Ok(model) = observe(tab, Detail::All, None).await else {
        return Vec::new();
    };
    let controls: Vec<String> = candidates(&model)
        .into_iter()
        .filter(|c| !c.is_link)
        .take(MAX_JUDGED_CONTROLS)
        .map(|c| c.label)
        .collect();
    if controls.is_empty() {
        return Vec::new();
    }
    let mut questions = Map::new();
    for (i, label) in controls.iter().enumerate() {
        questions.insert(
            format!("c{i}"),
            json!({
                "type": "noul",
                "instructions": format!("Is {label} a control that filters or sorts the products or results listed on this page?"),
                "criteria": {
                    "true": "Yes: a facet (price, brand, size, colour, rating...), a filter panel's toggle or apply button, or the sort order",
                    "false": "No: navigation, search, account, cart, cookies, or a product of its own"
                }
            }),
        );
    }
    let state = json!({ "page": model.title, "url": model.url });
    let Ok(answers) = jev.ask(&state, &Value::Object(questions)).await else {
        return Vec::new();
    };
    controls
        .into_iter()
        .enumerate()
        .filter(|(i, _)| answers[format!("c{i}")]["noul"].as_f64().unwrap_or(0.0) >= FILTER_MIN)
        .map(|(_, label)| label)
        .take(MAX_FILTERS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_come_out_of_criteria() {
        assert_eq!(numbers_in("under 5 000 kr, 28 inch wheels"), ["5000", "28"]);
        assert_eq!(numbers_in("max 4.5 kg."), ["4.5"]);
        assert!(numbers_in("hydraulic disc brakes").is_empty());
    }
}
