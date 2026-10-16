//! The approval gate (docs/design.md §5.7): actions that commit something on the user's
//! behalf (sending, posting, accepting, buying, deleting...) wait for the user's
//! explicit approval, given in the live view where the agent can't answer for them.
//! This also defends against prompt injection: page text can talk an agent into
//! clicking "Send", but it can't press Approve on the user's screen.

use aibrowse_browser::{ActionContext, ActionResult, ElementRef};
use aibrowse_navigator::{CONSEQUENTIAL, Jev, consequential};
use aibrowse_network::ApiRequest;
use rmcp::model::CallToolResult;

use crate::respond::failure;
use crate::session::Live;
use crate::tools::{ActParams, Action};
use crate::uploads::check_upload;

/// An approved-pending action, run by the server itself once the user approves, so
/// what runs is exactly what was approved.
pub struct Pending {
    pub handoff: String,
    pub parked: Parked,
    pub summary: String,
    /// The page's address when approval was asked: an element action belongs to it.
    pub page: String,
}

/// What waits for approval.
pub enum Parked {
    Act(ActParams),
    /// A fully resolved request (URL, headers, body): exactly what the user saw.
    Api {
        request: ApiRequest,
        pick: Option<String>,
    },
}

pub enum Gate {
    Proceed,
    /// Ask the user; `reason` is shown in the approval page.
    Ask {
        reason: String,
        summary: String,
    },
    Refuse(String),
    /// No user is present: the agent decides, and is told how.
    AgentDecides(String),
}

/// Keys that activate whatever has focus (submit a form, press a button).
fn activates(key: &str) -> bool {
    matches!(key, "Enter" | " " | "Space") || key.ends_with("+Enter")
}

/// Decides whether `p` may run now, needs approval, or is refused.
pub async fn gate(
    live: &mut Live,
    jev: Option<&Jev>,
    p: &ActParams,
    reference: Option<ElementRef>,
) -> Gate {
    if matches!(p.action, Action::Upload) {
        return gate_upload(live, p, reference).await;
    }
    if matches!(p.action, Action::CallTool) {
        return gate_tool(live, jev, p).await;
    }
    if !live.confirm_consequential() {
        return Gate::Proceed;
    }
    let tab = live.tab();
    let (verb, context): (&str, Option<ActionContext>) = match (&p.action, reference) {
        (Action::Click, Some(r)) => ("click", tab.action_context(r).await.ok()),
        (Action::Press, _) if p.key.as_deref().is_some_and(activates) => {
            ("press Enter on", tab.focused_context().await.ok().flatten())
        }
        _ => return Gate::Proceed,
    };
    let Some(context) = context else {
        return Gate::Proceed; // nothing focused / element gone: the action itself will report it
    };
    judge(live, jev, verb, &context, p.confirm, "act").await
}

/// Uploads send local files off the machine, so they always need approval: regardless
/// of the profile's policy and without asking Jev, since what matters is which files go
/// to which site, which only the user can judge. Obvious secrets are refused outright.
async fn gate_upload(live: &mut Live, p: &ActParams, reference: Option<ElementRef>) -> Gate {
    let files = match check_upload(p.files.as_deref().unwrap_or_default()) {
        Ok(files) => files,
        Err(err) => return Gate::Refuse(err),
    };
    let Some(reference) = reference else {
        return Gate::Proceed; // perform() reports the missing ref
    };
    let tab = live.tab();
    let element = match tab.action_context(reference).await {
        Ok(context) => context.element,
        Err(err) => return Gate::Refuse(err.to_string()),
    };
    let (title, url) = (tab.title().await, tab.url());
    let listing: Vec<String> = files.iter().map(|f| f.describe()).collect();
    let summary = format!("upload {} to {element}", listing.join(", "));
    if !live.attended() {
        if p.confirm == Some(true) {
            return Gate::Proceed;
        }
        return Gate::AgentDecides(format!(
            "Not done: \"{summary}\" would send these files from this computer to {url}, and no user is present to approve it (unattended session). Do it only if your instructions explicitly authorize uploading these files to this site, by calling act again with confirm=true; otherwise skip it and report it."
        ));
    }
    let reason = format!(
        "Allow the agent to upload files from this computer?\n{}\nTo: {element} on {title} — {url}",
        listing.join("\n")
    );
    Gate::Ask { reason, summary }
}

/// Decides whether a state-changing API call may run. Read-only calls always may.
pub async fn gate_api(
    live: &mut Live,
    jev: Option<&Jev>,
    request: &ApiRequest,
    triggered_by: Option<&str>,
    confirm: Option<bool>,
) -> Gate {
    if !live.confirm_consequential() {
        return Gate::Proceed;
    }
    let tab = live.tab();
    let mut context = ActionContext {
        url: tab.url(),
        title: tab.title().await,
        element: format!(
            "{} {}",
            request.method,
            aibrowse_network::render::url(&request.url)
        ),
        ..ActionContext::default()
    };
    if let Some(trigger) = triggered_by {
        context.surrounding =
            format!("The site's own page makes this request when the user does: {trigger}");
    }
    if let Some(body) = &request.body {
        context.entered = aibrowse_network::render::body(body, 600);
    }
    judge(live, jev, "send the request", &context, confirm, "api_call").await
}

/// Asks Jev whether the action commits something, and turns the answer into a decision
/// for this session (`tool` is what the agent calls again with confirm=true).
async fn judge(
    live: &Live,
    jev: Option<&Jev>,
    verb: &str,
    context: &ActionContext,
    confirm: Option<bool>,
    tool: &str,
) -> Gate {
    let Some(jev) = jev else {
        return Gate::Refuse(
            "this profile requires approval for consequential actions, but no Jev key is configured to judge them. Set JEV_API_KEY, or set {\"confirm\": \"off\"} in the profile's policy.json".into(),
        );
    };
    match consequential(jev, verb, context, live.agreements()).await {
        Ok(p) if p < CONSEQUENTIAL => Gate::Proceed,
        Ok(_) => commits(live, verb, context, confirm, tool),
        // Fail closed: an action that can't be judged doesn't run unasked.
        Err(err) => Gate::Refuse(format!(
            "could not judge whether this action is consequential ({err}); not performing it"
        )),
    }
}

/// The decision for an action known to commit something on the user's behalf: the
/// user approves it, or, with nobody present, the agent must explicitly take it on.
fn commits(
    live: &Live,
    verb: &str,
    context: &ActionContext,
    confirm: Option<bool>,
    tool: &str,
) -> Gate {
    if !live.attended() {
        if confirm == Some(true) {
            return Gate::Proceed;
        }
        return Gate::AgentDecides(format!(
            "Not done: \"{verb} {}\" would commit something on the user's behalf, and no user is present to approve it (unattended session). Do it only if your instructions explicitly authorize it, by calling {tool} again with confirm=true; otherwise skip it and report it.",
            context.element
        ));
    }
    let summary = format!("{verb} {}", context.element);
    let mut reason = format!(
        "Allow the agent to {summary}?\n{} — {}",
        context.title, context.url
    );
    if !context.entered.is_empty() {
        reason.push_str(&format!("\nIt will submit: {}", context.entered));
    } else if !context.surrounding.is_empty() {
        reason.push_str(&format!(
            "\nIn: {}",
            context.surrounding.chars().take(300).collect::<String>()
        ));
    }
    Gate::Ask { reason, summary }
}

/// Page tools (WebMCP) run the page's own code with the user's session, so they are
/// gated like the clicks they replace. The page's own hints count: a tool it marks
/// read-only runs directly, one it marks consequential needs approval without asking
/// Jev; anything else is judged like a click.
async fn gate_tool(live: &mut Live, jev: Option<&Jev>, p: &ActParams) -> Gate {
    let Some(name) = p.tool.as_deref() else {
        return Gate::Proceed; // perform reports the missing name
    };
    let tab = live.tab();
    let Ok(tool) = tab.webmcp_tool(name) else {
        return Gate::Proceed; // perform reports the missing tool
    };
    if tool.read_only || !live.confirm_consequential() {
        return Gate::Proceed;
    }
    let tab = live.tab();
    let args = p.args.as_ref().map(|a| a.to_string()).unwrap_or_default();
    let context = ActionContext {
        url: tab.url(),
        title: tab.title().await,
        element: format!(
            "the page's tool {name:?} ({})",
            tool.description.chars().take(160).collect::<String>()
        ),
        entered: args.chars().take(600).collect(),
        ..ActionContext::default()
    };
    if tool.consequential {
        return commits(live, "call", &context, p.confirm, "act");
    }
    judge(live, jev, "call", &context, p.confirm, "act").await
}

/// Runs an `act` request against the active tab. `Err` carries a parameter error.
pub async fn perform(
    live: &mut Live,
    p: &ActParams,
    reference: Option<ElementRef>,
) -> Result<aibrowse_browser::Result<ActionResult>, CallToolResult> {
    let needs = |what: &str| failure(format!("act {} needs `{what}`", p.action.name()));
    let tab = live.tab();
    Ok(match (&p.action, reference, p.text.as_deref()) {
        (Action::Click, Some(r), _) => tab.click(r).await,
        (Action::Hover, Some(r), _) => tab.hover(r).await,
        (Action::Fill, Some(r), Some(text)) => tab.fill(r, text).await,
        (Action::Select, Some(r), Some(text)) => tab.select(r, text).await,
        (Action::Set, Some(r), Some(text)) => match number_in(text) {
            Some(value) => tab.set_range(r, value).await,
            None => {
                return Err(failure(format!(
                    "act set needs a number in `text`, not {text:?}"
                )));
            }
        },
        (Action::Type, _, Some(text)) => tab.type_text(text).await,
        (Action::Press, ..) => match p.key.as_deref() {
            Some(key) => tab.press(key).await,
            None => return Err(needs("key")),
        },
        (Action::Scroll, ..) => tab.scroll(p.dx.unwrap_or(0.0), p.dy.unwrap_or(0.0)).await,
        (Action::AcceptDialog, _, prompt) => tab.handle_dialog(true, prompt).await,
        (Action::DismissDialog, ..) => tab.handle_dialog(false, None).await,
        (Action::Upload, Some(r), _) => {
            // Checked again: the files may have changed since the gate (or approval).
            let files = check_upload(p.files.as_deref().unwrap_or_default()).map_err(failure)?;
            let paths: Vec<_> = files.into_iter().map(|f| f.path).collect();
            tab.upload(r, &paths).await
        }
        (Action::Upload, None, _) => return Err(needs("ref and files")),
        (Action::CallTool, ..) => match p.tool.as_deref() {
            Some(name) => {
                let args = p.args.clone().unwrap_or_else(|| serde_json::json!({}));
                tab.call_webmcp_tool(name, args).await
            }
            None => return Err(needs("tool")),
        },
        (Action::Click | Action::Hover, None, _) => return Err(needs("ref")),
        (Action::Fill | Action::Select | Action::Set, ..) => return Err(needs("ref and text")),
        (Action::Type, _, None) => return Err(needs("text")),
    })
}

/// The number in `text` ("5000", "5 000 kr", "4.5"): its digits, spaces between them
/// dropped, and a decimal point.
fn number_in(text: &str) -> Option<f64> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | ' ' | '\u{a0}'))
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}
