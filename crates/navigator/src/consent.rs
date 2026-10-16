//! Cookie and privacy consent prompts. They cover the page on first visit and block
//! navigation, and the choice in them is the user's: accepting shares their data with
//! the site's partners. The navigator never accepts on its own; when a prompt offers a
//! way to decline everything optional, it takes that, the privacy-preserving and
//! reversible default (the user can change it in the site's settings later).
//!
//! Which button that is, is Jev's judgment, not a list of labels: prompts come in every
//! language, and the words mislead ("Godkänn nödvändiga kakor", Swedish for "accept the
//! necessary cookies", is the refusal; "OK" may confirm a refusal or accept everything).

use aibrowse_browser::ElementRef;
use aibrowse_perception::{NodeKind, PageModel};
use serde_json::{Map, Value, json};

use crate::candidates::{Candidate, candidates};
use crate::jev::{Jev, JevError, choice};

/// How sure Jev must be before a button is pressed for the user. High: pressing the
/// wrong one could accept tracking they never agreed to.
const DECLINE_MIN: f64 = 0.7;
const NONE: &str = "none";

/// The question that picks the button settling a consent prompt by declining, asked
/// along with whatever else is asked about the page.
pub(crate) fn question(options: &[Candidate]) -> Value {
    let mut criteria = Map::new();
    criteria.insert(
        NONE.into(),
        Value::String(
            "No cookie, tracking or privacy consent prompt is shown, or none of its buttons declines without accepting more".into(),
        ),
    );
    for option in options.iter().filter(|c| settles(c)) {
        criteria.insert(option.key.clone(), Value::String(option.label.clone()));
    }
    json!({
        "type": "choice",
        "instructions": "Is a cookie, tracking or privacy consent prompt shown on this page (in any language)? If so, which button declines everything optional, keeping only what is strictly necessary, without accepting anything more? In a prompt's settings, that may be the button saving the choices while every optional category is switched off as the page shows them. Never a button that accepts all or more than the necessary, and never one that opens settings to customize. Never one that also subscribes, pays, signs in or starts a purchase: a prompt that offers only accepting or paying (\"pay or consent\") has none.",
        "criteria": Value::Object(criteria)
    })
}

/// The question that picks the button opening a consent prompt's settings, for a prompt
/// whose first layer offers no way to decline (Hemnet's: "Acceptera alla" and
/// "Cookie-inställningar" only). Declining is then done in the settings, as a person
/// does; the prompt otherwise blocked the page, and the navigator looked the goal up
/// on the web for two minutes.
pub(crate) fn settings_question(options: &[Candidate]) -> Value {
    let mut criteria = Map::new();
    criteria.insert(
        NONE.into(),
        Value::String(
            "No consent prompt is shown, it has a button declining everything optional, or none of its buttons opens its settings".into(),
        ),
    );
    for option in options.iter().filter(|c| settles(c)) {
        criteria.insert(option.key.clone(), Value::String(option.label.clone()));
    }
    json!({
        "type": "choice",
        "instructions": "Is a cookie, tracking or privacy consent prompt shown that offers no button declining everything optional (in any language)? If so, which button opens its settings or details, where optional cookies can be switched off? A button offering settings and declining in one (\"Einstellungen oder ablehnen\") is one. Never one that accepts anything.",
        "criteria": Value::Object(criteria)
    })
}

/// The button the answer to `settings_question` chose, when Jev is sure of it.
pub(crate) fn settings_chosen<'a>(
    answers: &Value,
    options: &'a [Candidate],
) -> Option<&'a Candidate> {
    let (key, confidence) = choice(answers, "consent_settings");
    if key == NONE || confidence < DECLINE_MIN {
        return None;
    }
    options.iter().find(|c| c.key == key && settles(c))
}

/// How sure Jev must be that a dialog only confirms the choice just made.
const NOTICE_MIN: f64 = 0.6;

/// The one button of an open dialog: after declining, sites often confirm the choice
/// in a modal ("…will now reload to apply your cookie preferences. [OK]") that blocks
/// the page. Found by structure; whether it is such a notice is `notice_question`'s.
pub(crate) fn notice_button(model: &PageModel) -> Option<(ElementRef, String)> {
    let at = model
        .nodes
        .iter()
        .position(|n| matches!(n.role.as_str(), "dialog" | "alertdialog"))?;
    let depth = model.nodes[at].depth;
    let mut inside = model.nodes[at + 1..]
        .iter()
        .take_while(|n| n.depth > depth)
        .filter(|n| n.kind == NodeKind::Actionable && n.reference.is_some());
    let only = inside.next()?;
    if inside.next().is_some() || only.role != "button" {
        return None;
    }
    Some((only.reference?, only.name.clone()))
}

/// Asked, alongside the rest, right after declining when a one-button dialog is open.
/// Asked apart from `question`: one question about both declining and acknowledging
/// came out unsure of either.
pub(crate) fn notice_question() -> Value {
    json!({
        "type": "noul",
        "instructions": "`steps_taken` declined optional cookies. Is the open dialog only a notice confirming that choice, which its one button closes (in any language)?",
        "criteria": {
            "true": "Yes: it reports or confirms the cookie or privacy choice just made; its button only closes it or reloads",
            "false": "No: it asks something else, or its button would accept cookies, sign in, subscribe or commit anything"
        }
    })
}

/// The question whether the page is behind a "pay or consent" wall: only accepting
/// tracking, or paying or subscribing, lets one in, with no free way to decline.
pub(crate) fn wall_question() -> Value {
    json!({
        "type": "noul",
        "instructions": "Is a consent prompt shown (in any language) that offers paying, subscribing or signing up as the alternative to accepting tracking (\"pay or consent\")? Settings or a privacy link beside them don't change that: the navigator looks in the settings once before stopping.",
        "criteria": {
            "true": "Yes: its choices are accepting (\"Allen Zwecken zustimmen\", \"Mit Werbung weiterlesen\") or a subscription, trial or account (\"Zum Abonnement\", \"Pur-Abo\", \"PLUS kostenlos testen\")",
            "false": "No: there is no consent prompt, or it offers no subscription as the alternative (an ordinary prompt with a reject button or settings)"
        }
    })
}

/// The question that picks the button accepting a consent prompt, for a profile that
/// accepts agreements once declining wasn't possible (a pay-or-consent wall, a prompt
/// whose settings offer no way out): accepting, never paying or signing up.
pub(crate) fn accept_question(options: &[Candidate]) -> Value {
    let mut criteria = Map::new();
    criteria.insert(
        NONE.into(),
        Value::String("No consent prompt is shown, or none of its buttons accepts it".into()),
    );
    for option in options.iter().filter(|c| settles(c)) {
        criteria.insert(option.key.clone(), Value::String(option.label.clone()));
    }
    json!({
        "type": "choice",
        "instructions": "Is a cookie, tracking or privacy consent prompt shown (in any language)? If so, which button accepts it, letting one go on to the page (\"Accept all\", \"Allen Zwecken zustimmen\", \"Agree\")? Never one that subscribes, pays, starts a trial or signs in.",
        "criteria": Value::Object(criteria)
    })
}

/// The button the answer to `accept_question` chose, when Jev is sure of it.
pub(crate) fn accept_chosen<'a>(
    answers: &Value,
    options: &'a [Candidate],
) -> Option<&'a Candidate> {
    let (key, confidence) = choice(answers, "consent_accept");
    if key == NONE || confidence < DECLINE_MIN {
        return None;
    }
    options.iter().find(|c| c.key == key && settles(c))
}

/// Whether the answer to `wall_question` is a clear yes.
pub(crate) fn is_wall(answers: &Value) -> bool {
    let p = answers["consent_wall"]["noul"].as_f64().unwrap_or(0.0);
    tracing::debug!(wall = p, "pay-or-consent wall");
    p >= WALL_MIN
}

/// How sure Jev must be of a pay-or-consent wall to stop for the user.
const WALL_MIN: f64 = 0.8;

/// Whether the answer to `notice_question` is a clear yes.
pub(crate) fn is_notice(answers: &Value) -> bool {
    answers["consent_notice"]["noul"].as_f64().unwrap_or(0.0) >= NOTICE_MIN
}

/// What a consent prompt can be settled with: a visible button, or a link that stays on
/// the page. Asking about everything on a long page spread Jev's answer thin (and made
/// every step's request twice the size).
fn settles(option: &Candidate) -> bool {
    // ImmoScout24's way to decline is such a link ("not to consent", `href="#"`).
    let stays = |href: &str| href.starts_with('#') || href.starts_with("javascript:");
    option.visible
        && !option.is_field
        && (!option.is_link || option.href.as_deref().is_some_and(stays))
}

/// Whether the page offers anything a consent prompt could be settled with.
pub(crate) fn askable(options: &[Candidate]) -> bool {
    options.iter().any(settles)
}

/// The button the answer to `question` chose, when Jev is sure of it.
pub(crate) fn chosen<'a>(answers: &Value, options: &'a [Candidate]) -> Option<&'a Candidate> {
    let (key, confidence) = choice(answers, "consent");
    tracing::debug!(%key, confidence, "consent prompt");
    if key == NONE || confidence < DECLINE_MIN {
        return None;
    }
    options.iter().find(|c| c.key == key && settles(c))
}

/// Whether Jev was sure of the button that declines (`chosen`): then only whether it
/// also commits something besides is asked again (`judge::declining_commits`).
pub(crate) fn surely_declines(answers: &Value) -> bool {
    let (key, confidence) = choice(answers, "consent");
    key != NONE && confidence >= SURE_DECLINE
}

/// See `surely_declines`.
const SURE_DECLINE: f64 = 0.9;

/// The button that declines a consent prompt on `model`, asked on its own: for pages
/// the navigator doesn't otherwise think about, such as a search engine's results.
pub async fn decline_button(
    jev: &Jev,
    model: &PageModel,
) -> Result<Option<(ElementRef, String)>, JevError> {
    let options = candidates(model);
    if !askable(&options) {
        return Ok(None);
    }
    let state = json!({ "page": model.render() });
    let answers = jev
        .ask(&state, &json!({ "consent": question(&options) }))
        .await?;
    Ok(chosen(&answers, &options).map(|c| (c.reference, c.label.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibrowse_browser::ElementRef;

    fn option(key: &str, label: &str, is_field: bool) -> Candidate {
        Candidate {
            key: key.into(),
            reference: ElementRef::new(0, 1),
            label: label.into(),
            identity: label.into(),
            is_field,
            is_range: false,
            is_link: false,
            visible: true,
            option: None,
            undo: None,
            href: None,
        }
    }

    #[test]
    fn asks_about_the_buttons_and_acts_only_when_sure() {
        let options = vec![
            option("e1", "button \"Godkänn alla kakor\"", false),
            option("e2", "button \"Godkänn nödvändiga kakor\"", false),
            option("e3", "textbox \"Från\"", true),
        ];
        let q = question(&options);
        let criteria = q["criteria"].as_object().unwrap();
        assert!(criteria.contains_key("none") && criteria.contains_key("e2"));
        assert!(!criteria.contains_key("e3"), "fields don't settle a prompt");

        let answer = |key: &str, confidence: f64| json!({ "consent": { "choice": key, "confidence": confidence } });
        assert_eq!(chosen(&answer("e2", 0.9), &options).unwrap().key, "e2");
        assert!(chosen(&answer("e2", 0.5), &options).is_none());
        assert!(chosen(&answer("none", 0.99), &options).is_none());
        assert!(chosen(&answer("e3", 0.99), &options).is_none());
    }
}
