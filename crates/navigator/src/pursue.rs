//! Pursuing a goal (docs/design.md §5.5): what the navigator reports, where it works,
//! and the loop that thinks and acts in the tab in front (pursuit.rs), fanning out to
//! several tabs when no option clearly leads (fanout.rs).

use std::time::Duration;

use aibrowse_browser::{Browser, ElementRef, Tab};
use aibrowse_perception::{Detail, observe};
use serde::Serialize;

use crate::candidates::page_settings;
use crate::fanout::{Collect, fan_out};
use crate::jev::{Jev, JevError};
use crate::judge::Agreements;
use crate::pursuit::{Next, Pursuit};
use crate::search::Engine;
use crate::site_search::site_search;
use crate::words::{judged_search_words, numbers_in};

/// Fan-outs per pursuit: past two rounds of "open the likely ones and look", what is
/// left is unlikely, and the agent should hear what was tried.
const MAX_FAN_OUTS: usize = 2;
/// A branch explores, briefly: its link was only a guess.
const BRANCH_STEPS: usize = 4;

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_steps: usize,
    pub max_time: Duration,
    /// Below this confidence in the next action, hand back instead of guessing.
    pub min_confidence: f64,
    /// Steps it may take without a convincing option (each undone by going back if it
    /// leads nowhere): past a couple, the agent should look. A fan-out's branch takes
    /// none, its link being a guess already; a survey's sites take some, each being the
    /// only way to its site.
    pub guesses: usize,
    /// Whether it may look the goal up across the site (site_search.rs) when the site's
    /// own navigation stops short: not when the goal is on this very page, such as a
    /// listing's own filters, which a lookup would only leave.
    pub lookup: bool,
    /// Keys or chords the agent offered for this goal ("Meta+d" to fill a selection
    /// down in a spreadsheet), pressed where Jev finds one fits. Like `texts`, they are
    /// the agent's knowledge of the app; which one and when is decided on the page.
    pub keys: Vec<String>,
    /// Whether accepting terms or cookies to go on is taken like any step (the
    /// profile's policy, judge.rs).
    pub agreements: Agreements,
    /// When the goal is one part of several (parts.rs): the parts before and after it.
    /// Jev sees them, so it knows where this part ends: "select C3 to C10" is done once
    /// the cells are selected, and filling them is the next part's, not this one's.
    pub part_of: Option<PartOf>,
}

#[derive(Debug, Clone, Default)]
pub struct PartOf {
    pub before: Vec<String>,
    pub after: Vec<String>,
    /// The place the part before arrived from at its last page, where this part starts:
    /// a link back there undoes what that part did (eBay's "Buy It Now" toggle, offered
    /// again in the next part and clicked at 0.96).
    pub came_from: Option<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 12,
            max_time: Duration::from_secs(60),
            min_confidence: 0.5,
            guesses: 2,
            lookup: true,
            keys: Vec::new(),
            part_of: None,
            agreements: Agreements::default(),
        }
    }
}

impl Limits {
    /// What one branch of a fan-out may spend, given what the pursuit has used.
    pub fn for_branch(&self, used: Duration) -> Self {
        Self {
            max_steps: BRANCH_STEPS.min(self.max_steps),
            max_time: self.max_time.saturating_sub(used),
            min_confidence: self.min_confidence,
            guesses: 0,
            lookup: self.lookup,
            // A branch only follows its link to see where it leads.
            keys: Vec::new(),
            part_of: self.part_of.clone(),
            agreements: self.agreements,
        }
    }
}

/// Why the navigator stopped, and so who acts next.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The current page satisfies the goal.
    Reached,
    /// Only the user can proceed (login, 2FA, CAPTCHA, payment): hand off to them.
    NeedsUser { reason: String },
    /// Text must be typed or a judgment made: the calling agent's turn.
    NeedsAgent { field: Option<(ElementRef, String)> },
    /// Nothing on the page seems to help; the most plausible options, for the agent.
    Stuck { options: Vec<(String, f64)> },
    /// Several options looked equally good; the agent should choose.
    Unsure { options: Vec<(String, f64)> },
    /// The best next step would commit something on the user's behalf (send, accept,
    /// buy...). The navigator never does that itself; the agent decides, and the user
    /// approves.
    Consequential { element: ElementRef, label: String },
    /// The step or time budget ran out.
    OutOfBudget,
    /// The latest steps repeat; doing them again won't get anywhere.
    Looping { steps: Vec<String> },
    /// A branch of a fan-out went back from where it started: its link led nowhere
    /// (fanout.rs).
    DeadEnd,
    /// Links were explored side by side and none led to the goal, and nothing else on
    /// the page does: each link with why its branch gave up.
    GaveUp { tried: Vec<(String, String)> },
}

impl Outcome {
    /// Why a pursuit that ended this way didn't reach the goal, in a few words.
    pub fn why(&self) -> String {
        match self {
            Self::Reached => "reached the goal".into(),
            Self::NeedsUser { reason } => format!("needs the user ({reason})"),
            Self::NeedsAgent { .. } => "needs text typed".into(),
            Self::Stuck { .. } => "nothing there helps".into(),
            Self::Unsure { .. } => "no clear way on".into(),
            Self::Consequential { label, .. } => format!("next step would commit: {label}"),
            Self::OutOfBudget => "out of steps or time".into(),
            Self::Looping { .. } => "went in circles".into(),
            Self::DeadEnd => "a dead end".into(),
            Self::GaveUp { .. } => "nothing led there".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    pub action: String,
    pub confidence: f64,
    pub url: String,
    pub error: Option<String>,
}

/// One ranking Jev made on a page: whose turn it judged it to be, and how it ranked the
/// options it was given. Kept for traces, so a run can be audited and replayed as eval
/// data.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub url: String,
    /// Options offered on this page (links, buttons, fields, scroll).
    pub candidates: usize,
    /// "reached", "navigate", "user", "agent", "stuck" with their probabilities.
    pub turn: Vec<(String, f64)>,
    /// The best-ranked next actions, labelled, with their probabilities.
    pub next: Vec<(String, f64)>,
    /// When Jev judged the goal reached: how sure the check against the page's text
    /// was that it is the goal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_check: Option<f64>,
    /// When text was to be typed: the field, the suggested text Jev picked for it
    /// ("none" when none fits), and how sure it was. Below the bar, nothing is typed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<(String, String, f64)>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub outcome: Outcome,
    pub steps: Vec<Step>,
    /// What changed about tabs along the way ("the page opened a new tab..."), from the
    /// workspace, for the agent.
    pub tab_notes: Vec<String>,
    pub decisions: usize,
    /// With a reached goal: what the page is set to (`candidates::page_settings`), for
    /// the agent to check against the goal.
    pub settings: Vec<String>,
    /// The place the pursuit arrived at its last page from (for the next part).
    pub came_from: Option<String>,
    /// Each page's ranking, in order.
    pub trail: Vec<Decision>,
    pub elapsed: Duration,
}

/// Where the navigator works: the tab in front, which changes when a click opens a new
/// tab (as a person would see it come to the front).
pub trait Workspace {
    fn tab(&mut self) -> &mut Tab;

    /// Brings tabs the page opened since the last call to the front; returns notes
    /// describing what changed (empty when nothing did).
    fn follow_new_tabs(&mut self) -> impl std::future::Future<Output = Vec<String>> + Send;

    /// The browser, for working in several tabs at once (fanout.rs). Without one, the
    /// navigator stays in the tab in front.
    fn browser(&self) -> Option<&Browser> {
        None
    }

    /// Goes back from the tab in front to `opener`, the tab it was opened from: closes
    /// the one in front and brings `opener` to the front. False when it can't.
    fn back_to(&mut self, _opener: &str) -> impl std::future::Future<Output = bool> + Send {
        async { false }
    }

    /// Makes `tab` (a branch that reached the goal) the tab in front, for good; returns
    /// notes for the agent.
    fn adopt(&mut self, tab: Tab) -> impl std::future::Future<Output = Vec<String>> + Send {
        async move {
            drop(tab);
            Vec::new()
        }
    }

    /// The steps (element identities) that reached `goal` on `site` before, if known.
    fn recall_path(
        &mut self,
        _site: &str,
        _goal: &str,
    ) -> impl std::future::Future<Output = Option<Vec<String>>> + Send {
        async { None }
    }

    /// The pages the sitemaps of the site at `url` list, reading first the sitemaps whose
    /// names share most of `words`. Empty when there are none, or none can be read.
    fn site_pages(
        &mut self,
        _url: &str,
        _words: &[String],
    ) -> impl std::future::Future<Output = Vec<String>> + Send {
        async { Vec::new() }
    }

    /// Whether accepting terms or cookies to go on is taken unasked (the profile's
    /// policy): for the navigators a survey, a search or a lookup starts.
    fn agreements(&self) -> Agreements {
        Agreements::default()
    }

    /// Forgets the steps learned for `goal` on `site` (they didn't reach it again).
    fn forget_path(
        &mut self,
        _site: &str,
        _goal: &str,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    /// Remembers the steps that reached `goal` on `site`.
    fn learn_path(
        &mut self,
        _site: &str,
        _goal: &str,
        _steps: &[String],
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }
}

/// A lone tab: links that open new tabs aren't followed.
impl Workspace for Tab {
    fn tab(&mut self) -> &mut Tab {
        self
    }

    async fn follow_new_tabs(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// Whether a pursuit that ended this way may still find the goal through a web search
/// of the site: it ran out of ways on, rather than reaching it or stopping where
/// someone must act. Wanting text with nowhere to type it counts: the site has no
/// search box.
fn searchable(outcome: &Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Stuck { .. }
            | Outcome::Unsure { .. }
            | Outcome::GaveUp { .. }
            | Outcome::OutOfBudget
            | Outcome::DeadEnd
            | Outcome::NeedsAgent { field: None }
    )
}

/// Works toward `goal` from the current page until someone else must act. Where text
/// is needed, one of `texts` (the agent's suggestions) may be typed; anything else is
/// the agent's to write.
pub async fn pursue<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    goal: &str,
    texts: &[String],
    limits: &Limits,
) -> Result<Report, JevError> {
    // A path that reached this goal on this site before is replayed without asking Jev,
    // as long as each step's element is there. Whatever reaches the goal this time is
    // remembered for next time.
    let site = url::Url::parse(&workspace.tab().url())
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let replay = workspace.recall_path(&site, goal).await.unwrap_or_default();
    let replayed = !replay.is_empty();
    // Without the agent's suggestions, the goal's own words may go into a site search
    // before the agent is asked for any (see words.rs).
    let own_words: Vec<String> = if texts.is_empty() {
        judged_search_words(jev, goal, &site).await
    } else {
        Vec::new()
    };
    // And its numbers (a price limit, a count), for a field or slider that takes one.
    let own_texts: Vec<String> = if texts.is_empty() {
        own_words.iter().cloned().chain(numbers_in(goal)).collect()
    } else {
        Vec::new()
    };
    let texts = if texts.is_empty() {
        &own_texts[..]
    } else {
        texts
    };
    let mut pursuit = Pursuit::new(goal, texts, limits, replay, false);
    let mut tried: Vec<(String, String)> = Vec::new();
    let mut fan_outs = 0;
    // The site lookup (site_search.rs) is possible on a site the search engines don't
    // own; it runs once, with `lookup_words`.
    let can_look_up = limits.lookup
        && workspace.browser().is_some()
        && !site.is_empty()
        && !Engine::serves(&site);
    let mut looked_up = false;
    let mut learnable = true;

    let outcome = loop {
        // The budget holds within a step too: a page read that hung for minutes kept a
        // search's branch, and so the search, past every limit.
        let left = limits.max_time.saturating_sub(pursuit.elapsed()) + STEP_GRACE;
        let next = match tokio::time::timeout(left, pursuit.think(workspace.tab(), jev)).await {
            Ok(next) => next?,
            Err(_) => break Outcome::OutOfBudget,
        };
        // A site's own search that didn't lead to the goal within a couple of steps
        // rarely does later (microsoft.com's doesn't even index its support site), while
        // the web's index of the site often has the page as its first result: look it up
        // now rather than after exploring runs out. Only once the page the last step
        // opened has been judged and found short of the goal; if the lookup finds
        // nothing, exploring goes on from here as decided.
        // Likewise once the page's likeliest links were all explored side by side in
        // vain: what is left on it is a weaker guess than the web's index.
        // Not once the page itself was worked on (a journey planner's stations, an
        // editor's text, a listing's filters): a lookup only leaves it, at best for a page
        // that isn't filled in. A JSON blob looked up across the web ended on the site's
        // home page, "reached"; listings lost the filters already set.
        if can_look_up
            && !looked_up
            && !pursuit.worked_here()
            && !matches!(next, Next::Stop(_))
            && (pursuit.steps_since_typing() >= Some(PATIENCE_AFTER_SEARCH) || !tried.is_empty())
            && let Some(words) = lookup_words(jev, goal, texts, &own_words, &site).await
        {
            looked_up = true;
            if let Some(found) = look_up(workspace, jev, goal, &words, &site, &mut pursuit).await {
                learnable = false;
                break found;
            }
        }
        let Next::FanOut { links, options } = next else {
            match pursuit.act(workspace, next).await {
                Some(outcome) => break outcome,
                None => continue,
            }
        };
        if fan_outs >= MAX_FAN_OUTS || workspace.browser().is_none() {
            break Outcome::Unsure { options };
        }
        fan_outs += 1;
        let names: Vec<&str> = links.iter().map(|l| l.label.as_str()).collect();
        pursuit.push_step(
            format!(
                "opened {} links side by side: {}",
                links.len(),
                names.join(" | ")
            ),
            links[0].confidence,
        );
        let branch_limits = limits.for_branch(pursuit.elapsed());
        let result = fan_out(
            workspace,
            jev,
            goal,
            texts,
            &links,
            Collect::First,
            &branch_limits,
        )
        .await;
        pursuit.decisions += result.decisions;
        pursuit.tab_notes.extend(result.notes);
        // Reaching the goal beats everything; short of that, a branch that stopped
        // where the agent or the user must act is where control goes back (its refs
        // are valid in its tab, which becomes the tab in front).
        let mut waiting = result.waiting.into_iter();
        let found = match result.reached.into_iter().next() {
            Some(found) => Some(found),
            None => waiting.next(),
        };
        if let Some(browser) = workspace.browser() {
            for other in waiting {
                let _ = browser.close_tab(other.tab.target_id()).await;
            }
        }
        if let Some(found) = found {
            let branch = found.branch;
            let branch_outcome = branch.outcome.clone();
            pursuit.push_step(
                format!("continued in the tab for {}", branch.link.label),
                branch.link.confidence,
            );
            pursuit.path.push(branch.link.identity.clone());
            pursuit.path.extend(branch.path);
            pursuit.steps.extend(branch.steps);
            pursuit.trail.extend(branch.trail);
            pursuit.tab_notes.extend(workspace.adopt(found.tab).await);
            break branch_outcome;
        }
        tried.extend(
            result
                .gave_up
                .iter()
                .map(|b| (b.link.label.clone(), b.outcome.why())),
        );
        // Back on this page, those links are spent; decide again among the rest.
        pursuit.mark_tried(&links);
    };
    // Links were explored and gave up, and then nothing else here led anywhere: the
    // agent should hear what was tried rather than just "stuck".
    let outcome = match outcome {
        Outcome::Stuck { .. } | Outcome::Unsure { .. } if !tried.is_empty() => {
            Outcome::GaveUp { tried }
        }
        outcome => outcome,
    };
    // The site's own navigation stopped short (or there is nowhere on it to type): the
    // web's index of the site may know the page. A path found that way starts on the
    // search engine, so it isn't one to replay on the site.
    let words = if can_look_up && !looked_up && !pursuit.worked_here() && searchable(&outcome) {
        lookup_words(jev, goal, texts, &own_words, &site).await
    } else {
        None
    };
    let outcome = match words {
        Some(words) => match look_up(workspace, jev, goal, &words, &site, &mut pursuit).await {
            Some(found) => {
                learnable = false;
                found
            }
            None => outcome,
        },
        None => outcome,
    };
    // A learned path that was taken and didn't reach the goal is forgotten: kept, it is
    // taken the same way next time (npm's tab detour, learned whole, failed every run).
    if replayed && !matches!(outcome, Outcome::Reached) && !site.is_empty() {
        workspace.forget_path(&site, goal).await;
    }
    if learnable
        && matches!(outcome, Outcome::Reached)
        && !pursuit.path.is_empty()
        && !site.is_empty()
    {
        // The same step twice in a row is one step: on a single-page app the first click
        // may not show yet (GitHub's "Issues", learned and replayed twice).
        let mut path = pursuit.path.clone();
        path.dedup();
        workspace.learn_path(&site, goal, &path).await;
    }
    // What the page is set to: its settings (page chrome and noise left out,
    // candidates.rs) and its address, where listings keep criteria in the path
    // (`/austin-tx/under-500000/`) or query. A reached page from a lookup had no line
    // at all, though its address showed the missing beds filter.
    let settings = if matches!(outcome, Outcome::Reached) {
        // The fields only where the pursuit set something: on a page only read (a
        // scoreboard), its header's widgets are noise.
        let mut settings = if pursuit.worked_here() {
            match observe(workspace.tab(), Detail::All, None).await {
                Ok(model) => page_settings(&model),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        // The address where it may hold criteria: a listing's, one with a query, or
        // the page the pursuit set something on; an article's is no setting.
        let url = workspace.tab().url();
        let criteria = pursuit.worked_here()
            || pursuit.on_listing()
            || url::Url::parse(&url).is_ok_and(|u| u.query().is_some());
        if criteria {
            settings.extend(address_setting(&url));
        }
        settings
    } else {
        Vec::new()
    };
    let elapsed = pursuit.elapsed();
    let came_from = pursuit.came_from().map(str::to_owned);
    Ok(Report {
        outcome,
        came_from,
        settings,
        steps: pursuit.steps,
        tab_notes: pursuit.tab_notes,
        decisions: pursuit.decisions,
        trail: pursuit.trail,
        elapsed,
    })
}

/// The words to look the goal up with across the site: the goal's own (judged in any
/// language), which say what the page is about; the agent's texts are often what goes
/// into a form on the way ("Berlin Hbf", a JSON document to validate), and a query of
/// one of them found pages about that instead. A query-like text stands in when the goal
/// gives too few words.
async fn lookup_words(
    jev: &Jev,
    goal: &str,
    texts: &[String],
    own_words: &[String],
    site: &str,
) -> Option<String> {
    let goal_words = if own_words.is_empty() {
        judged_search_words(jev, goal, site).await
    } else {
        own_words.to_vec()
    };
    goal_words.into_iter().next().or_else(|| {
        texts
            .iter()
            .find(|t| {
                t.chars().any(char::is_alphabetic)
                    && t.split_whitespace().count() <= MAX_QUERY_WORDS
                    && !t.contains(['{', '}', '[', ']', '<', '>', '=', '\n'])
            })
            .cloned()
    })
}

/// A text longer than this is something to enter, not a query.
const MAX_QUERY_WORDS: usize = 8;

/// A page's address as a setting (`address: /bilar/volvo?price_to=200000`): criteria
/// set as chips, links or an SEO path show nowhere else. Tracking parameters left out;
/// `None` for a bare home page or one-segment path without a query.
fn address_setting(url: &str) -> Option<String> {
    let kept = url::Url::parse(&crate::loops::place(url)).ok()?;
    let path = kept.path().trim_end_matches('/');
    let segments = path.split('/').filter(|s| !s.is_empty()).count();
    if kept.query().is_none() && segments < 2 {
        return None;
    }
    let mut address: String = path.chars().take(80).collect();
    if let Some(query) = kept.query() {
        address.push('?');
        address.extend(query.chars().take(160));
    }
    Some(format!("address: {address}"))
}

/// How far past its time budget a step may run before it is cut off.
const STEP_GRACE: Duration = Duration::from_secs(15);

/// Steps after typing into the site's own search before the site lookup is tried.
const PATIENCE_AFTER_SEARCH: usize = 2;

/// Looks the goal up across the site (its sitemaps, then a `site:` web search) and,
/// when a page there fits, continues in its tab: the outcome there, or `None` when
/// nothing fitted (the steps say what was tried).
async fn look_up<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    goal: &str,
    words: &str,
    site: &str,
    pursuit: &mut Pursuit<'_>,
) -> Option<Outcome> {
    let searched = site_search(workspace, jev, goal, words, site).await;
    pursuit.decisions += searched.decisions;
    pursuit.tab_notes.extend(searched.notes);
    pursuit.trail.extend(searched.trail);
    let looked = searched.looked.join(", then ");
    match searched.found {
        Some(found) => {
            let branch = found.branch;
            pursuit.push_step(format!("looked the goal up in {looked}"), 1.0);
            pursuit.push_step(
                format!("continued in the tab for {}", branch.link.label),
                branch.link.confidence,
            );
            pursuit.steps.extend(branch.steps);
            pursuit.trail.extend(branch.trail);
            pursuit.tab_notes.extend(workspace.adopt(found.tab).await);
            Some(branch.outcome)
        }
        None => {
            let tried = if searched.tried.is_empty() {
                "nothing there fitted".to_owned()
            } else {
                let pages: Vec<String> = searched
                    .tried
                    .iter()
                    .map(|(page, why)| format!("{page} ({why})"))
                    .collect();
                format!("opened {}", pages.join("; "))
            };
            pursuit.push_step(format!("looked the goal up in {looked}: {tried}"), 0.0);
            None
        }
    }
}
