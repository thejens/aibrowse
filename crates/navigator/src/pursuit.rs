//! One pursuit of a goal in one tab, in two halves:
//!
//! - `think` looks at the page and decides the next step. It sends no input, so several
//!   pursuits can think at once (fanout.rs).
//! - `act` carries the decision out with input, one tab at a time.
//!
//! `pursue` (pursue.rs) alternates the two in the tab in front.
//!
//! Navigating is a search: every click that leads somewhere is a choice point, and
//! `go_back` returns to the last one when the step led the wrong way. The step it undoes
//! is remembered as a dead end there (loops.rs), and Jev decides again on that page,
//! knowing it. Fan-out (fanout.rs) is the same search, several choices at a time.
//!
//! The agent may suggest texts to type (`texts`: search terms, a postcode). Where text
//! is needed, Jev may pick one of them for the field and the navigator types it and
//! presses Enter itself; anything else is written by the agent, so it is handed back.

use std::collections::VecDeque;
use std::time::Instant;

use aibrowse_browser::{ActionResult, ElementRef, FIELD_STAYED_EMPTY, Tab};
use aibrowse_perception::{Detail, Position, main_text, observe};
use serde_json::{Map, Value, json};

use crate::candidates::{Candidate, GO_BACK, NOTHING, PRESS, SCROLL, candidates, field_values};
use crate::consent;
use crate::digest::about;
use crate::jev::{Jev, JevError, choice, ranked};
use crate::judge::{Agreements, CONSEQUENTIAL, consequential, declining_commits};
use crate::loops::{Memory, fingerprint, place};
use crate::pursue::{Decision, Limits, Outcome, Step, Workspace};
use crate::words::content_words;

/// The page outline given to Jev is capped: it decides from a list of options, and a huge
/// state of irrelevant text makes those decisions worse, not better.
const MAX_OUTLINE_CHARS: usize = 6000;
const SCROLL_STEP: f64 = 650.0;
/// Ranked next actions kept per decision: enough to see what nearly won.
const DECISION_OPTIONS: usize = 5;

/// Below `min_confidence`, an element still wins when it clearly leads the other
/// elements: this much confidence, and this many times the runner-up's.
const CLEAR_LEAD: f64 = 0.3;
const LEAD_RATIO: f64 = 1.4;

/// On the way to a public page, how likely "reached" must be for a sign-in prompt to
/// be taken as noise on the goal page rather than a wall in the way.
const PUBLIC_REACHED: f64 = 0.2;

/// How long a page with nothing to act on gets to finish rendering, looked at again
/// this often. Waiting for it to settle doesn't help: it has settled, and draws later.
const RENDER_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
const RENDER_POLL: std::time::Duration = std::time::Duration::from_millis(300);

/// How much of a page's text the reached check reads: its opening, where a page says
/// what it is, and the passages about the goal (digest::about).
const VERIFY_CHARS: usize = 3000;
/// Below this, the reached check vetoes the page. It only vetoes pages it is fairly
/// sure are wrong: demanding confidence that a page is right rejected good pages whose
/// opening text is general or in another language than the goal.
const VERIFY_MIN: f64 = 0.25;
/// Main text shorter than this may still be rendering (`fulfils`): read again after
/// `THIN_WAIT`, up to `THIN_POLLS` times while it grows.
const THIN_TEXT: usize = 400;
const THIN_WAIT: std::time::Duration = std::time::Duration::from_millis(700);
const THIN_POLLS: usize = 3;

/// Steps that failed before doing anything offered again, per pursuit (`retry_later`).
const MAX_RETRIES: usize = 2;

/// The reached check's vetoing questions (`fulfils`), each with what it means told to
/// the agent.
const VETOES: [(&str, &str); 4] = [
    (
        "contradicts",
        "it is set to something the goal rules out (another date, place, variant or sort)",
    ),
    ("unmet", "a criterion the goal states isn't applied on it"),
    (
        "short_of",
        "it is a single item, or a form before its result, where the goal asks for more",
    ),
    (
        "stale",
        "its data is from an earlier period than the goal asks for (today's may not be out yet)",
    ),
];

/// How sure Jev must be that a page is set to something the goal rules out for that to
/// veto it as reached (`fulfils`): a wrong veto costs steps, a missed one a false
/// "reached", which the reached report's settings line still lets the agent catch.
const CONTRADICTS: f64 = 0.7;
/// When Jev would rather navigate on but "reached" is at least this likely, the page's
/// text decides: a help article whose title words the task differently than the goal
/// ("Change default apps" for "change the default browser") looks like a step on the
/// way in an outline, and reads like the goal in its text. Low, because the check is
/// cheap (one Jev request, ~0.25 s, once per page text) and the outline rarely shows
/// what a page says: H&M's returns page, its instructions expanded, was 0.2 "reached"
/// by its outline and 0.95 by its text.
const CLOSE_REACHED: f64 = 0.1;
/// A consent prompt takes one press to decline (or two: its settings, then saving with
/// the optional off) and perhaps one to close the notice confirming it; more presses
/// would be a prompt the navigator doesn't understand.
const MAX_CONSENT_CLICKS: usize = 3;
/// The wait before loading an error page again: long enough for a throttled or briefly
/// overloaded site to answer, short beside a pursuit.
const RELOAD_PAUSE: std::time::Duration = std::time::Duration::from_secs(2);
/// How sure the text check must be to overrule Jev's top turn: well above the veto's
/// bar, since here the outline leaned the other way.
const CONFIRM_MIN: f64 = 0.6;

/// Fields with values a step's description names at most (`fields_then`).
const FIELDS_THEN: usize = 3;

/// How sure Jev must be that a suggested text fits the field to type it.
const TEXT_MIN: f64 = 0.5;

/// Options that would commit something skipped on one page before handing back: past
/// a few, the page is about committing, and the agent should look.
const MAX_SKIPPED_COMMITS: usize = 3;
/// How sure Jev must be that a step which would commit something is the way on for the
/// pursuit to stop before it, for the agent, rather than look past it.
const CLEAR_WAY: f64 = 0.6;

/// Links opened side by side when no option clearly leads (fanout.rs), at most...
const FAN_OUT: usize = 3;
/// ...and only those Jev gives at least this much: a long tail of 2% options is noise.
const FAN_OUT_MIN: f64 = 0.1;

/// A link the pursuit might open in a background tab.
#[derive(Debug, Clone)]
pub struct Link {
    pub open: Opening,
    pub label: String,
    pub identity: String,
    pub confidence: f64,
    /// An index (a sitemap, a search engine) named this page for the goal, rather than
    /// it being a step on the way: its text is checked before anything is clicked.
    pub named_for_goal: bool,
}

/// How a link's background tab is opened.
#[derive(Debug, Clone)]
pub enum Opening {
    /// Modifier-clicking the element on the page, as a person would.
    Click(ElementRef),
    /// Loading the address in a new tab: a page known from elsewhere (a sitemap), with
    /// no link to it on the page.
    Address(String),
}

/// What `think` decided.
pub(crate) enum Next {
    /// The pursuit is over.
    Stop(Outcome),
    /// A consent banner blocks the page; refusing non-essential cookies clears it
    /// without deciding anything on the user's behalf that they'd regret. Some sites
    /// then confirm the refusal in a dialog with one button, which is pressed too.
    Cookies {
        button: ElementRef,
        /// The step as the trail tells it.
        step: String,
    },
    /// The page is an error page (the site overloaded, throttling, a gateway down):
    /// load it again once, after a pause, before deciding anything from it.
    Reload {
        url: String,
    },
    /// The next step of a learned path.
    Replay {
        candidate: Chosen,
    },
    Scroll {
        confidence: f64,
    },
    Click {
        candidate: Chosen,
        confidence: f64,
    },
    /// Back to the last choice point: the step from there led the wrong way.
    GoBack {
        confidence: f64,
    },
    /// Press one of the agent's offered keys or chords, wherever focus is.
    Press {
        chord: String,
        confidence: f64,
    },
    /// Type one of the agent's suggested texts into a field and press Enter.
    Type {
        field: Chosen,
        text: String,
        confidence: f64,
    },
    /// No option clearly leads, but several links might: open them side by side.
    /// `options` is what the agent would be told if that can't happen.
    FanOut {
        links: Vec<Link>,
        options: Vec<(String, f64)>,
    },
}

pub(crate) struct Chosen {
    reference: ElementRef,
    label: String,
    identity: String,
    is_link: bool,
    is_range: bool,
    /// The option to choose, for an option of a native `<select>` (candidates.rs).
    option: Option<String>,
    /// The option that would undo this choice (`Candidate::undo`).
    undo: Option<String>,
}

/// A step that took the pursuit to another page, so going back undoes it.
struct ChoicePoint {
    /// The place it was taken from, and what was taken there.
    place: String,
    identity: String,
    label: String,
    /// When the step opened a new tab: the tab it was taken in, to return to.
    opener: Option<String>,
}

pub(crate) struct Pursuit<'g> {
    goal: &'g str,
    limits: Limits,
    started: Instant,
    pub steps: Vec<Step>,
    pub decisions: usize,
    pub trail: Vec<Decision>,
    pub tab_notes: Vec<String>,
    /// The element identities clicked so far, learned as the path when the goal is
    /// reached.
    pub path: Vec<String>,
    /// The place each step of `path` was taken from (`cut_detour`).
    path_from: Vec<String>,
    memory: Memory,
    /// Places where scrolling no longer moves the page (the bottom).
    scrolled_out: Vec<(String, u64)>,
    /// Consent buttons pressed (by identity): declining, then perhaps the notice
    /// confirming it. Never more than `MAX_CONSENT_CLICKS`.
    consent_clicks: Vec<String>,
    /// Whether a consent prompt's settings were opened, to decline there.
    consent_settings: bool,
    /// Dropdown options that would undo a choice this pursuit made: not offered again.
    /// Unsure whether its choice showed, the navigator set PyPI's "Order by" back to
    /// "Relevance" and Hemnet's maximum price back to "Max".
    undos: Vec<String>,
    /// Places loaded again after showing an error page: each is retried once.
    reloaded: Vec<String>,
    /// Whether the goal needs the user's account, as Jev first judged it. It is about
    /// the goal, which doesn't change, so the first answer holds: asked again on a
    /// sign-in page, the page itself sways it (apple.com's check coverage: 0.2–0.45 on
    /// the way, 0.64 on the sign-in page), just where it decides whether that page is a
    /// wall to go back from or the user's turn.
    needs_account: Option<f64>,
    /// Steps that don't count against the step budget: declining cookies and loading an
    /// error page again clear the way to a page rather than move toward the goal, and a
    /// search result allowed two steps spent both on a cookie wall before it was read.
    /// Both are bounded on their own (`MAX_CONSENT_CLICKS`, once per place).
    housekeeping: usize,
    /// A path that reached this goal on this site before, replayed without asking Jev
    /// as long as each step's element is there; the first mismatch hands back to
    /// deciding.
    replay: VecDeque<String>,
    /// Exploring one branch of a fan-out: going back from where the branch started ends
    /// it (its parent page decides again), and a pursuit without a clear next step ends
    /// rather than fanning out again.
    branch: bool,
    /// The choice points behind the current page, latest last.
    choices: Vec<ChoicePoint>,
    /// Texts the agent suggested typing where text is needed.
    texts: &'g [String],
    /// Texts typed so far, by field (`typed_key`): the same search again (from its own
    /// results page, say) gives the same results, so each is typed into a search box once
    /// per pursuit, and into any other field once per page.
    typed: Vec<String>,
    /// Whether the page the pursuit is on is a listing or search results, where a click
    /// that keeps the page (a filter panel opened, a facet expanded) works on it.
    listing: bool,
    /// The indices in `steps` of housekeeping steps (cookies, a reload), which aren't
    /// steps toward the goal (`steps_since_typing`).
    chores: Vec<usize>,
    /// Whether a consent prompt was accepted (once per pursuit, agreements accepted).
    accepted_consent: bool,
    /// Links not offered on the current page (`avoid_links`).
    avoided: Vec<String>,
    /// Whether the page itself was worked on (`worked_here`).
    worked_here: bool,
    /// Steps that failed before doing anything and were offered again (`retry_later`).
    retried: Vec<String>,
    /// The index in `steps` of the last site search (text typed that led to another
    /// page), for `steps_since_typing`.
    typed_at: Option<usize>,
    /// Steps taken without a convincing option (see `Limits::guesses`).
    guesses: usize,
    /// The reached check's verdict on each place it read, by place and a hash of the
    /// text it read (see `fulfils`). Asked again, a borderline judgment can come out the
    /// other way, so a text is read once; a tab or filter that changes what a place says
    /// without changing its address is read again.
    checked: Vec<(String, u64, f64)>,
    /// The page was named for the goal by an index (see `Link::named_for_goal`): its
    /// first look starts with the text check.
    verify_first: bool,
    /// The page `think` last looked at: (URL, place, state fingerprint), and its title.
    here: (String, String, u64),
    /// The place before `here`, when the pursuit has moved: a link leading back there
    /// is an undo (eBay's "Buy It Now" link, clicked again on the filtered page, took
    /// the filter off), and going back has its own option.
    came_from: Option<String>,
    title: String,
    /// The fields on the page `think` last looked at, labelled with their values.
    fields: Vec<String>,
}

impl<'g> Pursuit<'g> {
    pub fn new(
        goal: &'g str,
        texts: &'g [String],
        limits: &Limits,
        replay: Vec<String>,
        branch: bool,
    ) -> Self {
        Self {
            goal,
            limits: limits.clone(),
            started: Instant::now(),
            steps: Vec::new(),
            decisions: 0,
            trail: Vec::new(),
            tab_notes: Vec::new(),
            path: Vec::new(),
            path_from: Vec::new(),
            memory: Memory::default(),
            scrolled_out: Vec::new(),
            consent_clicks: Vec::new(),
            consent_settings: false,
            undos: Vec::new(),
            reloaded: Vec::new(),
            needs_account: None,
            housekeeping: 0,
            replay: replay.into(),
            branch,
            choices: Vec::new(),
            texts,
            typed: Vec::new(),
            retried: Vec::new(),
            worked_here: false,
            listing: false,
            avoided: Vec::new(),
            accepted_consent: false,
            chores: Vec::new(),
            typed_at: None,
            guesses: 0,
            checked: Vec::new(),
            verify_first: false,
            here: (String::new(), String::new(), 0),
            came_from: limits.part_of.as_ref().and_then(|p| p.came_from.clone()),
            title: String::new(),
            fields: Vec::new(),
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// Starts the pursuit by checking whether the page's text already is the goal.
    pub fn verify_first(&mut self) {
        self.verify_first = true;
    }

    /// The URL and title of the page last looked at.
    pub fn page(&self) -> (&str, &str) {
        (&self.here.0, &self.title)
    }

    /// Looks at the page in `tab` and decides what to do next. Sends no input.
    pub async fn think(&mut self, tab: &mut Tab, jev: &Jev) -> Result<Next, JevError> {
        if self.started.elapsed() >= self.limits.max_time {
            return Ok(Next::Stop(Outcome::OutOfBudget));
        }
        // The step budget limits what is done, not what is seen: the page the last step
        // opened is still judged (a branch's fourth click is often the one that lands),
        // by its text alone, since no step may follow.
        let out_of_steps = self.steps.len() - self.housekeeping >= self.limits.max_steps;
        let mut observed = observe(tab, Detail::All, None).await;
        // A page with nothing to act on is still rendering (an app that settled before
        // drawing its content): it gets one more moment before anyone judges it.
        let rendering_since = Instant::now();
        while observed.as_ref().is_ok_and(|m| candidates(m).is_empty())
            && rendering_since.elapsed() < RENDER_WAIT
        {
            tokio::time::sleep(RENDER_POLL).await;
            observed = observe(tab, Detail::All, None).await;
        }
        let model = match observed {
            Ok(model) => model,
            Err(err) => {
                self.steps.push(failed_step(
                    "observe the page",
                    &tab.url(),
                    &err.to_string(),
                ));
                return Ok(Next::Stop(Outcome::Stuck {
                    options: Vec::new(),
                }));
            }
        };
        // What the page's fields hold, for judging whether a part of a task is done:
        // on an app drawn on a canvas, a field's value (the Name Box's "C3:C10") is the
        // only sign of what was done.
        self.fields = field_values(&model);
        self.listing = model
            .page
            .as_ref()
            .is_some_and(|p| matches!(p.kind, "listing" | "search-results"));
        let url = model.url.clone();
        let here = place(&url);
        self.cut_detour(&here);
        let state_id = fingerprint(&model);
        if !self.here.1.is_empty() && self.here.1 != here {
            self.came_from = Some(self.here.1.clone());
        }
        self.here = (url, here.clone(), state_id);
        self.title = model.title.clone();
        if model.page.as_ref().is_some_and(|p| p.kind == "error") && !self.reloaded.contains(&here)
        {
            self.reloaded.push(here.clone());
            return Ok(Next::Reload {
                url: model.url.clone(),
            });
        }
        // A page an index named for the goal probably is it, and pages like that (a
        // help article with FAQ toggles) offer plenty to click that isn't needed: its
        // text decides first. Short of a clear yes, it is explored as usual.
        if self.verify_first || out_of_steps {
            self.verify_first = false;
            // Its own entry in the trail, where the check's verdict is recorded.
            self.trail.push(Decision {
                url: self.here.0.clone(),
                candidates: 0,
                turn: Vec::new(),
                next: Vec::new(),
                text_check: None,
                text: None,
            });
            if confirmed(self.fulfils(tab, jev, &here).await?) {
                return Ok(Next::Stop(Outcome::Reached));
            }
            if out_of_steps {
                return Ok(Next::Stop(Outcome::OutOfBudget));
            }
        }
        let mut options = candidates(&model);
        options.retain(|c| !self.undos.contains(&c.identity));
        // A checkbox, switch or radio the pursuit already set stays set: clicking it
        // again only clears it (Zalando's size "43" checked, then unchecked; Fnac's
        // price band set, cleared, set).
        options.retain(|c| {
            !(SETTING_ROLES
                .iter()
                .any(|role| c.identity.starts_with(role))
                && self.path.contains(&c.identity))
        });
        if !self.avoided.is_empty()
            && let Ok(base) = url::Url::parse(&model.url)
        {
            options.retain(|c| {
                !c.href
                    .as_deref()
                    .and_then(|href| base.join(href).ok())
                    .is_some_and(|target| self.avoided.iter().any(|a| *a == target.as_str()))
            });
        }
        if let (Some(from), Ok(base)) = (&self.came_from, url::Url::parse(&self.here.0)) {
            options.retain(|c| {
                !c.href
                    .as_deref()
                    .and_then(|href| base.join(href).ok())
                    .is_some_and(|target| undoes(&target, &base, from))
            });
        }
        if let Some(identity) = self.replay.pop_front() {
            if let Some(candidate) = options.iter().find(|c| c.identity == identity) {
                self.memory.record(&here, state_id, &candidate.identity);
                return Ok(Next::Replay {
                    candidate: chosen(candidate),
                });
            }
            // The page changed since the path was learned: decide from here on.
            self.replay.clear();
        }
        let tried = self.memory.rejected_at(&here);
        let mut state = state(self.goal, &model.render(), &self.steps, &tried);
        if let Some(part_of) = &self.limits.part_of {
            state["parts_done_before"] = json!(part_of.before);
            state["parts_after"] = json!(part_of.after);
        }
        let back = self.back_option();
        // Scrolling is offered only with something to scroll to: on a page shown whole
        // ("This product is no longer listed"), it was the likeliest step (0.9), did
        // nothing, and kept the navigator from going back.
        let viewport = &model.viewport;
        let more_below = viewport.scroll_y + viewport.height < viewport.content_height - 1.0;
        let can_scroll = more_below || options.iter().any(|c| !c.visible);
        let mut asked = questions(
            &options,
            back.as_deref(),
            self.texts,
            &self.limits.keys,
            can_scroll,
        );
        if self
            .limits
            .part_of
            .as_ref()
            .is_some_and(|p| !p.after.is_empty())
        {
            asked["turn"]["instructions"] = json!(
                "`goal` is one part of a larger task: `parts_done_before` are done, and `parts_after` come next, each in its own turn. Given `goal`, the current `page` and the `steps_taken` so far, who should act next? `goal` is reached as soon as its own part is done; what `parts_after` ask for is not part of it."
            );
            // A part is often an action ("select C3 to C10"), done rather than found: the
            // page shows it done (the Name Box says C3:C10), not that it is the page.
            asked["turn"]["criteria"]["reached"] = json!(
                "What `goal` asks for is done: the page shows it (a value, a selection, a setting as asked), or it is the page `goal` asks for; nothing more to do for this part"
            );
        }
        // Accepting has a click of its own, after declining used theirs (chefkoch's
        // "save selection" led straight back to its wall).
        let accept_left = self.limits.agreements == Agreements::Accept && !self.accepted_consent;
        if (self.consent_clicks.len() < MAX_CONSENT_CLICKS || accept_left)
            && consent::askable(&options)
        {
            asked["consent"] = consent::question(&options);
            if !self.consent_settings {
                asked["consent_settings"] = consent::settings_question(&options);
            }
            asked["consent_wall"] = consent::wall_question();
            if self.limits.agreements == Agreements::Accept {
                asked["consent_accept"] = consent::accept_question(&options);
            }
        }
        // Right after declining, a one-button dialog may be the site confirming it.
        let notice = (!self.consent_clicks.is_empty()
            && self.consent_clicks.len() < MAX_CONSENT_CLICKS)
            .then(|| consent::notice_button(&model))
            .flatten();
        if notice.is_some() {
            asked["consent_notice"] = consent::notice_question();
        }
        let answers = jev.ask(&state, &asked).await?;
        self.decisions += 1;

        let turn = ranked(&answers, "turn");
        self.trail.push(Decision {
            url: self.here.0.clone(),
            candidates: options.len(),
            turn: turn.clone(),
            next: labelled(&ranked(&answers, "next"), &options, DECISION_OPTIONS),
            text_check: None,
            text: None,
        });
        // A consent prompt is settled first, by declining (consent.rs): it covers the
        // page, and what it asks is the user's to accept.
        if let Some((button, label)) = notice
            && consent::is_notice(&answers)
        {
            self.consent_clicks.push(label.clone());
            return Ok(Next::Cookies {
                button,
                step: format!("closed the notice confirming it ({label})"),
            });
        }
        // A wall that offers only accepting tracking or paying, once its settings were
        // looked at or there are none: the user's choice, not the navigator's to make
        // and not one to route around (a web lookup found kicker's last-season table).
        // Stopped for only where the profile asks about agreements; otherwise accepting
        // is a step like any, taken below once declining was looked for.
        if self.limits.agreements == Agreements::Ask
            && consent::is_wall(&answers)
            && consent::chosen(&answers, &options).is_none()
            && (self.consent_settings || consent::settings_chosen(&answers, &options).is_none())
        {
            return Ok(Next::Stop(Outcome::NeedsUser {
                reason: "consent_wall".to_owned(),
            }));
        }
        // Where declining isn't possible (a pay-or-consent wall, or settings already
        // looked in without a way out) and the profile accepts agreements, the prompt is
        // accepted: stopping at every such wall left an unattended agent nowhere.
        // A decline already clicked that left the prompt up is no way out either.
        let declines = consent::chosen(&answers, &options)
            .filter(|b| !self.consent_clicks.contains(&b.identity));
        let no_decline =
            declines.is_none() && (consent::is_wall(&answers) || self.consent_settings);
        if accept_left
            && no_decline
            && let Some(button) = consent::accept_chosen(&answers, &options)
            && !self.consent_clicks.contains(&button.identity)
            && self
                .declines_only(tab, jev, button.reference, false)
                .await?
        {
            self.accepted_consent = true;
            self.consent_clicks.push(button.identity.clone());
            return Ok(Next::Cookies {
                button: button.reference,
                step: format!(
                    "accepted the consent prompt, which offers no free way to decline ({})",
                    button.label
                ),
            });
        }
        if self.consent_clicks.len() < MAX_CONSENT_CLICKS
            && let Some(button) = consent::chosen(&answers, &options)
            && !self.consent_clicks.contains(&button.identity)
            // The button that declines must commit nothing else: "Reject all and
            // subscribe" on a pay-or-consent wall subscribes. The same check as for any
            // click on the user's behalf, which knows declining from committing; when
            // the whole page made Jev sure the button declines, only whether it does
            // something besides.
            && self.declines_only(tab, jev, button.reference, consent::surely_declines(&answers)).await?
        {
            self.consent_clicks.push(button.identity.clone());
            return Ok(Next::Cookies {
                button: button.reference,
                step: format!("declined optional cookies ({})", button.label),
            });
        }
        // No way to decline on the prompt's first layer: its settings, to decline there.
        if self.consent_clicks.len() < MAX_CONSENT_CLICKS
            && !self.consent_settings
            && let Some(button) = consent::settings_chosen(&answers, &options)
            && !self.consent_clicks.contains(&button.identity)
            && self.commits_nothing(tab, jev, button.reference).await?
        {
            self.consent_settings = true;
            self.consent_clicks.push(button.identity.clone());
            return Ok(Next::Cookies {
                button: button.reference,
                step: format!(
                    "opened the cookie settings to decline there ({})",
                    button.label
                ),
            });
        }
        let ranked_next = ranked(&answers, "next");
        let needs_account = *self
            .needs_account
            .get_or_insert(answers["needs_account"]["noul"].as_f64().unwrap_or(1.0));
        let public = needs_account < 0.5;
        let reached_p = turn
            .iter()
            .find(|(t, _)| t == "reached")
            .map_or(0.0, |(_, p)| *p);
        let mut top_turn = turn.first().map(|(t, _)| t.clone()).unwrap_or_default();
        if top_turn == "user" && public {
            // On the way to a public page, "only the user can continue" is a sign-in
            // prompt or wall, not the user's turn. Many sites show a sign-in prompt on
            // every page, so when the page may well be the goal it is; otherwise a
            // wall is a wrong turn to go back from, or Jev's next-best turn decides.
            top_turn = if reached_p >= PUBLIC_REACHED {
                "reached".to_owned()
            } else if back.is_some() {
                return Ok(Next::GoBack { confidence: 1.0 });
            } else {
                turn.iter()
                    .map(|(t, _)| t.clone())
                    .find(|t| t != "user")
                    .unwrap_or_default()
            };
        }
        // "Reached" is judged from the outline, which shows how a page is built, not
        // what it says: checked against the page's text before it is reported. A page
        // that doesn't hold up is often close (a careers page whose job list is one
        // link further), so the navigator goes on from it; one that leads nowhere is
        // gone back from as soon as Jev finds it stuck.
        // The other way round, a page Jev thinks may well be the goal but would rather
        // navigate on from is read too, and taken if its text clearly is the goal.
        // A place whose text was vetoed once needs a clear yes when what it says changes
        // (more loaded on scrolling): a borderline second read doesn't undo the veto.
        let vetoed = self
            .checked
            .iter()
            .any(|(place, _, p)| *place == here && *p < VERIFY_MIN);
        // A later part starts on the page where the one before it was judged done, and
        // "reached" there before a step of its own is often that verdict read again:
        // "the second page of the category" was 0.94 reached on its first page, its
        // text 0.49. It needs a clear yes too (a sort the page has by default does).
        let unstarted_part = self
            .limits
            .part_of
            .as_ref()
            .is_some_and(|part| !part.before.is_empty())
            && self.steps.is_empty();
        let floor = if vetoed || unstarted_part {
            CONFIRM_MIN
        } else {
            VERIFY_MIN
        };
        // A page without readable text is taken as Jev sees it in the tab in front (a
        // spreadsheet drawn on a canvas); a branch's page, opened side by side or by a
        // lookup, isn't taken unread: Zillow's and KVD's lookup pages "reached" so.
        let unread = if self.branch { 0.0 } else { 1.0 };
        if top_turn == "reached" && self.fulfils(tab, jev, &here).await?.unwrap_or(unread) < floor {
            top_turn = "navigate".to_owned();
        } else if top_turn == "navigate"
            && reached_p >= CLOSE_REACHED
            && confirmed(self.fulfils(tab, jev, &here).await?)
        {
            top_turn = "reached".to_owned();
        }
        match top_turn.as_str() {
            "reached" => return Ok(Next::Stop(Outcome::Reached)),
            "user" => {
                let (reason, _) = choice(&answers, "user_task");
                return Ok(Next::Stop(Outcome::NeedsUser { reason }));
            }
            "agent" => {
                let (field, _) = choice(&answers, "field");
                let field = options
                    .iter()
                    .find(|c| c.key == field)
                    .map(|c| (c.reference, c.label.clone()));
                // Text is needed but there is nowhere to type it yet: the field is
                // usually behind a button ("Search for help"), so the navigator opens
                // it and hands back once there is a field.
                let no_field = field.is_none() && !options.iter().any(|c| c.is_field);
                // A slider isn't typed into, so "which field takes text" often says none
                // where the agent's numbers are for sliders (a price range's two
                // handles): setting them is a step, chosen as one below.
                let sliders_to_set = field.is_none()
                    && options.iter().any(|c| c.is_range)
                    && self.texts.iter().any(|t| number_in(t).is_some());
                if !no_field && !sliders_to_set {
                    let chosen_field = options
                        .iter()
                        .find(|c| Some(c.reference) == field.as_ref().map(|f| f.0));
                    if let Some(candidate) = chosen_field
                        && let Some(next) = self
                            .typing(candidate, &answers, tab, jev, &here, state_id)
                            .await?
                    {
                        return Ok(next);
                    }
                    // None of the agent's texts fits, but one of its keys is the step
                    // Jev ranks first: in Google Sheets, "fill the formula down" was
                    // 0.53 "text is needed" (the formula bar) and 0.90 "press Meta+d".
                    // The agent offered the key for this, so it is pressed, as a step
                    // on the way rather than a hand-back.
                    let key_first = ranked_next.first().is_some_and(|(key, p)| {
                        self.offered_key(key).is_some() && *p >= self.limits.min_confidence
                    });
                    if !key_first {
                        return Ok(Next::Stop(Outcome::NeedsAgent { field }));
                    }
                }
            }
            // Stuck on a page reached by a choice: that choice was a wrong turn.
            "stuck" if back.is_some() => {
                let confidence = turn.first().map_or(0.0, |(_, p)| *p);
                return Ok(Next::GoBack { confidence });
            }
            "stuck" => {
                return Ok(Next::Stop(Outcome::Stuck {
                    options: labelled(&ranked_next, &options, 3),
                }));
            }
            _ => {}
        }

        // Navigate: the best option not already taken in this state (see loops.rs).
        let untried: Vec<(String, f64)> = ranked_next
            .iter()
            .filter(|(key, _)| {
                if key == SCROLL {
                    return !self.scrolled_out.contains(&(here.clone(), state_id));
                }
                if key == GO_BACK {
                    return back.is_some();
                }
                if let Some(chord) = self.offered_key(key) {
                    return self
                        .memory
                        .allows(&here, state_id, &pressed_identity(chord));
                }
                by_key(&options, key)
                    .is_some_and(|c| self.memory.allows(&here, state_id, &c.identity))
            })
            .cloned()
            .collect();
        if untried.is_empty() {
            return Ok(Next::Stop(Outcome::Stuck {
                options: labelled(&ranked_next, &options, 3),
            }));
        }
        // A step that would commit something (buy, send, accept) is a wrong turn on the
        // way to a goal that doesn't: skip it and take the next best. When the goal
        // itself commits, it is the agent's to decide (and the user's to approve).
        let goal_commits = answers["goal_commits"]["noul"].as_f64().unwrap_or(1.0) >= 0.5;
        let mut untried = untried;
        for _ in 0..MAX_SKIPPED_COMMITS {
            let picked = match pick(&untried, self.limits.min_confidence) {
                Some(picked) => Some(picked),
                None => {
                    // A branch doesn't fan out again: it is one already.
                    let links = fan_out_links(&untried, &options);
                    if !self.branch && links.len() >= 2 {
                        return Ok(Next::FanOut {
                            links,
                            options: labelled(&ranked_next, &options, 3),
                        });
                    }
                    // Nothing clear and nothing to open side by side: try the best
                    // option anyway, since going back undoes a wrong guess.
                    let guess = (self.guesses < self.limits.guesses)
                        .then(|| best_element(&untried))
                        .flatten();
                    // Short of a convincing step, the page may already be it: read
                    // before guessing, as before scrolling. Blocket's listing had every
                    // criterion in its address when a 0.14 guess cleared its search.
                    // Not on a part that hasn't taken a step yet: its page is where the
                    // part before ended, and read again it passed for the next part's
                    // goal (Hacker News' first Show page for "the second page").
                    if !unstarted_part && confirmed(self.fulfils(tab, jev, &here).await?) {
                        return Ok(Next::Stop(Outcome::Reached));
                    }
                    if guess.is_some() {
                        self.guesses += 1;
                    }
                    guess
                }
            };
            let Some((key, confidence)) = picked else {
                // Only what is still open: a step already taken here and blocked from
                // being taken again was told as the likeliest (ICA's servings button).
                return Ok(Next::Stop(Outcome::Unsure {
                    options: labelled(&untried, &options, 3),
                }));
            };
            if key == SCROLL {
                // Scrolling means what is needed may be on this very page; the outline
                // shows how it is built, not what it says (a price under a plan's
                // heading), so its text is read before looking further down.
                if confirmed(self.fulfils(tab, jev, &here).await?) {
                    return Ok(Next::Stop(Outcome::Reached));
                }
                return Ok(Next::Scroll { confidence });
            }
            if key == GO_BACK {
                return Ok(Next::GoBack { confidence });
            }
            if let Some(chord) = self.offered_key(&key).map(str::to_owned) {
                let identity = pressed_identity(&chord);
                self.memory.record(&here, state_id, &identity);
                // A key does whatever the focused element does with it: Enter in a
                // message box sends. Judged like a click on that element, and one that
                // would commit is never pressed on the user's behalf, even toward a goal
                // that commits: with no element to stop before, the agent presses it
                // itself (through act, where the user approves).
                if self.press_commits(tab, jev, &chord).await? {
                    self.memory.reject(&here, &identity);
                    untried.retain(|(k, _)| *k != key);
                    continue;
                }
                return Ok(Next::Press { chord, confidence });
            }
            let Some(candidate) = by_key(&options, &key) else {
                return Ok(Next::Stop(Outcome::Stuck {
                    options: labelled(&ranked_next, &options, 3),
                }));
            };
            // Scrolling reveals more each time, so it stays available until the page
            // stops moving (the bottom); every other step is remembered.
            self.memory.record(&here, state_id, &candidate.identity);
            // A slider is set to a value (a price filter's maximum) from the texts; a
            // click alone sets nothing, so without a text that fits it is passed over.
            if candidate.is_range {
                // With one number among the texts there is nothing to choose: the text
                // question asks what to type, and a slider isn't typed into, so Jev
                // often answered none. With several, it decides.
                let numbers: Vec<&String> = self
                    .texts
                    .iter()
                    .filter(|t| number_in(t).is_some())
                    .collect();
                if let [only] = numbers[..] {
                    let identity = typed_identity(&candidate.identity, only);
                    if !self
                        .typed
                        .contains(&self.typed_key(&candidate.label, &identity))
                        && self.memory.allows(&here, state_id, &identity)
                    {
                        self.memory.record(&here, state_id, &identity);
                        self.typed.push(self.typed_key(&candidate.label, &identity));
                        return Ok(Next::Type {
                            field: chosen(candidate),
                            text: only.clone(),
                            confidence,
                        });
                    }
                }
                if let Some(next) = self
                    .typing(candidate, &answers, tab, jev, &here, state_id)
                    .await?
                {
                    return Ok(next);
                }
                self.memory.reject(&here, &candidate.identity);
                untried.retain(|(k, _)| *k != key);
                continue;
            }
            // A field ranked first while navigating is where text goes next, though Jev
            // weighed the turn as a step rather than as text needed: "select E2 to E10"
            // ranked the Name Box first, and a click on it typed nothing, so the part
            // drifted to the next part's shortcut. With a text that fits, it is typed.
            if candidate.is_field
                && let Some(next) = self
                    .typing(candidate, &answers, tab, jev, &here, state_id)
                    .await?
            {
                return Ok(next);
            }
            if let Ok(context) = tab.action_context(candidate.reference).await {
                self.decisions += 1;
                let action = match &candidate.option {
                    Some(option) => format!("choose the option {option:?} in"),
                    None => "click".to_owned(),
                };
                let p = consequential(jev, &action, &context, self.limits.agreements).await?;
                if p >= CONSEQUENTIAL {
                    tracing::debug!(label = %candidate.label, consequential = p, "step would commit something");
                    // A step Jev is clear is the way on (accepting a ticket shop's
                    // terms to see its calendar, 0.71) is the agent's to take, with the
                    // user's approval: stopped before rather than passed over, which
                    // left Lawson's terms page "unsure" or gone back on as a wrong turn.
                    if goal_commits || confidence >= CLEAR_WAY {
                        return Ok(Next::Stop(Outcome::Consequential {
                            element: candidate.reference,
                            label: candidate.label.clone(),
                        }));
                    }
                    self.memory.reject(&here, &candidate.identity);
                    untried.retain(|(k, _)| *k != key);
                    continue;
                }
            }
            return Ok(Next::Click {
                candidate: chosen(candidate),
                confidence,
            });
        }
        Ok(Next::Stop(Outcome::Stuck {
            options: labelled(&ranked_next, &options, 3),
        }))
    }

    /// Carries out what `think` decided, in the workspace's tab in front. Returns the
    /// outcome when the pursuit ends here (it loops).
    pub async fn act<W: Workspace>(&mut self, workspace: &mut W, next: Next) -> Option<Outcome> {
        let url = self.here.0.clone();
        let tab_before = workspace.tab().target_id().to_owned();
        // For a click: the choice point it makes, and whether it navigated this tab.
        let mut choice: Option<(ChoicePoint, bool)> = None;
        // A web link that was clicked: where the page was before.
        let mut link_from: Option<String> = None;
        let (description, confidence, result) = match next {
            Next::Stop(outcome) => return Some(outcome),
            Next::FanOut { options, .. } => return Some(Outcome::Unsure { options }),
            Next::Reload { url: page } => {
                tokio::time::sleep(RELOAD_PAUSE).await;
                let result = workspace.tab().navigate(&page).await;
                self.housekeeping += 1;
                self.chores.push(self.steps.len());
                self.steps.push(Step {
                    action: "loaded the page again: it was an error page".to_owned(),
                    confidence: 1.0,
                    url,
                    error: result.err().map(|e| e.to_string()),
                });
                return None;
            }
            Next::Cookies { button, step } => {
                let result = workspace.tab().click(button).await;
                self.housekeeping += 1;
                self.chores.push(self.steps.len());
                self.steps.push(Step {
                    action: step,
                    confidence: 1.0,
                    url,
                    error: result.err().map(|e| e.to_string()),
                });
                return None;
            }
            Next::Replay { candidate } => {
                let (result, step) = operate(workspace.tab(), &candidate).await;
                if result.is_ok()
                    && let Some(undo) = &candidate.undo
                {
                    self.undos.push(undo.clone());
                }
                if let Ok(done) = &result {
                    self.walked(candidate.identity.clone());
                    choice = Some((self.choice_point(&candidate), moved(done)));
                }
                (format!("{step} (learned path)"), 1.0, result.map(drop))
            }
            Next::GoBack { confidence } => return self.go_back(workspace, confidence).await,
            Next::Type {
                field,
                text,
                confidence,
            } => {
                // A slider is set to the number in the text; nothing is submitted.
                if field.is_range {
                    let result = match number_in(&text) {
                        Some(value) => workspace
                            .tab()
                            .set_range(field.reference, value)
                            .await
                            .map_err(|e| e.to_string()),
                        None => Err(format!("{text:?} holds no number to set a slider to")),
                    };
                    if result.is_ok() {
                        self.walked(typed_identity(&field.identity, &text));
                        self.worked_here = true;
                    }
                    let note = result
                        .as_ref()
                        .ok()
                        .and_then(|done| done.notes.first().cloned())
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default();
                    self.steps.push(Step {
                        action: format!("set {} to {text:?}{note}", field.label),
                        confidence,
                        url,
                        error: result.err(),
                    });
                    return self
                        .memory
                        .looping()
                        .map(|steps| Outcome::Looping { steps });
                }
                let mut chose = None;
                let mut unmatched = None;
                let mut held_enter = false;
                // A field that stays empty took nothing: a failed step, not text typed.
                let filled = match workspace.tab().fill(field.reference, &text).await {
                    Ok(done) if done.notes.iter().any(|n| n == FIELD_STAYED_EMPTY) => Err(
                        aibrowse_browser::BrowserError::Element(FIELD_STAYED_EMPTY.to_owned()),
                    ),
                    filled => filled,
                };
                let typed = match filled {
                    Ok(filled) => {
                        let suggestions = if field.label.starts_with("combobox") {
                            matching_suggestion(workspace.tab(), &text).await
                        } else {
                            Suggestions::None
                        };
                        match suggestions {
                            Suggestions::Match(option, name) => {
                                chose = Some(name);
                                workspace.tab().click(option).await
                            }
                            Suggestions::Other(told) => {
                                unmatched = Some(told);
                                Ok(filled)
                            }
                            Suggestions::None if self.more_to_fill(&field.label, &text) => {
                                held_enter = true;
                                Ok(filled)
                            }
                            Suggestions::None => workspace.tab().submit_field().await,
                        }
                    }
                    // A field that took nothing won't take it again.
                    Err(err) if err.to_string() == FIELD_STAYED_EMPTY => Err(err),
                    Err(err) => {
                        // Nothing was typed (the field was covered, gone): the text is
                        // still to be typed, here once the cover is dismissed, or elsewhere.
                        let identity = typed_identity(&field.identity, &text);
                        let key = self.typed_key(&field.label, &identity);
                        self.typed.retain(|t| *t != key);
                        self.retry_later(&identity);
                        Err(err)
                    }
                };
                if let Ok(done) = &typed {
                    // A search: typing that led to another page (its results). Filling a
                    // form's field leads nowhere yet, and its next steps (a date, the
                    // form's button) aren't a search that came up short: on SJ the
                    // lookup ran instead of the button, two steps after the stations.
                    if moved(done) {
                        self.typed_at = Some(self.steps.len());
                    }
                    let identity = typed_identity(&field.identity, &text);
                    self.walked(identity.clone());
                    let point = ChoicePoint {
                        place: self.here.1.clone(),
                        identity,
                        label: format!("typing {text:?} into {}", field.label),
                        opener: None,
                    };
                    choice = Some((point, done.navigated));
                }
                let at = if field.identity.ends_with("\"\"") {
                    self.fields_then(&field.identity)
                } else {
                    String::new()
                };
                let took_back = typed
                    .as_ref()
                    .ok()
                    .and_then(|done| done.notes.first())
                    .is_some_and(|n| n.starts_with("Enter starts a new line"));
                // Text that stays in a form (a suggestion chosen, a list left open, an
                // editor's text) is the page being worked on. A plain Enter that left the
                // page where it was is a search shown in place, as likely as a form: npm
                // docs' search box (a combobox) lists its results under it, and the site
                // lookup must stay possible after it.
                // A suggestion that led to results (Microsoft's search) is a search. A
                // plain text box that kept the page where it was after Enter is a form's
                // (OSM's route ends, Sixt's pickup address): a search field is a search
                // box or a combobox.
                let stayed = typed
                    .as_ref()
                    .is_ok_and(|done| !moved(done) || narrowed(done));
                let form_field = field.label.starts_with("textbox");
                if stayed
                    && (chose.is_some()
                        || unmatched.is_some()
                        || took_back
                        || held_enter
                        || form_field)
                {
                    self.worked_here = true;
                }
                let then = match (&chose, &unmatched) {
                    (Some(name), _) => format!("chose the suggestion {name:?}"),
                    (None, Some(told)) => {
                        format!("left its suggestions open: none names it ({told})")
                    }
                    (None, None) if held_enter => {
                        "left Enter for later (the form has more to fill in)".to_owned()
                    }
                    (None, None) if took_back => {
                        "left it there (Enter only adds a line in it)".to_owned()
                    }
                    (None, None) => "pressed Enter".to_owned(),
                };
                (
                    format!("typed {text:?} into {}{at} and {then}", field.label),
                    confidence,
                    typed.map(drop),
                )
            }
            Next::Press { chord, confidence } => {
                let (keys, what) = key_parts(&chord);
                let result = workspace.tab().press(keys).await;
                if result.is_ok() {
                    self.walked(pressed_identity(&chord));
                }
                let at = self.fields_then("");
                let step = match what {
                    Some(what) => format!("pressed {keys} ({what}){at}"),
                    None => format!("pressed {keys}{at}"),
                };
                (step, confidence, result.map(drop))
            }
            Next::Scroll { confidence } => {
                let (here, state_id) = (self.here.1.clone(), self.here.2);
                let before = workspace.tab().viewport().await.map(|v| v.scroll_y).ok();
                let result = workspace.tab().scroll(0.0, SCROLL_STEP).await.map(drop);
                let after = workspace.tab().viewport().await.map(|v| v.scroll_y).ok();
                if before == after {
                    self.scrolled_out.push((here, state_id));
                }
                ("scrolled down".to_owned(), confidence, result)
            }
            Next::Click {
                candidate,
                confidence,
            } => {
                if candidate.is_link {
                    link_from = Some(self.here.1.clone());
                }
                let (result, step) = operate(workspace.tab(), &candidate).await;
                if result.is_ok()
                    && let Some(undo) = &candidate.undo
                {
                    self.undos.push(undo.clone());
                }
                if let Ok(done) = &result {
                    self.walked(candidate.identity.clone());
                    choice = Some((self.choice_point(&candidate), moved(done)));
                    // An option chosen or a filter switched (it has an undo): the
                    // listing is being narrowed where it is. A suggestion clicked in a
                    // list under a field, the page staying: a form being filled in.
                    // A checkbox, radio, switch or option is a setting, wherever it
                    // leads (a filter that reloads the listing with its query), and so is
                    // a step that only changed the address's query.
                    let suggestion = candidate.label.starts_with("option") && !moved(done);
                    let setting = SETTING_ROLES
                        .iter()
                        .any(|role| candidate.label.starts_with(role));
                    // On a listing, a click that opens a panel in place (its filters)
                    // is work on it; any other click there is navigating as usual (a
                    // page typed a listing for its many links, Apple's warranty pages).
                    let opens = ["(expands)", "(opens a dialog", "(opens a menu"]
                        .iter()
                        .any(|o| candidate.label.contains(o));
                    let on_listing = self.listing && !moved(done) && opens;
                    if candidate.option.is_some()
                        || candidate.undo.is_some()
                        || suggestion
                        || setting
                        || on_listing
                        || narrowed(done)
                    {
                        self.worked_here = true;
                    }
                } else {
                    self.retry_later(&candidate.identity);
                }
                (step, confidence, result.map(drop))
            }
        };
        // A link may open in a new tab; carry on there, as a person would.
        let opened = workspace.follow_new_tabs().await;
        // A click that led to another page, here or in a new tab, can be gone back on;
        // one that only changed this page (a menu, a tab panel) can't.
        if let Some((mut point, navigated)) = choice {
            if !opened.is_empty() {
                point.opener = Some(tab_before);
                self.choices.push(point);
            } else if navigated {
                self.choices.push(point);
            }
        }
        // A link that leaves the page where it was (no new page, no new tab) didn't do
        // what it promised; Jev must not take the page for its destination.
        let result = match (result, link_from) {
            (Ok(()), Some(from)) if opened.is_empty() && place(&workspace.tab().url()) == from => {
                Err("the page didn't change".to_owned())
            }
            (result, _) => result.map_err(|e| e.to_string()),
        };
        let description = if opened.is_empty() {
            description
        } else {
            format!("{description} (opened in a new tab)")
        };
        self.tab_notes.extend(opened);
        self.steps.push(Step {
            action: description,
            confidence,
            url,
            error: result.err(),
        });
        self.memory
            .looping()
            .map(|steps| Outcome::Looping { steps })
    }

    /// What `typed` remembers a text typed into a field by. A search box by the field
    /// alone: the same search from anywhere gives the same results. Any other field by
    /// the page too: a journey planner's "From" on the next page (its own planner, after a
    /// shortcut form on the home page) is empty again and takes the same station.
    fn typed_key(&self, field_label: &str, identity: &str) -> String {
        if field_label.starts_with("searchbox") {
            identity.to_owned()
        } else {
            format!("{} {identity}", self.here.1)
        }
    }

    /// Whether Enter after typing `text` into a form's text box would submit the form
    /// before the rest is filled in: other texts are still to be typed and the page has
    /// fields still empty. mobile.de's search ran with the model alone, the year and the
    /// price lost. A search box or combobox takes Enter as before.
    fn more_to_fill(&self, field_label: &str, text: &str) -> bool {
        let form_box = ["textbox", "spinbutton"]
            .iter()
            .any(|role| field_label.starts_with(role));
        let untyped = self.texts.iter().any(|t| {
            t != text
                && !self
                    .typed
                    .iter()
                    .any(|k| k.contains(&format!("type {t:?} into")))
        });
        // Named fields only: a spreadsheet's unnamed formula bar is no rest of a form.
        let empty_fields = self
            .fields
            .iter()
            .filter(|f| {
                !f.contains("(value: ") && !f.starts_with(field_label) && !f.ends_with("\"\"")
            })
            .count();
        // A part of a task is one step as asked ("Select C3 to C10" in the Name Box,
        // Enter included); the parts after it are their own.
        let one_part = self.limits.part_of.is_some();
        form_box && untyped && empty_fields > 0 && !one_part
    }

    /// Links (absolute addresses) not to offer on the page this pursuit is on now: a
    /// search branch's pagination (fanout.rs). Replaced on every page.
    pub fn avoid_links(&mut self, links: Vec<String>) {
        self.avoided = links;
    }

    /// Records a step taken toward the goal, for the path learned when it is reached,
    /// with the place it was taken from.
    fn walked(&mut self, identity: String) {
        self.path.push(identity);
        self.path_from.push(self.here.1.clone());
    }

    /// Cuts a detour out of the path: arriving at a place a step of it was taken from,
    /// after steps taken elsewhere, those steps led back here and are no part of the way
    /// (npm's "Versions" tab, then "Readme" back, learned whole and replayed every time).
    /// Steps taken on this very page (a form filled, filters set) stay.
    fn cut_detour(&mut self, here: &str) {
        let came_back = self.path_from.last().is_some_and(|last| last != here);
        if !came_back {
            return;
        }
        if let Some(first) = self.path_from.iter().position(|from| from == here) {
            self.path.truncate(first);
            self.path_from.truncate(first);
        }
    }

    /// A step that failed before doing anything (a cookie dialog that came up late
    /// covered it) is offered again once, rather than counting as tried: after the
    /// dialog is dismissed the page is back in the state it was taken from, where it
    /// would otherwise never be taken again. Only once, so a step that can't be done
    /// doesn't come round forever.
    fn retry_later(&mut self, identity: &str) {
        // A few in all: under an overlay that stays (Platsbanken's, Zillow's), every
        // element it covers failed and came round again, 3 to 5 clicks in a row.
        if self.retried.len() >= MAX_RETRIES || self.retried.iter().any(|r| r == identity) {
            return;
        }
        self.retried.push(identity.to_owned());
        let (here, state_id) = (self.here.1.clone(), self.here.2);
        self.memory.forget(&here, state_id, identity);
    }

    /// The fields holding a value when a step was taken, other than `acted_on`, as
    /// " (while: …)". Where a step without a name of its own acted (a key press, an
    /// unnamed field) is otherwise lost: on a canvas spreadsheet, "typed \"1\" into
    /// textbox \"\"" never said the Name Box read A1, and the part wasn't seen as done.
    fn fields_then(&self, acted_on: &str) -> String {
        let held: Vec<&str> = self
            .fields
            .iter()
            .filter(|f| f.contains("(value: ") && (acted_on.is_empty() || !f.starts_with(acted_on)))
            .take(FIELDS_THEN)
            .map(String::as_str)
            .collect();
        if held.is_empty() {
            String::new()
        } else {
            format!(" (while: {})", held.join(", "))
        }
    }

    /// Links that branches explored and gave up on: not offered again on this page.
    pub fn mark_tried(&mut self, links: &[Link]) {
        let here = self.here.1.clone();
        for link in links {
            self.memory.reject(&here, &link.identity);
        }
    }

    /// One of the agent's suggested texts for `field`, when Jev picked one that fits, it
    /// hasn't led nowhere in this field before, and typing it (and Enter) wouldn't
    /// commit anything the goal doesn't. `None` hands back for the agent to write.
    async fn typing(
        &mut self,
        field: &Candidate,
        answers: &Value,
        tab: &mut Tab,
        jev: &Jev,
        here: &str,
        state_id: u64,
    ) -> Result<Option<Next>, JevError> {
        if self.texts.is_empty() {
            return Ok(None);
        }
        // With several texts, the request also asked which fits each field by name
        // (`text_for_fields`): the field-blind question can't tell which text a field
        // takes (in Google Sheets it put "=A2*3" in the Name Box, at 0.69).
        let per_field = format!("{TEXT_FOR}{}", field.key);
        let (mut key, mut confidence) = if answers.get(&per_field).is_some() {
            choice(answers, &per_field)
        } else {
            choice(answers, "text")
        };
        // A field past those asked about by name is asked about again, named.
        if confidence < TEXT_MIN && self.texts.len() > 1 && answers.get(&per_field).is_none() {
            (key, confidence) = self.text_for(field, jev).await?;
        }
        let text = key
            .strip_prefix('t')
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| self.texts.get(n));
        if let Some(decision) = self.trail.last_mut() {
            decision.text = Some((
                field.label.clone(),
                text.cloned().unwrap_or(key.clone()),
                confidence,
            ));
        }
        let Some(text) = text else {
            return Ok(None);
        };
        if confidence < TEXT_MIN {
            return Ok(None);
        }
        let identity = typed_identity(&field.identity, text);
        let key = self.typed_key(&field.label, &identity);
        if self.typed.contains(&key) || !self.memory.allows(here, state_id, &identity) {
            return Ok(None);
        }
        // Enter in a field submits its form: a search, or a message. In a search box it
        // runs a search, which commits nothing, whatever the words ("freeze card"), and
        // a slider takes no Enter: it filters or sorts.
        let search_box = field.label.starts_with("searchbox") || field.is_range;
        if !search_box && let Ok(context) = tab.action_context(field.reference).await {
            self.decisions += 1;
            let action = format!("type {text:?} into, then press Enter in,");
            let p = consequential(jev, &action, &context, self.limits.agreements).await?;
            if p >= CONSEQUENTIAL {
                tracing::debug!(field = %field.label, consequential = p, "typing would commit something");
                return Ok(None);
            }
        }
        self.memory.record(here, state_id, &identity);
        self.typed.push(key);
        Ok(Some(Next::Type {
            field: chosen(field),
            text: text.clone(),
            confidence,
        }))
    }

    /// How likely the page's own text says it is what the goal asks for, judged once per
    /// place and text. A page without readable text can't be checked (`None`), unless its
    /// text was read before (a dialog now hides it): that verdict stands.
    async fn fulfils(
        &mut self,
        tab: &mut Tab,
        jev: &Jev,
        here: &str,
    ) -> Result<Option<f64>, JevError> {
        let first = main_text(tab).await;
        let failed = first.is_err();
        let mut text = first.unwrap_or_else(|e| {
            tracing::warn!(url = %tab.url(), error = %e, "read failed: page taken as empty");
            String::new()
        });
        // A page still rendering its content reads as its banner alone (NVD's CVE page:
        // the .gov notice, then CVSS 10.0 and the versions seconds later), and a check of
        // that vetoed the right page. Thin text is read again while it grows; a read
        // that failed (timed out on a heavy page) isn't tried again, each try 30 s.
        for _ in 0..if failed { 0 } else { THIN_POLLS } {
            if text.chars().count() >= THIN_TEXT {
                break;
            }
            tokio::time::sleep(THIN_WAIT).await;
            let again = main_text(tab).await.unwrap_or_default();
            if again.len() <= text.len() {
                break;
            }
            text = again;
        }
        let host = url::Url::parse(&self.here.0)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        let text = about(&text, &content_words(self.goal, &host), VERIFY_CHARS);
        if text.trim().is_empty() {
            return Ok(self
                .checked
                .iter()
                .filter(|(place, _, _)| place == here)
                .map(|(_, _, p)| *p)
                .reduce(f64::min));
        }
        let read = {
            let mut hasher = std::hash::DefaultHasher::new();
            std::hash::Hash::hash(&text, &mut hasher);
            std::hash::Hasher::finish(&hasher)
        };
        if let Some((_, _, p)) = self
            .checked
            .iter()
            .find(|(place, hash, _)| place == here && *hash == read)
        {
            return Ok(Some(*p));
        }
        let mut state = json!({
            "goal": self.goal,
            "today": crate::today::today(),
            "page": { "title": self.title, "url": self.here.0, "text": text },
        });
        // What the page's fields are set to: results for another date, a round trip
        // for a one-way, a car route for walking directions read as the page asked for
        // when only its text was judged.
        if !self.fields.is_empty() {
            state["page"]["fields"] = json!(self.fields);
        }
        let mut questions = json!({
            "fulfils": {
                "type": "noul",
                "instructions": "Is this page what `goal` asks for, judging by its text and its fields?",
                "criteria": {
                    "true": "Yes: the page is that page, or holds exactly what `goal` asks for, or is the page where it would be and shows there is none (a release whose notes list no breaking changes)",
                    "false": "No: it is a different or more general page (a related topic, an overview, a list leading elsewhere)"
                }
            }
        });
        // A part of a task is often an action, done rather than found ("select C3 to
        // C10"): asked whether the page is what it asks for, Jev hesitated (0.25), as
        // the page's text doesn't show a selection. Its fields' values and the steps
        // taken do.
        if let Some(part_of) = &self.limits.part_of {
            // The parts before this one left the page as it is: without them, the
            // category page the last part reached read as "its second page" (0.64).
            state["parts_done_before"] = json!(part_of.before);
            state["steps_taken"] = json!(
                self.steps
                    .iter()
                    .map(|s| s.action.clone())
                    .collect::<Vec<_>>()
            );
            questions["fulfils"] = json!({
                "type": "noul",
                "instructions": "Is what `goal` asks for done, or is this the page it asks for, judging by the page's text, its fields' values and the `steps_taken`? `parts_done_before` were done before it and left the page where `goal` starts; `goal` asks for something beyond them, done or not.",
                "criteria": {
                    "true": "Yes: what `goal` asks to do has been done here (the fields or text show it, or a step taken did exactly that), or the page is what `goal` asks for, or the page where it would be shows there is none",
                    "false": "No: it isn't done yet, or the page is a different one"
                }
            });
        }
        // Whether the page is set to something else than asked, on its own: folded
        // into the question above it either vetoed pages describing other things than
        // the goal (kernel.org's other kinds of release, 0.5) or, in a part of a task,
        // was replaced with the part's own question. A clear yes vetoes at any floor.
        // In a part, what the parts before it set counts too: Trenitalia's search ran
        // for today, though part 1 was to set 20 November.
        questions["contradicts"] = json!({
            "type": "noul",
            "instructions": "Is this page set to something `goal` rules out (or, if given, what `parts_done_before` set)? Look at its fields' values and at what its results are for.",
            "criteria": {
                "true": "Yes: its fields or its results are for another date, place, route, trip type, mode of travel, quantity, variant, brand, size, price range or sort order than stated (results for today where a date was given, a round trip for a one-way, a car route for walking, another brand's listing, sorted by relevance where cheapest first was asked)",
                "false": "No: nothing it is set to contradicts what was stated, or the page isn't set to anything (an article, a document, a list)"
            }
        });
        // Pages that look like the goal's but don't hold it yet, on their own too: in
        // the question above they cost a borderline page its yes (kernel.org's releases,
        // 0.64 → 0.41). A product page for a listing (a survey's "hybrid bikes" reached
        // on one bike), a calculator before its answer (INPS's simulator).
        questions["short_of"] = json!({
            "type": "noul",
            "instructions": "Does this page fall short of `goal` in one of these ways?",
            "criteria": {
                "true": "Yes: it is a single item (one product, one listing, one car) where `goal` asks for a list or listing of such items, or `goal` asks for a result (an amount, a price, a verdict) and the page is the form or calculator that would give it, with the result still to come. A goal that asks for the form, tool or page to do something on is not short of it, nor is one document (release notes, an article) for what is listed in it, or shown not to be",
                "false": "No: neither of those"
            }
        });
        // A limit or filter the goal states that the page doesn't apply: left at "Any"
        // ("Erstzulassung von: Beliebig", 27,174 cars where 2019 or newer at most 200 000
        // was asked), which is no contradiction, being no other value.
        questions["unmet"] = json!({
            "type": "noul",
            "instructions": "Does `goal` state a limit, filter or criterion (a year, a price, a number of rooms, a date, a brand, a category) that this page doesn't apply? Look at its address, its fields' values and its results.",
            "criteria": {
                "true": "Yes: a criterion `goal` states is not applied: its field is still empty or at \"any\", an option for it was chosen but not applied (its panel's apply or update button not pressed, the results unchanged), or a category `goal` names isn't the one shown, and neither the address nor the results show it",
                "false": "No: every criterion `goal` states is applied, or `goal` states none"
            }
        });
        // Data from an earlier period than a "current", "latest" or "today" goal asks for:
        // a web lookup found last season's league table for "the current table" (0.92).
        questions["stale"] = json!({
            "type": "noul",
            "instructions": "Given `today`, does this page show data from an earlier period than `goal` asks for (the current, latest or today's)?",
            "criteria": {
                "true": "Yes: it is an earlier season's, year's, edition's or day's than the one `goal` means (2024/25 for the current table in 2026), or an old version where the latest was asked",
                "false": "No: it is the current period's data, or `goal` doesn't ask for a period"
            }
        });
        // Whether the goal asks for a period at all, on its own: asked only whether the
        // page's data is from an earlier one, Jev said 0.5 of japan-guide's Rail Pass
        // article, which no goal dated.
        questions["asks_current"] = json!({
            "type": "noul",
            "instructions": "Does `goal` ask for the current, latest, today's or a dated period's data (this season's table, today's rate, the latest version)?",
            "criteria": {
                "true": "Yes: it asks for the current or latest, or a date or period",
                "false": "No: it asks for a page or a fact without a period"
            }
        });
        let answers = jev.ask(&state, &questions).await?;
        self.decisions += 1;
        let mut p = answers["fulfils"]["noul"].as_f64().unwrap_or(1.0);
        let raw = |name: &str| answers[name]["noul"].as_f64().unwrap_or(0.0);
        let noul = |name: &str| match name {
            "stale" if raw("asks_current") < CONTRADICTS => 0.0,
            _ => raw(name),
        };
        tracing::debug!(
            place = here,
            fulfils = p,
            contradicts = noul("contradicts"),
            short_of = noul("short_of"),
            unmet = noul("unmet"),
            stale = noul("stale"),
            "reached check"
        );
        // The first clear reason the page isn't it, told with the outcome: an agent
        // handed back "unsure between scroll and a textbox" on ECB's page of yesterday's
        // rate could not tell that today's isn't published yet.
        // The surest one says why: the first in the list told Rightmove's missing price
        // limit as "set to something else".
        let veto = VETOES
            .iter()
            .filter(|(name, _)| noul(name) >= CONTRADICTS)
            .max_by(|a, b| noul(a.0).total_cmp(&noul(b.0)))
            .map(|(_, why)| *why);
        if let Some(why) = veto {
            p = 0.0;
            let note = format!("{} looked like the goal, but {why}", self.here.0);
            if !self.tab_notes.contains(&note) {
                self.tab_notes.push(note);
            }
        }
        if let Some(decision) = self.trail.last_mut() {
            decision.text_check = Some(p);
        }
        self.checked.push((here.to_owned(), read, p));
        Ok(Some(p))
    }

    /// What `go_back` would mean here, for Jev, or `None` when there is nowhere to go
    /// back to. In a branch, going back from where it started abandons the branch, and
    /// the page it was opened from decides again.
    fn back_option(&self) -> Option<String> {
        match self.choices.last() {
            Some(c) => Some(format!(
                "Go back to {} and choose again: the step taken there ({}) led the wrong way",
                c.place, c.label
            )),
            None if self.branch => {
                Some("Go back: this page doesn't lead toward `goal`, and nothing on it does".into())
            }
            None => None,
        }
    }

    fn choice_point(&self, candidate: &Chosen) -> ChoicePoint {
        ChoicePoint {
            place: self.here.1.clone(),
            identity: candidate.identity.clone(),
            label: candidate.label.clone(),
            opener: None,
        }
    }

    /// Returns to the last choice point (browser back, or closing the tab the step
    /// opened, which is how one goes back from a new tab) and marks the step taken
    /// there a dead end. The next `think` decides again on that page.
    async fn go_back<W: Workspace>(
        &mut self,
        workspace: &mut W,
        confidence: f64,
    ) -> Option<Outcome> {
        let Some(choice) = self.choices.pop() else {
            // A branch going back from where it started: its parent decides again.
            return Some(Outcome::DeadEnd);
        };
        let url = self.here.0.clone();
        let result = match &choice.opener {
            Some(opener) => {
                if workspace.back_to(opener).await {
                    Ok(())
                } else {
                    Err("could not return to the previous tab".to_owned())
                }
            }
            None => workspace
                .tab()
                .back()
                .await
                .map(drop)
                .map_err(|e| e.to_string()),
        };
        self.memory.reject(&choice.place, &choice.identity);
        if self.path.last() == Some(&choice.identity) {
            self.path.pop();
            self.path_from.pop();
        }
        self.memory.record(&self.here.1, self.here.2, GO_BACK);
        self.steps.push(Step {
            action: format!("went back: {} led the wrong way", choice.label),
            confidence,
            url,
            error: result.err(),
        });
        self.memory
            .looping()
            .map(|steps| Outcome::Looping { steps })
    }

    /// Which suggested text goes into `field`, asked with the field named.
    async fn text_for(&mut self, field: &Candidate, jev: &Jev) -> Result<(String, f64), JevError> {
        let state = json!({
            "goal": self.goal,
            "field": field.label,
            "page": format!("{} ({})", self.title, self.here.0),
            "steps_taken": self.steps.iter().rev().take(6).rev().map(|s| s.action.clone()).collect::<Vec<_>>(),
        });
        let questions = json!({ "text": text_question(self.texts) });
        let answers = jev.ask(&state, &questions).await?;
        self.decisions += 1;
        Ok(choice(&answers, "text"))
    }

    /// The offered key or chord an option key stands for, if it is one.
    fn offered_key(&self, key: &str) -> Option<&str> {
        let n: usize = key.strip_prefix(PRESS)?.parse().ok()?;
        self.limits.keys.get(n).map(String::as_str)
    }

    /// Whether pressing `chord` would commit something on the user's behalf, judged on
    /// the focused element (or the page, when nothing has focus).
    async fn press_commits(
        &mut self,
        tab: &mut Tab,
        jev: &Jev,
        chord: &str,
    ) -> Result<bool, JevError> {
        let context = match tab.focused_context().await {
            Ok(Some(context)) => context,
            Ok(None) => aibrowse_browser::ActionContext {
                url: tab.url(),
                title: tab.title().await,
                element: "the page".to_owned(),
                surrounding: String::new(),
                entered: String::new(),
            },
            // Nothing to judge it by: not pressed on the user's behalf.
            Err(_) => return Ok(true),
        };
        self.decisions += 1;
        let action = match key_parts(chord) {
            (keys, Some(what)) => format!("press {keys} ({what}) in"),
            (keys, None) => format!("press {keys} in"),
        };
        Ok(consequential(jev, &action, &context, self.limits.agreements).await? >= CONSEQUENTIAL)
    }

    /// Records a step taken on the pursuit's behalf (a fan-out, or a branch's steps).
    /// Whether clicking `element` commits nothing on the user's behalf (judge.rs).
    async fn commits_nothing(
        &mut self,
        tab: &mut Tab,
        jev: &Jev,
        element: ElementRef,
    ) -> Result<bool, JevError> {
        let Ok(context) = tab.action_context(element).await else {
            return Ok(false);
        };
        self.decisions += 1;
        Ok(consequential(jev, "click", &context, self.limits.agreements).await? < CONSEQUENTIAL)
    }

    /// Whether clicking a consent prompt's declining button commits nothing else: the
    /// full check (`commits_nothing`), or with `sure` only whether it does something
    /// besides declining.
    async fn declines_only(
        &mut self,
        tab: &mut Tab,
        jev: &Jev,
        element: ElementRef,
        sure: bool,
    ) -> Result<bool, JevError> {
        if !sure {
            return self.commits_nothing(tab, jev, element).await;
        }
        let Ok(context) = tab.action_context(element).await else {
            return Ok(false);
        };
        self.decisions += 1;
        Ok(declining_commits(jev, &context).await? < 0.5)
    }

    /// The place the pursuit arrived at its current page from.
    pub fn came_from(&self) -> Option<&str> {
        self.came_from.as_deref()
    }

    /// Whether the page the pursuit ended on is a listing or search results.
    pub fn on_listing(&self) -> bool {
        self.listing
    }

    /// Whether the goal is being worked on the page rather than found elsewhere: a form
    /// filled in (text that stayed in its field, not a search that led to results), an
    /// option chosen, a filter switched or a slider set. A lookup would only leave it.
    pub fn worked_here(&self) -> bool {
        self.worked_here
    }

    /// Steps taken since text was last typed (a site search and Enter), if any was.
    pub fn steps_since_typing(&self) -> Option<usize> {
        // Only steps that did something toward the goal: a cookie declined or a click
        // that failed (covered) sent StepStone's half-filtered listing to the lookup.
        self.typed_at.map(|at| {
            (at + 1..self.steps.len())
                .filter(|i| !self.chores.contains(i) && self.steps[*i].error.is_none())
                .count()
        })
    }

    pub fn push_step(&mut self, action: String, confidence: f64) {
        self.steps.push(Step {
            action,
            confidence,
            url: self.here.0.clone(),
            error: None,
        });
    }
}

/// Typing `text` into a field, as a step: remembered and gone back on like a click.
/// The page's text clearly is the goal: enough to take a page without Jev's "reached".
/// Text that couldn't be read confirms nothing.
fn confirmed(text_check: Option<f64>) -> bool {
    text_check.is_some_and(|p| p >= CONFIRM_MIN)
}

/// The number in a text such as "5000", "5 000 kr" or "4.5": its digits, spaces
/// between them dropped, and a decimal point.
fn number_in(text: &str) -> Option<f64> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ' ' || *c == '\u{a0}')
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

/// An offered key split into the chord and, when the agent said, what it does:
/// "Meta+d: fill the selection down".
fn key_parts(offered: &str) -> (&str, Option<&str>) {
    match offered.split_once(": ") {
        Some((keys, what)) if !keys.trim().is_empty() => (keys.trim(), Some(what.trim())),
        _ => (offered.trim(), None),
    }
}

fn pressed_identity(chord: &str) -> String {
    format!("press {chord}")
}

fn typed_identity(field: &str, text: &str) -> String {
    format!("type {text:?} into {field}")
}

fn chosen(candidate: &Candidate) -> Chosen {
    Chosen {
        reference: candidate.reference,
        label: candidate.label.clone(),
        identity: candidate.identity.clone(),
        is_link: candidate.is_link,
        is_range: candidate.is_range,
        option: candidate.option.clone(),
        undo: candidate.undo.clone(),
    }
}

/// Whether a link to `target`, on the page at `here` reached from the place `from`,
/// undoes that last move: it leads back to `from`, or to this same page without a query
/// parameter the move added (by name). On eBay the "Buy It Now" filter, once on, is a link to the
/// listing without `LH_BIN=1` (and with other parameters besides): clicked again as if
/// it applied the filter, it took it off.
fn undoes(target: &url::Url, here: &url::Url, from: &str) -> bool {
    if place(target.as_str()) == from {
        return true;
    }
    let Ok(before) = url::Url::parse(from) else {
        return false;
    };
    if target.path() != here.path() || before.path() != here.path() {
        return false;
    }
    // By name: the next page of a paged list changes `p=2` to `p=3`, which drops nothing.
    let names = |url: &url::Url| -> Vec<String> {
        url.query_pairs().map(|(k, _)| k.into_owned()).collect()
    };
    let (now, then, there) = (names(here), names(&before), names(target));
    now.iter()
        .filter(|name| !then.contains(name))
        .any(|added| !there.contains(added))
}

/// After typing into a field that suggests as you type (a combobox), the suggestion the
/// page shows for the text, if one matches it: the same text, else one that starts
/// with it. Chosen by a click as a person does, not by Enter, which on many such fields
/// chooses nothing: SJ's station fields kept "Stockholm Central" as typed text and the
/// search didn't go through.
async fn matching_suggestion(tab: &mut Tab, text: &str) -> Suggestions {
    let Ok(model) = observe(tab, Detail::Interactive, None).await else {
        return Suggestions::None;
    };
    let squash = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let wanted = squash(text);
    let options: Vec<(ElementRef, String)> = model
        .nodes
        .iter()
        .filter(|n| n.role == "option")
        .filter(|n| n.position.is_none_or(|p| p == Position::Visible))
        .filter_map(|n| Some((n.reference?, n.name.clone())))
        .collect();
    let found = options
        .iter()
        .find(|(_, name)| squash(name) == wanted)
        .or_else(|| {
            options
                .iter()
                .find(|(_, name)| squash(name).starts_with(&wanted))
        })
        // Every word of the text, in a longer name ("Berlin, Berlin, Germany" for
        // "Berlin, Germany"): the shortest such suggestion, the least added to it.
        .or_else(|| {
            let words = |s: &str| -> Vec<String> {
                s.split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(str::to_lowercase)
                    .collect()
            };
            let wanted = words(text);
            options
                .iter()
                // Starting as the text does: "Mitte, Berlin, Germany" holds every word
                // of "Berlin, Germany" too, and is another place.
                .filter(|(_, name)| {
                    let have = words(name);
                    !wanted.is_empty()
                        && have.first() == wanted.first()
                        && wanted.iter().all(|w| have.contains(w))
                })
                .min_by_key(|(_, name)| name.len())
        });
    match found {
        Some((option, name)) => Suggestions::Match(*option, name.clone()),
        None if options.is_empty() => Suggestions::None,
        None => Suggestions::Other(
            options
                .iter()
                .take(SUGGESTIONS_TOLD)
                .map(|(_, name)| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// What a combobox suggests for the text typed into it.
enum Suggestions {
    /// No list of suggestions.
    None,
    /// The suggestion naming the text.
    Match(ElementRef, String),
    /// Suggestions, none naming the text (the first few, quoted): Enter would choose
    /// one of them anyway (SL's "Arlanda" took "Bromma flygplats"), so the list is left
    /// open for the next step to choose from, or the agent.
    Other(String),
}

/// Suggestions named in the step when none matches.
const SUGGESTIONS_TOLD: usize = 5;

/// Roles whose click sets something on the page (a filter, a choice) rather than going
/// somewhere.
const SETTING_ROLES: [&str; 6] = [
    "checkbox",
    "radio",
    "switch",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
];

/// Whether a step only changed the address's query on the same path: a listing narrowed
/// (a price typed and Enter, a filter that reloads with `?brand=…`), not another page.
fn narrowed(done: &ActionResult) -> bool {
    let (Ok(before), Ok(after)) = (
        url::Url::parse(&done.url_before),
        url::Url::parse(&done.url_after),
    ) else {
        return false;
    };
    before.host_str() == after.host_str()
        && before.path() == after.path()
        && before.query() != after.query()
}

/// Whether a step took the tab to another page, so going back undoes it: a new
/// document, or another address in the same one (a single-page app's routing: on
/// PriceRunner a product link only changed the address, the product page was gone, and
/// with nothing to go back to the navigator went to the home page instead of back to
/// the results).
fn moved(done: &ActionResult) -> bool {
    done.navigated || place(&done.url_after) != place(&done.url_before)
}

/// Clicks the candidate, or chooses its option in a native `<select>`; returns what
/// happened and the step as the trail tells it.
async fn operate(
    tab: &mut Tab,
    candidate: &Chosen,
) -> (aibrowse_browser::Result<ActionResult>, String) {
    match &candidate.option {
        Some(option) => (
            tab.select(candidate.reference, option).await,
            format!("chose {}", candidate.label),
        ),
        None => (
            tab.click(candidate.reference).await,
            format!("clicked {}", candidate.label),
        ),
    }
}

fn by_key<'c>(options: &'c [Candidate], key: &str) -> Option<&'c Candidate> {
    options.iter().find(|c| c.key == key)
}

/// The best untried element to try when nothing is convincing, if any is at all likely.
fn best_element(untried: &[(String, f64)]) -> Option<(String, f64)> {
    untried
        .iter()
        .find(|(key, p)| key != SCROLL && key != GO_BACK && *p >= FAN_OUT_MIN)
        .cloned()
}

/// The untried links worth opening side by side, best first.
fn fan_out_links(untried: &[(String, f64)], options: &[Candidate]) -> Vec<Link> {
    untried
        .iter()
        .filter(|(_, p)| *p >= FAN_OUT_MIN)
        .filter_map(|(key, p)| {
            let c = by_key(options, key).filter(|c| c.is_link)?;
            Some(Link {
                open: Opening::Click(c.reference),
                label: c.label.clone(),
                identity: c.identity.clone(),
                confidence: *p,
                named_for_goal: false,
            })
        })
        .take(FAN_OUT)
        .collect()
}

/// The next action among untried ones, ranked by confidence, or `None` when nothing is
/// convincing enough. Scrolling isn't a rival to a concrete step: "scroll to the footer"
/// and "click the footer link" are the same plan, and the click gets there in one move
/// (act scrolls to off-screen elements itself). So the best element is judged against
/// the other elements, and scrolling is the choice only when no element is convincing.
fn pick(untried: &[(String, f64)], min_confidence: f64) -> Option<(String, f64)> {
    let mut elements = untried.iter().filter(|(key, _)| key != SCROLL);
    let best = elements.next();
    let runner_up = elements.next().map_or(0.0, |(_, p)| *p);
    if let Some((key, p)) = best
        && (*p >= min_confidence || (*p >= CLEAR_LEAD && *p >= LEAD_RATIO * runner_up))
    {
        return Some((key.clone(), *p));
    }
    untried
        .iter()
        .find(|(key, p)| key == SCROLL && *p >= min_confidence)
        .cloned()
}

pub(crate) fn failed_step(action: &str, url: &str, error: &str) -> Step {
    Step {
        action: action.to_owned(),
        confidence: 0.0,
        url: url.to_owned(),
        error: Some(error.to_owned()),
    }
}

fn state(goal: &str, outline: &str, steps: &[Step], tried_here: &[String]) -> Value {
    let outline: String = dialogs_first(outline)
        .chars()
        .take(MAX_OUTLINE_CHARS)
        .collect();
    let history: Vec<String> = steps
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|s| match &s.error {
            Some(err) => format!("{} on {} (failed: {err})", s.action, s.url),
            None => format!("{} on {}", s.action, s.url),
        })
        .collect();
    let mut state = json!({
        "goal": goal,
        "today": crate::today::today(),
        "page": outline,
        "steps_taken": history,
    });
    if !tried_here.is_empty() {
        // Steps from this page already taken and gone back on: they led nowhere.
        state["tried_here_led_nowhere"] = json!(tried_here);
    }
    state
}

/// The outline with its top-level dialogs moved up under the header, when it is too
/// long to send whole. A dialog covers the page (a cookie prompt, a sign-in modal), but
/// the outline lists it in page order, often last, after the footer, where cutting the
/// outline to size dropped it and Jev never saw what blocks the page.
fn dialogs_first(outline: &str) -> String {
    if outline.chars().count() <= MAX_OUTLINE_CHARS {
        return outline.to_owned();
    }
    let lines: Vec<&str> = outline.lines().collect();
    let body = lines
        .iter()
        .position(|l| l.starts_with("- "))
        .unwrap_or(lines.len());
    let (mut dialogs, mut rest): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let mut in_dialog = false;
    for line in &lines[body..] {
        if line.starts_with("- ") {
            in_dialog = line.starts_with("- dialog") || line.starts_with("- alertdialog");
        }
        if in_dialog {
            dialogs.push(line);
        } else {
            rest.push(line);
        }
    }
    lines[..body]
        .iter()
        .chain(&dialogs)
        .chain(&rest)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// All questions in one request: Jev answers them together for the cost of one. `back`
/// describes going back, when there is somewhere to go back to.
fn questions(
    options: &[Candidate],
    back: Option<&str>,
    texts: &[String],
    keys: &[String],
    can_scroll: bool,
) -> Value {
    let mut next = Map::new();
    for option in options {
        next.insert(option.key.clone(), Value::String(option.label.clone()));
    }
    for (n, chord) in keys.iter().enumerate() {
        next.insert(
            format!("{PRESS}{n}"),
            Value::String(match key_parts(chord) {
                (keys, Some(what)) => format!(
                    "Press {keys}: {what} (a keyboard shortcut, acting on what is selected or focused)"
                ),
                (keys, None) => format!(
                    "Press {keys} (a keyboard shortcut, acting on what is selected or focused)"
                ),
            }),
        );
    }
    if can_scroll {
        next.insert(
            SCROLL.to_owned(),
            Value::String("Scroll down: what is needed is probably further down this page".into()),
        );
    }
    if let Some(back) = back {
        next.insert(GO_BACK.to_owned(), Value::String(back.to_owned()));
    }
    // A choice needs a criterion (Jev refuses an empty one), and the turn question still
    // counts on a page with nothing left to act on (all tried, or still rendering): it
    // may be reached, or the user's. Chosen, it matches no step and the pursuit is stuck.
    if next.is_empty() {
        next.insert(
            NOTHING.to_owned(),
            Value::String("Nothing to act on is left on this page".into()),
        );
    }
    let mut fields = Map::new();
    for option in options.iter().filter(|c| c.is_field) {
        fields.insert(option.key.clone(), Value::String(option.label.clone()));
    }
    fields.insert(
        "none".into(),
        Value::String("No field needs to be filled in".into()),
    );
    // With shortcuts offered, pressing one is a step like a click; without, the wording
    // stays as every other goal has been judged by.
    let navigate = if keys.is_empty() {
        "Clicking a link or button on this page, or scrolling, gets closer to `goal` without typing anything"
    } else {
        "Clicking a link or button on this page, pressing one of the offered keyboard shortcuts, or scrolling, gets closer to `goal` without typing anything"
    };
    let turn = json!({
        "reached": "The current page already is what `goal` asks for; nothing more to do",
        "navigate": navigate,
        "user": "Only the human account owner can continue: signing in, a verification or 2FA code, a CAPTCHA, or confirming a payment",
        "agent": "Text must be typed (e.g. a search query, a message) or a decision made that depends on details only the requester knows",
        "stuck": "Nothing on this page helps toward `goal`"
    });

    let mut asked = json!({
        "turn": {
            "type": "choice",
            "instructions": "Given `goal`, the current `page` and the `steps_taken` so far, who should act next?",
            "criteria": turn
        },
        "next": {
            "type": "choice",
            "instructions": "Which single action on this page best advances `goal`? Prefer actions not already in `steps_taken`.",
            "criteria": Value::Object(next)
        },
        "field": {
            "type": "choice",
            "instructions": "If text must be entered to advance `goal`, which field?",
            "criteria": Value::Object(fields)
        },
        "text": text_question(texts),
        "goal_commits": {
            "type": "noul",
            "instructions": "Does reaching `goal` itself mean committing something on the person's behalf?",
            "criteria": {
                "true": "Yes: `goal` is to send, post, buy, pay, apply, subscribe, accept, delete or change a setting",
                "false": "No: `goal` is to find or open something: a page, an article, a list, a form to look at"
            }
        },
        "needs_account": {
            "type": "noul",
            "instructions": "Does reaching `goal` require being signed in to the person's own account?",
            "criteria": {
                "true": "Yes: `goal` is about the person's own data or account: their messages, settings, orders, billing, dashboard",
                "false": "No: `goal` is a public page anyone can see without signing in: help and support articles, pricing, terms, privacy, careers, docs"
            }
        },
        "user_task": {
            "type": "choice",
            "instructions": "If only the human can continue, what must they do?",
            "criteria": {
                "sign_in": "Sign in or create an account",
                "verify": "Enter a verification, 2FA or one-time code, or confirm on another device",
                "captcha": "Solve a CAPTCHA or prove they are human",
                "payment": "Confirm or enter payment details",
                "other": "Something else only they can do"
            }
        }
    });
    if let Value::Object(asked) = &mut asked {
        asked.extend(text_for_fields(options, texts));
    }
    asked
}

/// Prefix of the questions asking which text goes into one field, by its key.
const TEXT_FOR: &str = "text_for:";
/// Fields asked about by name in one request: the likeliest few (visible ones first);
/// any other is asked about separately if it is chosen.
const TEXT_FIELDS: usize = 8;

/// With several texts, which fits each field, asked per field with the field named.
/// One text fits wherever text is needed, so then the field-blind question suffices.
fn text_for_fields(options: &[Candidate], texts: &[String]) -> Map<String, Value> {
    let mut out = Map::new();
    if texts.len() < 2 {
        return out;
    }
    for field in options.iter().filter(|c| c.is_field).take(TEXT_FIELDS) {
        let mut question = text_question(texts);
        question["instructions"] = Value::String(format!(
            "If text must be typed into the field {} to advance `goal`, which of these suggested texts goes into that field?",
            field.label
        ));
        out.insert(format!("{TEXT_FOR}{}", field.key), question);
    }
    out
}

/// Which of the agent's suggested texts fits the field, if any. Without suggestions the
/// question has only `none`, so the answer is always to hand back.
fn text_question(texts: &[String]) -> Value {
    let mut criteria = Map::new();
    for (n, text) in texts.iter().enumerate() {
        criteria.insert(format!("t{n}"), Value::String(format!("Type {text:?}")));
    }
    criteria.insert(
        "none".into(),
        Value::String("None of these fits: the requester must write the text".into()),
    );
    json!({
        "type": "choice",
        "instructions": "If text must be typed into `field` to advance `goal`, which of these suggested texts should be typed?",
        "criteria": Value::Object(criteria)
    })
}

/// Option keys with their labels, for telling the agent what was considered.
pub(crate) fn labelled(
    ranked: &[(String, f64)],
    options: &[Candidate],
    n: usize,
) -> Vec<(String, f64)> {
    ranked
        .iter()
        .take(n)
        .map(|(key, p)| {
            let label = options
                .iter()
                .find(|c| c.key == *key)
                .map_or_else(|| key.clone(), |c| format!("{} [{}]", c.label, c.reference));
            (label, *p)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(list: &[(&str, f64)]) -> Vec<(String, f64)> {
        list.iter().map(|(k, p)| ((*k).to_owned(), *p)).collect()
    }

    #[test]
    fn dialogs_come_first_in_a_long_outline() {
        let footer = "  - link \"Footer link\" [e9]\n".repeat(400);
        let outline = format!(
            "# Bank\nurl: https://bank.example/\n\n- main\n  - heading \"Banking\" level=1\n- contentinfo\n{footer}- dialog \"Choose your cookies\"\n  - button \"Reject non-essential cookies\" [e518]"
        );
        let state = state("freeze a card", &outline, &[], &[]);
        let page = state["page"].as_str().unwrap();
        assert!(page.len() <= MAX_OUTLINE_CHARS + 4);
        assert!(
            page.starts_with("# Bank\nurl: https://bank.example/\n\n- dialog \"Choose your cookies\"\n  - button \"Reject non-essential cookies\" [e518]\n- main"),
            "{page}"
        );
        // A short outline is left in page order.
        assert_eq!(
            dialogs_first("- main\n- dialog \"x\""),
            "- main\n- dialog \"x\""
        );
    }

    #[test]
    fn a_concrete_step_beats_scrolling_toward_it() {
        let near_tie = options(&[("e1", 0.46), (SCROLL, 0.44), ("e2", 0.03)]);
        assert_eq!(pick(&near_tie, 0.5).unwrap().0, "e1");
        let scroll_first = options(&[(SCROLL, 0.49), ("e1", 0.35), ("e2", 0.06)]);
        assert_eq!(pick(&scroll_first, 0.5).unwrap().0, "e1");
    }

    #[test]
    fn close_elements_and_weak_options_are_unsure() {
        let close = options(&[("e1", 0.35), ("e2", 0.30), (SCROLL, 0.1)]);
        assert_eq!(pick(&close, 0.5), None);
        let scroll_only = options(&[(SCROLL, 0.8), ("e1", 0.1)]);
        assert_eq!(pick(&scroll_only, 0.5).unwrap().0, SCROLL);
        let weak = options(&[(SCROLL, 0.4), ("e1", 0.2), ("e2", 0.2)]);
        assert_eq!(pick(&weak, 0.5), None);
    }

    #[test]
    fn offered_keys_say_what_they_do() {
        assert_eq!(
            key_parts("Meta+d: fill the selection down"),
            ("Meta+d", Some("fill the selection down"))
        );
        assert_eq!(key_parts("Escape"), ("Escape", None));
        assert_eq!(key_parts(" Control+Enter "), ("Control+Enter", None));
    }

    #[test]
    fn numbers_come_out_of_texts() {
        assert_eq!(number_in("5000"), Some(5000.0));
        assert_eq!(number_in("5 000 kr"), Some(5000.0));
        assert_eq!(number_in("under 4.5 kg"), Some(4.5));
        assert_eq!(number_in("hybrid"), None);
    }

    fn candidate(key: &str, link: bool) -> Candidate {
        Candidate {
            key: key.to_owned(),
            reference: ElementRef::parse(key).unwrap(),
            label: format!("link \"{key}\""),
            identity: key.to_owned(),
            is_field: false,
            is_range: false,
            is_link: link,
            visible: true,
            option: None,
            undo: None,
            href: None,
        }
    }

    #[test]
    fn only_likely_links_are_opened_side_by_side() {
        let untried = options(&[
            ("e1", 0.35),
            (SCROLL, 0.3),
            ("e2", 0.2),
            ("e3", 0.12),
            ("e4", 0.11),
            ("e5", 0.05),
        ]);
        let all = vec![
            candidate("e1", true),
            candidate("e2", false),
            candidate("e3", true),
            candidate("e4", true),
            candidate("e5", true),
        ];
        let links: Vec<String> = fan_out_links(&untried, &all)
            .into_iter()
            .map(|l| l.identity)
            .collect();
        // e2 is a button (it can't open a tab), e5 is too unlikely; at most three.
        assert_eq!(links, ["e1", "e3", "e4"]);
    }

    #[test]
    fn a_link_that_takes_back_the_last_move_is_an_undo() {
        let url = |s: &str| url::Url::parse(s).unwrap();
        let from = "https://shop.test/sch?q=lego";
        let here = url("https://shop.test/sch?q=lego&bin=1");
        // The filter, once on, is a link without it (and with other parameters).
        assert!(undoes(
            &url("https://shop.test/sch?q=lego&rt=nc"),
            &here,
            from
        ));
        // Straight back where it came from.
        assert!(undoes(&url(from), &here, from));
        // Another filter on top keeps this one.
        assert!(!undoes(
            &url("https://shop.test/sch?q=lego&bin=1&max=50"),
            &here,
            from
        ));
        // The next page of a paged list changes a value, dropping nothing.
        let paged = url("https://shop.test/list?p=2");
        assert!(!undoes(
            &url("https://shop.test/list?p=3"),
            &paged,
            "https://shop.test/list"
        ));
        // Another page altogether.
        assert!(!undoes(&url("https://shop.test/item/7"), &here, from));
    }
}
