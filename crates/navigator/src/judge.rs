//! Judging whether an action is consequential: whether it commits something on the
//! user's behalf (sending, publishing, buying, deleting, connecting...) rather than
//! just moving around or entering data. Consequential actions need the user's approval.

use aibrowse_browser::ActionContext;
use serde_json::json;

use crate::jev::{Jev, JevError};

/// Probability above which an action counts as consequential. Deliberately low: a
/// needless question costs the user a click, a missed one can send a message they never
/// saw.
pub const CONSEQUENTIAL: f64 = 0.35;

/// Whether accepting terms of use, conditions or cookies to go on counts as committing
/// something (the profile's `"agreements"` policy). Accepted by default: a session that
/// stopped at every terms page and cookie wall couldn't get anywhere unattended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Agreements {
    /// Taken like any other step.
    #[default]
    Accept,
    /// Committing: the user approves (or the agent, unattended).
    Ask,
}

/// The probability that `action` (e.g. "click") on `context` commits something on the
/// user's behalf. With `agreements` accepted, a step that only accepts terms or cookies
/// to go on commits nothing.
pub async fn consequential(
    jev: &Jev,
    action: &str,
    context: &ActionContext,
    agreements: Agreements,
) -> Result<f64, JevError> {
    let state = json!({
        "action": format!("{action} {}", context.element),
        "page": format!("{} ({})", context.title, context.url),
        "around_the_element": context.surrounding,
        "entered_by_the_assistant": context.entered,
    });
    let questions = json!({
        "commits": {
            "type": "noul",
            "instructions": "An assistant is operating a person's browser, logged in as them. Would `action` commit something on the person's behalf that others see or that is hard to undo?",
            "criteria": {
                "true": "Yes: sends or posts a message, comment, application or invitation; accepts, connects or follows; buys or pays; deletes, cancels or unsubscribes; changes account settings; submits a form to someone; accepts cookies or tracking beyond what is strictly necessary",
                "false": "No: only navigates, opens or closes something, expands content, searches, filters, sorts, or types without submitting; submits a search, filter or lookup form, whatever its button says (\"Search\", \"Find jobs\", \"Jobsuche absenden\", \"Visa 93 annonser\"): it only shows results; applies or confirms filters or options chosen in a panel of a listing or search (\"OK\", \"Apply\", \"Show 12 results\"); saves or confirms a choice made in a picker or dialog while filling in a search or form (a date, a time, a number of travellers), which only fills it in; declines optional cookies or tracking (keeping only what is necessary), or closes the notice confirming that. A link to a page about doing something (\"Replace a lost card\", \"Apply now\", \"Cancel subscription\") only opens that page; the commitment would be a later step there"
            }
        }
    });
    let mut questions = questions;
    // Declining optional cookies is the user's privacy-preserving default, but asked
    // only whether it "commits" something, Jev hesitates (0.47 for XXL's "Neka alla",
    // over the bar): it changes a setting. So the same request asks whether the button
    // declines a consent prompt and does nothing besides, and one that clearly does
    // only that commits nothing.
    questions["declines_cookies"] = json!({
        "type": "noul",
        "instructions": "Is the element in `action` a button of a cookie, tracking or privacy consent prompt (in any language) that declines everything optional, keeping only what is strictly necessary?",
        "criteria": {
            "true": "Yes: it rejects or declines optional cookies or tracking",
            "false": "No: it accepts any, opens settings, or is not part of such a prompt"
        }
    });
    questions["also_commits"] = also_commits_question();
    if agreements == Agreements::Accept {
        questions["only_agrees"] = json!({
            "type": "noul",
            "instructions": "Does the element in `action` only accept or agree to something in order to go on (terms of use or of sale, conditions, a privacy or cookie notice, accepting cookies), without buying, paying, subscribing, signing up, sending or sharing anything?",
            "criteria": {
                "true": "Yes: an \"I agree\", \"Accept\" or \"Accept all cookies\" that only lets one go on (Lawson Ticket's \"Agree\" to its terms before its calendar)",
                "false": "No: it also buys, pays, books, subscribes, signs up, sends or deletes something, or it isn't an agreement"
            }
        });
    }
    let answers = jev.ask(&state, &questions).await?;
    let p = |name: &str| answers[name]["noul"].as_f64();
    if p("declines_cookies").unwrap_or(0.0) >= DECLINES && p("also_commits").unwrap_or(1.0) < 0.5 {
        return Ok(0.0);
    }
    if agreements == Agreements::Accept
        && p("only_agrees").unwrap_or(0.0) >= AGREES
        && p("also_commits").unwrap_or(1.0) < 0.5
    {
        return Ok(0.0);
    }
    Ok(p("commits").unwrap_or(1.0))
}

/// The probability that clicking the button in `context`, which Jev already found
/// declines a consent prompt's optional cookies (judging the whole page), also commits
/// something besides: subscribes, pays, signs in. Asked alone, without the question
/// whether declining "commits": on the button's cut-down surroundings that question
/// vetoed Clas Ohlson's "Avvisa alla" (0.84), and the prompt was never declined.
pub async fn declining_commits(jev: &Jev, context: &ActionContext) -> Result<f64, JevError> {
    let state = json!({
        "action": format!("click {}", context.element),
        "page": format!("{} ({})", context.title, context.url),
        "around_the_element": context.surrounding,
    });
    let questions = json!({ "also_commits": also_commits_question() });
    let answers = jev.ask(&state, &questions).await?;
    Ok(answers["also_commits"]["noul"].as_f64().unwrap_or(1.0))
}

fn also_commits_question() -> serde_json::Value {
    json!({
        "type": "noul",
        "instructions": "Besides what its label says about cookies or privacy, would clicking the element in `action` subscribe, pay, start a purchase or trial, or sign in or up?",
        "criteria": {
            "true": "Yes: it also does one of those (\"Reject and subscribe\" on a pay-or-consent wall)",
            "false": "No: nothing besides its choice about cookies or privacy"
        }
    })
}

/// How sure Jev must be that a step only accepts terms or cookies for it to commit
/// nothing, when the profile accepts agreements.
const AGREES: f64 = 0.7;

/// How sure Jev must be that a button only declines optional cookies for it to commit
/// nothing: high, since a wrong yes would let an "accept all" through unasked.
const DECLINES: f64 = 0.7;
