//! What a tool call learned along the way that its result text doesn't carry in
//! structured form: action results, the outline, navigator decisions, approvals and API
//! requests. Tools report them with one call each; they go to the call's trace event.
//!
//! The collector is task-local to the tool call, so concurrent calls never mix their
//! notes and tools need no handle to the tracer. Outside a traced call (tracing off,
//! developer commands) reporting does nothing and costs nothing: the closures that copy
//! data only run inside a collector.

use std::cell::RefCell;

use aibrowse_browser::ActionResult;
use aibrowse_navigator::{Decision, Outcome, Report, Step};
use aibrowse_network::{ApiRequest, ApiResponse, render};
use serde::Serialize;

tokio::task_local! {
    static NOTES: RefCell<Notes>;
}

/// Everything reported during one tool call.
#[derive(Debug, Default)]
pub struct Notes {
    pub actions: Vec<ActionResult>,
    /// The last outline the call produced (what the agent saw last).
    pub outline: Option<String>,
    /// One per goal pursued: a goal in parts has one for each part.
    pub navigator: Vec<NavigatorTrace>,
    pub human: Vec<Human>,
    pub api: Vec<ApiTrace>,
}

/// A `navigate(goal)` run: every decision and step, and why it stopped.
#[derive(Debug, Serialize)]
pub struct NavigatorTrace {
    pub goal: String,
    pub outcome: String,
    pub steps: Vec<StepTrace>,
    pub decisions: Vec<Decision>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct StepTrace {
    pub action: String,
    pub confidence: f64,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// An approval or handoff event.
#[derive(Debug, Serialize)]
pub struct Human {
    /// "approval" or "handoff".
    pub kind: &'static str,
    /// "requested", "approved", "denied", "done", "cancelled".
    pub event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What was asked or approved, as shown to the user.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// A request `api_call` sent from the page, redacted like `network_log`.
#[derive(Debug, Serialize)]
pub struct ApiTrace {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Request bodies kept per API call.
const MAX_BODY: usize = 2_000;

/// Runs `call` with a fresh collector and returns its output with what it reported.
pub async fn collect<F: Future>(call: F) -> (F::Output, Notes) {
    NOTES
        .scope(RefCell::new(Notes::default()), async {
            let output = call.await;
            (output, NOTES.with(RefCell::take))
        })
        .await
}

fn report(f: impl FnOnce(&mut Notes)) {
    let _ = NOTES.try_with(|notes| f(&mut notes.borrow_mut()));
}

pub fn action(result: &ActionResult) {
    report(|n| {
        let mut result = result.clone();
        result.url_before = render::url(&result.url_before);
        result.url_after = render::url(&result.url_after);
        n.actions.push(result);
    });
}

pub fn outline(text: &str) {
    report(|n| n.outline = Some(text.to_owned()));
}

pub fn navigator(goal: &str, run: &Report) {
    report(|n| {
        n.navigator.push(NavigatorTrace {
            goal: goal.to_owned(),
            outcome: outcome_name(&run.outcome),
            steps: run.steps.iter().map(step).collect(),
            decisions: run
                .trail
                .iter()
                .map(|d| Decision {
                    url: render::url(&d.url),
                    ..d.clone()
                })
                .collect(),
            elapsed_ms: run.elapsed.as_millis() as u64,
        });
    });
}

pub fn human(kind: &'static str, event: &'static str, id: Option<&str>, detail: &str) {
    report(|n| {
        n.human.push(Human {
            kind,
            event,
            id: id.map(str::to_owned),
            detail: detail.to_owned(),
        });
    });
}

pub fn api(request: &ApiRequest, response: Result<&ApiResponse, &str>) {
    report(|n| {
        let (status, error) = match response {
            Ok(r) => (Some(r.status).filter(|s| *s != 0), r.error.clone()),
            Err(err) => (None, Some(err.to_owned())),
        };
        n.api.push(ApiTrace {
            method: request.method.clone(),
            url: render::url(&request.url),
            headers: render::headers(&request.headers),
            body: request.body.as_deref().map(|b| render::body(b, MAX_BODY)),
            status,
            error,
        });
    });
}

fn step(s: &Step) -> StepTrace {
    StepTrace {
        action: s.action.clone(),
        confidence: s.confidence,
        url: render::url(&s.url),
        error: s.error.clone(),
    }
}

pub(crate) fn outcome_name(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Reached => "reached".into(),
        Outcome::NeedsUser { reason } => format!("needs user: {reason}"),
        Outcome::NeedsAgent {
            field: Some((_, label)),
        } => format!("needs agent: {label}"),
        Outcome::NeedsAgent { field: None } => "needs agent".into(),
        Outcome::Stuck { .. } => "stuck".into(),
        Outcome::Unsure { .. } => "unsure".into(),
        Outcome::Consequential { label, .. } => format!("consequential: {label}"),
        Outcome::OutOfBudget => "out of budget".into(),
        Outcome::Looping { steps } => format!("looping: {}", steps.join(" → ")),
        Outcome::DeadEnd => "dead end".into(),
        Outcome::GaveUp { tried } => format!(
            "gave up: {}",
            tried
                .iter()
                .map(|(label, why)| format!("{label} ({why})"))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn notes_stay_with_their_call_and_cost_nothing_outside_one() {
        outline("not traced");
        let request = ApiRequest {
            method: "POST".into(),
            url: "https://x.com/api/send?access_token=abc".into(),
            headers: vec![
                ("cookie".into(), "li_at=SECRET".into()),
                ("csrf-token".into(), "ajax:SECRET".into()),
                ("accept".into(), "application/json".into()),
            ],
            body: Some(r#"{"password":"SECRET","to":"ada"}"#.into()),
            with_cookies: true,
        };
        let ((), notes) = collect(async {
            outline("the page");
            api(&request, Err("network down"));
        })
        .await;
        assert_eq!(notes.outline.as_deref(), Some("the page"));
        let line = serde_json::to_string(&notes.api).unwrap();
        assert!(!line.contains("SECRET") && !line.contains("abc"), "{line}");
        assert!(
            line.contains("application/json") && line.contains("ada"),
            "{line}"
        );
    }
}
