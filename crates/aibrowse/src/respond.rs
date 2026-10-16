//! Building tool responses: action summaries, outlines, and the guards and errors shared by
//! every tool.

use std::sync::atomic::{AtomicBool, Ordering};

use aibrowse_browser::{ActionResult, ElementRef};
use aibrowse_perception::{Detail, PageModel};
use rmcp::model::{CallToolResult, ContentBlock};

use aibrowse_navigator::{Outcome, Report};

use crate::announce::webmcp_line;
use crate::session::{Live, describe};
use crate::trace::notes;

/// Common tail of every action: follow tabs the page opened or closed, then describe
/// the outcome, optionally followed by the new outline.
pub async fn after_action(
    live: &mut Live,
    result: aibrowse_browser::Result<ActionResult>,
    with_outline: bool,
) -> Vec<ContentBlock> {
    let tab_notes = live.sync_tabs().await;
    let mut content = match result {
        Ok(result) => {
            notes::action(&result);
            vec![ContentBlock::text(describe(&result, &tab_notes))]
        }
        Err(err) => {
            let mut message = err.to_string();
            for note in &tab_notes {
                message.push_str("\nNote: ");
                message.push_str(note);
            }
            return vec![ContentBlock::text(format!("Error: {message}"))];
        }
    };
    if live.has_tabs()
        && let Some(news) = live.agent_interfaces().await
    {
        content.push(ContentBlock::text(news));
    }
    if with_outline && live.has_tabs() {
        content.push(ContentBlock::text(
            render_outline(live, Detail::Interactive, None, true)
                .await
                .0,
        ));
    }
    content
}

/// The page outline, in full.
pub async fn outline(live: &mut Live, detail: Detail) -> String {
    render_outline(live, detail, None, false).await.0
}

/// The page outline, or only what changed since the agent's last outline when it is the
/// same document and little changed (after steps that act on the page).
pub async fn outline_changes(live: &mut Live, detail: Detail) -> String {
    render_outline(live, detail, None, true).await.0
}

/// The page outline, optionally of one region only, with the Page Model it was rendered
/// from (`None` when the page couldn't be observed), e.g. to draw marks with.
pub async fn outline_with_model(
    live: &mut Live,
    detail: Detail,
    region: Option<&str>,
) -> (String, Option<PageModel>) {
    render_outline(live, detail, region, false).await
}

/// The page outline; after an action on the same document, `as_changes` sends only what
/// changed since the agent's last outline (see changes.rs).
async fn render_outline(
    live: &mut Live,
    detail: Detail,
    region: Option<&str>,
    as_changes: bool,
) -> (String, Option<PageModel>) {
    if let Some(blocked) = dialog_block(live) {
        return (blocked, None);
    }
    let (mut text, model) = match aibrowse_perception::observe(live.tab(), detail, region).await {
        Ok(model) => {
            let rendered = model.render();
            let target = live.tab().target_id().to_owned();
            let changes = as_changes
                .then_some(live.seen.as_ref())
                .flatten()
                .and_then(|seen| seen.changes(&target, &model.url, detail, &rendered));
            // Diffs are taken against whole-page outlines only: one region is not what
            // the agent holds of the rest of the page.
            if region.is_none() {
                live.seen = Some(crate::changes::Seen::new(
                    &target, &model.url, detail, &rendered,
                ));
            }
            (changes.unwrap_or(rendered), Some(model))
        }
        Err(err) => (format!("(could not observe the page: {err})"), None),
    };
    if let Some(tools) = webmcp_line(&live.tab().webmcp_tools()) {
        text.push_str("\n\n");
        text.push_str(&tools);
    }
    // News that couldn't be given with the navigation (the probe was still running).
    if let Some(news) = live.agent_interfaces().await {
        text = format!("{news}\n\n{text}");
    }
    notes::outline(&text);
    (text, model)
}

/// A viewport screenshot as base64 JPEG, with the visible actionable elements of `marks`
/// outlined and labelled with their refs (marks.rs).
pub async fn screenshot(live: &mut Live, marks: Option<&PageModel>) -> Result<String, String> {
    let data = live.tab().screenshot().await.map_err(|e| e.to_string())?;
    match marks {
        Some(model) => crate::marks::draw_marks(&data, model.marks(), model.viewport.width),
        None => Ok(data),
    }
}

/// The Page Model to draw marks from when the agent asked for a screenshot only. It is
/// not rendered, so it doesn't become the baseline that later outline changes are
/// computed against. `None` while a dialog freezes the page.
pub async fn model_for_marks(live: &mut Live) -> Option<PageModel> {
    if dialog_block(live).is_some() {
        return None;
    }
    aibrowse_perception::observe(live.tab(), Detail::Interactive, None)
        .await
        .ok()
}

/// While a dialog is open the page is frozen; tell the agent instead of hanging.
pub fn dialog_block(live: &mut Live) -> Option<String> {
    live.tab().dialog().map(|d| {
        format!(
            "A {} dialog is blocking the page: {:?}. Use act accept_dialog or dismiss_dialog first.",
            d.kind, d.message
        )
    })
}

/// Set when serving over HTTP (`aibrowse serve`): the user sits at another machine, so
/// nothing appears on this one's screen and every link goes to the agent to relay.
static REMOTE: AtomicBool = AtomicBool::new(false);

/// Marks this process as serving a user who isn't at this machine.
pub fn serve_remotely() {
    REMOTE.store(true, Ordering::Relaxed);
}

/// Whether pages and notifications may go to this computer's screen. Never for
/// automated runs (tests, scripts set `AIBROWSE_HANDOFF_OPEN=0`), remote users, or off
/// macOS.
fn may_use_screen() -> bool {
    cfg!(target_os = "macos")
        && !REMOTE.load(Ordering::Relaxed)
        && !std::env::var("AIBROWSE_HANDOFF_OPEN").is_ok_and(|v| v == "0")
}

/// Opens `url` in the default browser. Returns false where that isn't possible.
pub fn open_in_browser(url: &str) -> bool {
    may_use_screen()
        && std::process::Command::new("open")
            .arg(url)
            .status()
            .is_ok_and(|s| s.success())
}

/// Asks for the user's attention: a notification, plus the page itself unless they
/// already have a live view open (where the request shows up anyway). Returns whether
/// the user was reached on this computer.
pub fn notify_user(url: &str, reason: &str, open_page: bool) -> bool {
    if !may_use_screen() {
        return false;
    }
    if open_page {
        open_in_browser(url);
    }
    // AppleScript string literal: escape backslashes and quotes.
    let message = reason.replace('\\', "\\\\").replace('"', "\\\"");
    std::process::Command::new("osascript")
        .args([
            "-e",
            &format!("display notification \"{message}\" with title \"aibrowse needs you\" sound name \"Ping\""),
        ])
        .status()
        .is_ok_and(|s| s.success())
}

/// Parses a ref from an outline: "e123" on the page, "f2e45" inside iframe 2.
pub fn parse_ref(reference: &str) -> Result<ElementRef, String> {
    ElementRef::parse(reference).ok_or_else(|| {
        format!(
            "invalid ref {reference:?}; expected something like \"e123\" or \"f2e45\" from observe"
        )
    })
}

pub fn text(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message.into())])
}

pub fn failure(err: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(err.to_string())])
}

/// What the navigator did and whose turn it is now.
/// A clear request for the user, for a handoff the navigator stopped at: what to do,
/// where, and what it is for, so the person knows what is wanted of them.
fn user_ask(task: &str, report: &Report, goal: &str) -> String {
    let site = report
        .steps
        .iter()
        .rev()
        .find_map(|s| url::Url::parse(&s.url).ok())
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_owned())
        });
    let on = site.map(|s| format!(" on {s}")).unwrap_or_default();
    let what = match task {
        "sign_in" => format!("Please sign in{on}"),
        "verify" => {
            format!("Please enter the verification code{on} (or confirm on your other device)")
        }
        "captcha" => format!("Please complete the check{on} that you're not a robot"),
        "payment" => format!("Please confirm or enter the payment details{on}"),
        "consent_wall" => format!(
            "The page{on} lets one in only by accepting tracking or by subscribing. Please choose one if you're happy to"
        ),
        _ => format!("Please do what the page{on} asks of you"),
    };
    format!("{what} so I can continue to: {goal}. Press Done when you're through.")
}

/// The navigator's report on a goal made of parts (`goals`, in order), of which
/// `parts` were pursued: each part as `describe_report` tells it, then the parts it
/// didn't get to and how to carry on with them.
pub fn describe_parts(
    goals: &[String],
    parts: &[(String, Report)],
    tab_notes: &[String],
    attended: bool,
) -> String {
    if let ([goal], [(_, report)]) = (goals, parts) {
        return describe_report(goal, report, tab_notes, attended);
    }
    let mut text = format!("A goal in {} parts, done in order.\n", goals.len());
    for (n, (goal, report)) in parts.iter().enumerate() {
        let notes = if n + 1 == parts.len() { tab_notes } else { &[] };
        text.push_str(&format!("\nPart {} of {}: ", n + 1, goals.len()));
        text.push_str(&describe_report(goal, report, notes, attended));
        text.push('\n');
    }
    let stopped = parts.len().saturating_sub(1);
    let left = &goals[parts.len().min(goals.len())..];
    if !left.is_empty() {
        let quoted: Vec<String> = left.iter().map(|g| format!("{g:?}")).collect();
        text.push_str(&format!(
            "\nNot started, since part {} stopped: {}. Once it is done, call navigate with goal={:?} and then=[{}] to go on.",
            stopped + 1,
            quoted.join(", "),
            goals[stopped],
            quoted.join(", ")
        ));
    }
    text
}

pub fn describe_report(
    goal: &str,
    report: &Report,
    tab_notes: &[String],
    attended: bool,
) -> String {
    let mut text = format!(
        "Goal {goal:?}: {} steps, {} decisions, {:.1}s.\n",
        report.steps.len(),
        report.decisions,
        report.elapsed.as_secs_f64()
    );
    for (i, step) in report.steps.iter().enumerate() {
        text.push_str(&format!(
            "  {}. {} ({:.2})",
            i + 1,
            step.action,
            step.confidence
        ));
        if let Some(err) = &step.error {
            text.push_str(&format!(" — failed: {err}"));
        }
        text.push('\n');
    }
    let options = |options: &[(String, f64)]| {
        options
            .iter()
            .map(|(label, p)| format!("{label} ({p:.2})"))
            .collect::<Vec<_>>()
            .join("; ")
    };
    text.push_str(&match &report.outcome {
        Outcome::Reached if report.settings.is_empty() => {
            "Reached: this page satisfies the goal.".to_owned()
        }
        Outcome::Reached => format!(
            "Reached: this page satisfies the goal. It is set to: {}. If that isn't what the goal asks, change it (act) or call navigate with what is still to do.",
            report.settings.join("; ")
        ),
        Outcome::NeedsUser { reason } if attended => format!(
            "Needs the user ({reason}): only they can continue. If that's appropriate, call request_handoff with reason {:?} (adjust it if you know better; add give_up_after_minutes if you can go on without it), then call navigate with the goal again.",
            user_ask(reason, report, goal)
        ),
        Outcome::NeedsUser { reason } => format!(
            "Blocked: this needs the account owner ({reason}), and no user is present (unattended session). Report it rather than working around it."
        ),
        // A field that already holds a value (SL's "Till: Arlanda flygplats" with no
        // suggestion naming it, Wise's amount with the currency left) isn't waiting for
        // text: the message names what to check instead of asking to fill it again.
        Outcome::NeedsAgent { field: Some((reference, label)) } if label.contains("(value: ") => format!(
            "Your turn: {label} [{reference}] already holds a value, and the navigator can't go on from here. Check it: a suggestion to choose under it, another value, or another field or choice still to set (observe shows them). Then call navigate with the goal again."
        ),
        Outcome::NeedsAgent { field: Some((reference, label)) } => format!(
            "Your turn: text is needed in {label} [{reference}]. Fill it (act fill), then call navigate with the goal again."
        ),
        Outcome::NeedsAgent { field: None } => {
            "Your turn: a decision or text input is needed that the navigator can't make. Look at the page and act.".to_owned()
        }
        Outcome::Stuck { options: o } => format!("Stuck: nothing here clearly helps. Most plausible: {}", options(o)),
        Outcome::Unsure { options: o } => format!("Unsure between: {}. Pick one with act, then navigate on.", options(o)),
        Outcome::Consequential { element, label } => format!(
            "Stopped before {label} [{element}]: it would commit something on the user's behalf. If it is what the user wants, click it with act; they will be asked to approve."
        ),
        Outcome::OutOfBudget => "Stopped: step or time budget used up. Check progress with observe.".to_owned(),
        Outcome::Looping { steps } => format!(
            "Stopped: going in circles, the last steps repeat ({}). Doing them again won't help; look at the page and decide another way.",
            steps.join(" → ")
        ),
        Outcome::DeadEnd => "Stopped: this page is a dead end for the goal.".to_owned(),
        Outcome::GaveUp { tried } => format!(
            "Gave up: the likely links were explored side by side and none led there ({}), and nothing else here does. Look at the page and decide another way, or tell the user.",
            tried
                .iter()
                .map(|(label, why)| format!("{label}: {why}"))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    });
    for note in tab_notes {
        text.push_str(&format!("\nNote: {note}"));
    }
    text
}
