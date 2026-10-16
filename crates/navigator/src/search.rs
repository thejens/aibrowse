//! `search(query, criterion)` (docs/design.md §5.5): a web search that opens the results
//! side by side and returns the pages that fulfil a criterion: fan-out in "collect all"
//! mode over a results page.

use std::time::Duration;

use aibrowse_browser::Tab;
use aibrowse_perception::main_text;
use futures_util::future::join_all;

use crate::digest::passages_for;
use crate::fanout::{Branch, Collect, Found, fan_out};
use crate::jev::Jev;
use crate::pursue::{Limits, Outcome, Workspace};
use crate::pursuit::Link;

mod results;
mod verdict;

pub(crate) use results::results_page;
use results::{link_target, result_name};
use verdict::{Page, Verdict};

/// Results opened at most, whatever the caller asks: past this, a second search with a
/// sharper query beats reading further down.
pub const MAX_PAGES: usize = 10;
/// Text returned per page that fulfils the criterion; `read` gets the rest.
const EXCERPT_TOKENS: usize = 200;
/// Steps a result may take to show it fulfils the criterion: the result itself is
/// usually the page, sometimes it is one click away (a listing, a "read more"). A result
/// that needs more is a different search.
const RESULT_STEPS: usize = 2;
const RESULT_TIME: Duration = Duration::from_secs(45);
/// The results the search judges by their text however their branches ended: the
/// engine's likeliest, where a long canonical page is often the answer its branch's
/// check read too little of to tell.
const TOP_RESULTS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Google,
    Bing,
    DuckDuckGo,
    Brave,
}

/// How long an engine that answered with a challenge ("I'm not a robot") is left alone:
/// asking again soon only meets it again, and the next engine answers meanwhile.
const CHALLENGE_REST: Duration = Duration::from_secs(30 * 60);

/// When each engine last answered with a challenge, for this process.
static CHALLENGED: std::sync::Mutex<Vec<(Engine, std::time::Instant)>> =
    std::sync::Mutex::new(Vec::new());

/// The engines asked in turn when the first is resting or refuses: Google's index is
/// the broadest, and DuckDuckGo (Bing's index) and Brave (its own) seldom challenge.
pub(crate) const FALLBACKS: [Engine; 3] = [Engine::Google, Engine::DuckDuckGo, Engine::Brave];

impl Engine {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "google" => Some(Self::Google),
            "bing" => Some(Self::Bing),
            "duckduckgo" | "ddg" => Some(Self::DuckDuckGo),
            "brave" => Some(Self::Brave),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Bing => "Bing",
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Brave => "Brave",
        }
    }

    /// Whether the engine answered with a challenge lately (see `CHALLENGE_REST`).
    pub(crate) fn resting(self) -> bool {
        let challenged = CHALLENGED.lock().unwrap_or_else(|p| p.into_inner());
        challenged
            .iter()
            .any(|(engine, at)| *engine == self && at.elapsed() < CHALLENGE_REST)
    }

    fn challenged(self) {
        let mut challenged = CHALLENGED.lock().unwrap_or_else(|p| p.into_inner());
        challenged.retain(|(engine, _)| *engine != self);
        challenged.push((self, std::time::Instant::now()));
    }

    /// The results page for `query`, as typing it into the address bar would load it.
    pub fn results_url(self, query: &str) -> String {
        let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        match self {
            Self::Google => format!("https://www.google.com/search?q={q}"),
            Self::Bing => format!("https://www.bing.com/search?q={q}"),
            Self::DuckDuckGo => format!("https://duckduckgo.com/?q={q}"),
            Self::Brave => format!("https://search.brave.com/search?q={q}"),
        }
    }

    /// Whether `host` is a search engine's own site (not merely a domain the company
    /// owns, as Microsoft owns Bing): a goal pursued there is no site to search.
    pub(crate) fn serves(host: &str) -> bool {
        let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
        host.split('.').any(|part| part == "google")
            || under("bing.com")
            || under("duckduckgo.com")
            || under("search.brave.com")
    }

    /// Whether `host` belongs to the engine itself (its own pages, ads, images, maps),
    /// rather than being a result.
    fn owns(self, host: &str) -> bool {
        let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
        let label = |name: &str| host.split('.').any(|part| part == name);
        match self {
            Self::Google => {
                label("google")
                    || [
                        "googleusercontent.com",
                        "gstatic.com",
                        "googleadservices.com",
                    ]
                    .iter()
                    .any(|d| under(d))
            }
            Self::Bing => ["bing.com", "microsoft.com", "msn.com"]
                .iter()
                .any(|d| under(d)),
            Self::DuckDuckGo => under("duckduckgo.com"),
            Self::Brave => under("brave.com"),
        }
    }
}

/// A result page that fulfils the criterion.
#[derive(Debug, Clone)]
pub struct Hit {
    pub title: String,
    pub url: String,
    /// What its main text says about the criterion.
    pub excerpt: String,
    /// The name of the result it was reached from, when the page itself is one click
    /// further.
    pub via: Option<String>,
    /// The names of other results that led to the same page.
    pub also_from: Vec<String>,
    /// Counted by its text alone: its branch stopped short of its own verdict.
    pub by_text: bool,
}

/// A result that doesn't, and why.
#[derive(Debug, Clone)]
pub struct Miss {
    /// The result's name on the results page.
    pub label: String,
    pub url: String,
    pub why: String,
    /// Whether it was judged; one that ran out of steps or time (`why` says which) may
    /// still fulfil the criterion.
    pub settled: bool,
    /// For one that wasn't: what its last page says about the criterion, for the
    /// caller to judge.
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub struct SearchReport {
    /// The engine that answered: the one asked for, or DuckDuckGo when Google showed a
    /// challenge.
    pub engine: Engine,
    pub results_url: String,
    /// Results found on the results page, before `max_pages` was applied.
    pub results: usize,
    pub hits: Vec<Hit>,
    pub misses: Vec<Miss>,
    pub decisions: usize,
    pub elapsed: Duration,
}

/// Searches `query` with `engine` in the workspace's tab in front, opens the first
/// `max_pages` results side by side and returns those that fulfil `criterion`. The tab
/// in front is left on the results page; every tab the search opened is closed.
pub async fn search<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    query: &str,
    criterion: &str,
    engine: Engine,
    max_pages: usize,
) -> Result<SearchReport, String> {
    let started = std::time::Instant::now();
    // Google often answers a browser it has seen search a lot with a challenge instead
    // of results ("I'm not a robot"); DuckDuckGo then answers instead, as in the site
    // lookup, and the report says which did.
    let mut first_error = None;
    let mut answered = None;
    for candidate in std::iter::once(engine).chain(FALLBACKS.into_iter().filter(|e| *e != engine)) {
        match results_page(workspace, jev, candidate, query, criterion, None).await {
            Ok(found) => {
                answered = Some((candidate, found));
                break;
            }
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    let Some((engine, found)) = answered else {
        return Err(first_error.unwrap_or_default());
    };
    let results_url = engine.results_url(query);
    let results = found.len();
    let links: Vec<Link> = found
        .into_iter()
        .take(max_pages.clamp(1, MAX_PAGES))
        .collect();
    let goal = format!("a page that fulfils this: {criterion}");
    let limits = Limits {
        max_steps: RESULT_STEPS,
        max_time: RESULT_TIME,
        // A result that isn't clearly the page or one click from it doesn't qualify.
        guesses: 0,
        agreements: workspace.agreements(),
        ..Limits::default()
    };
    let fanned = fan_out(workspace, jev, &goal, &[], &links, Collect::All, &limits).await;
    let mut decisions = fanned.decisions;
    let rank = |branch: &Branch| {
        links
            .iter()
            .position(|l| l.identity == branch.link.identity)
            .unwrap_or(links.len())
    };

    // The pages to judge, in the engine's order as ranked: those reached, each page
    // once, and those whose branches stopped short in time to be read. Two results may
    // lead to the same page (a docs site and its crate page), or to one page under two
    // addresses that differ only in tracking parameters (`?srsltid=…` from a result,
    // `?gclid=…` from an ad): the same path and title.
    let mut opened: Vec<Opened> = Vec::new();
    let mut misses: Vec<Miss> = Vec::new();
    let mut to_close: Vec<Tab> = Vec::new();
    let reached = fanned.reached.into_iter().map(|found| (found, true));
    let unfinished = fanned.unfinished.into_iter().map(|found| (found, false));
    for (found, reached) in reached.chain(unfinished) {
        let branch = &found.branch;
        // Stopped short with steps or time to spare (unsure, stuck), a result's own
        // check has spoken; the top results, the likeliest, get the search's verdict
        // anyway, as a long page's check reads only part of it.
        let judged = !matches!(branch.outcome, Outcome::OutOfBudget);
        if !reached && judged && rank(branch) >= TOP_RESULTS {
            misses.push(miss(branch, true, String::new()));
            to_close.push(found.tab);
            continue;
        }
        match opened
            .iter_mut()
            .find(|o| same_page(&o.branch.url, &o.branch.title, &branch.url, &branch.title))
        {
            Some(other) => {
                other.also_from.push(result_name(&branch.link.label));
                to_close.push(found.tab);
            }
            None => opened.push(Opened::new(found, reached)),
        }
    }
    opened.sort_by_key(|o| rank(&o.branch));
    // What each page says about the criterion, not its opening (a shop's price for a
    // service sits under its pitch): read side by side, each in its own tab.
    join_all(opened.iter_mut().map(|o| o.read(jev, criterion))).await;
    let pages: Vec<Page> = opened
        .iter()
        .map(|o| Page {
            title: &o.branch.title,
            url: &o.branch.url,
            excerpt: &o.excerpt,
        })
        .collect();
    let verdicts = verdict::judge(jev, criterion, &pages).await;
    decisions += usize::from(verdicts.is_some());
    let mut verdicts = verdicts.map(Vec::into_iter);

    let mut hits: Vec<Hit> = Vec::new();
    for o in opened {
        let verdict = verdicts.as_mut().and_then(Iterator::next);
        to_close.push(o.tab);
        let branch = o.branch;
        tracing::debug!(url = %branch.url, reached = o.reached, ?verdict, "search verdict");
        match verdict {
            Some(v) if v.is_from_elsewhere() => misses.push(Miss {
                why: "not from the source the criterion names".to_owned(),
                ..miss(&branch, true, String::new())
            }),
            _ if o.reached || verdict.is_some_and(Verdict::fulfils) => hits.push(Hit {
                via: (!branch.steps.is_empty()).then(|| result_name(&branch.link.label)),
                title: branch.title,
                url: branch.url,
                excerpt: o.excerpt,
                also_from: o.also_from,
                by_text: !o.reached,
            }),
            // Its own check said no with steps to spare, and the text doesn't say yes.
            _ if !matches!(branch.outcome, Outcome::OutOfBudget) => {
                misses.push(miss(&branch, true, String::new()));
            }
            // The excerpt says whether it fits, at the cost of its tokens rather than
            // more of the search's time.
            _ => misses.push(miss(&branch, false, o.excerpt)),
        }
    }
    // A result that wants a sign-in or typed text isn't a page that fulfils it.
    for found in fanned.waiting {
        misses.push(miss(&found.branch, true, String::new()));
        to_close.push(found.tab);
    }
    misses.extend(fanned.gave_up.iter().map(|b| miss(b, true, String::new())));
    if let Some(browser) = workspace.browser() {
        for tab in to_close {
            let _ = browser.close_tab(tab.target_id()).await;
        }
    }
    Ok(SearchReport {
        engine,
        results_url,
        results,
        hits,
        misses,
        decisions,
        elapsed: started.elapsed(),
    })
}

/// A page a result led to, open in its tab until the search has judged it.
struct Opened {
    tab: Tab,
    branch: Branch,
    /// Whether its branch's own check found it fulfils the criterion.
    reached: bool,
    also_from: Vec<String>,
    excerpt: String,
}

impl Opened {
    fn new(found: Found, reached: bool) -> Self {
        Self {
            tab: found.tab,
            branch: found.branch,
            reached,
            also_from: Vec::new(),
            excerpt: String::new(),
        }
    }

    async fn read(&mut self, jev: &Jev, criterion: &str) {
        let text = main_text(&mut self.tab).await.unwrap_or_else(|e| {
            tracing::warn!(url = %self.tab.url(), error = %e, "read failed: page taken as empty");
            String::new()
        });
        self.excerpt = passages_for(
            &text,
            Some(jev),
            criterion,
            &self.branch.title,
            EXCERPT_TOKENS,
        )
        .await;
    }
}

/// A result that didn't fulfil the criterion, or wasn't settled (`settled` false), as
/// the report tells it.
fn miss(branch: &Branch, settled: bool, excerpt: String) -> Miss {
    Miss {
        label: result_name(&branch.link.label),
        url: if branch.url.is_empty() {
            link_target(&branch.link.label)
        } else {
            branch.url.clone()
        },
        why: match branch.outcome {
            Outcome::OutOfBudget => cut_short(branch.elapsed),
            ref outcome => outcome.why(),
        },
        settled,
        excerpt,
    }
}

/// Which budget a result that ran out used up: its steps, or its time. A result out of
/// steps after 21s isn't "out of time", and the caller may well open it.
fn cut_short(elapsed: Duration) -> String {
    if elapsed >= RESULT_TIME {
        format!("ran out of its {}s", RESULT_TIME.as_secs())
    } else {
        format!(
            "ran out of its {RESULT_STEPS} steps after {}s",
            elapsed.as_secs()
        )
    }
}

/// Whether `url` with `title` is the page at `seen_url` with `seen_title`: the same
/// address, or the same path and title under another query string.
fn same_page(seen_url: &str, seen_title: &str, url: &str, title: &str) -> bool {
    let path = |u: &str| {
        url::Url::parse(u)
            .ok()
            .map(|u| (u.host_str().map(str::to_owned), u.path().to_owned()))
    };
    seen_url == url
        || (!title.is_empty()
            && seen_title == title
            && path(seen_url).is_some()
            && path(seen_url) == path(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_own_their_pages_but_not_results() {
        assert!(Engine::Google.owns("www.google.com"));
        assert!(Engine::Google.owns("support.google.co.uk"));
        assert!(Engine::Google.owns("lh3.googleusercontent.com"));
        assert!(!Engine::Google.owns("www.rust-lang.org"));
        assert!(!Engine::Google.owns("googlefan.example.com"));
        assert!(Engine::Bing.owns("www.bing.com"));
        assert!(!Engine::DuckDuckGo.owns("duck.com.example"));
    }

    #[test]
    fn queries_are_encoded_like_the_address_bar_does() {
        assert_eq!(
            Engine::Google.results_url("foo bar & baz"),
            "https://www.google.com/search?q=foo+bar+%26+baz"
        );
    }

    #[test]
    fn one_page_under_two_tracking_addresses_is_one_hit() {
        let seen = "https://skidshop.se/pages/bootfitting?srsltid=AU7";
        let same = |url: &str, title: &str| same_page(seen, "Bootfitting", url, title);
        assert!(same(seen, ""));
        assert!(same(
            "https://skidshop.se/pages/bootfitting?gclid=EAI",
            "Bootfitting"
        ));
        assert!(!same(
            "https://skidshop.se/pages/bootfitting?gclid=EAI",
            "Other"
        ));
        assert!(!same("https://skidshop.se/pages/priser", "Bootfitting"));
    }

    #[test]
    fn running_out_of_steps_is_told_apart_from_running_out_of_time() {
        assert_eq!(
            cut_short(Duration::from_secs(21)),
            "ran out of its 2 steps after 21s"
        );
        assert_eq!(cut_short(RESULT_TIME), "ran out of its 45s");
    }
}
