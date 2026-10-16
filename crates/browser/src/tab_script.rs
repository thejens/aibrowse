//! Running script against the page: agent-supplied JavaScript, and DOM operations that
//! have no physical-input equivalent we can drive.

use serde_json::{Value, json};

use crate::error::{BrowserError, Result};
use crate::frames::ElementRef;
use crate::isolated::IsolatedWorld;
use crate::tab::{ActionResult, Tab, stale};

/// Results larger than this are truncated before they reach the agent's context.
const MAX_RESULT_CHARS: usize = 20_000;

/// Picks an option by value or visible label (exact, then case-insensitive substring).
/// Uses the prototype's value setter so frameworks that shadow `value` on the instance
/// (React) still see the change, and fires the events a user selection would.
const SELECT_OPTION: &str = r#"function (choice) {
  if (!(this instanceof HTMLSelectElement)) return { error: 'not a <select> element' };
  const options = [...this.options];
  // Whitespace as a reader sees it: an option written "4\u00a0milj\u00a0kr" is named "4 milj kr".
  const norm = (s) => s.replace(/\s+/g, ' ').trim().toLowerCase();
  const digits = (s) => /^\D*\d[\d\s.,\u00a0]*\D*$/.test(s) ? s.replace(/\D/g, '') : '';
  const option = options.find((o) => o.value === choice || o.label === choice)
    || options.find((o) => norm(o.label) === norm(choice))
    || options.find((o) => norm(o.label).includes(norm(choice)))
    // A number by its digits: "200000" is the option "£200,000" (or "200 000 kr").
    || (digits(choice) && options.find((o) => digits(o.label) === digits(choice)));
  if (!option) return { error: 'no option matches', options: options.map((o) => o.label).slice(0, 50) };
  this.focus();
  Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(this, option.value);
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return { selected: option.label };
}"#;

/// An element's name as a person would call it, as a JS function `nameOf(element)`: its
/// ARIA name, a button input's value, its `<label>`, its own text, title or
/// placeholder. A field's value is what it holds, not its name: a checkbox labelled
/// "Music" may have the value "3".
macro_rules! name_of {
    () => {
        r#"
  const nameOf = (e) => {
    const squash = (s) => (s || '').replace(/\s+/g, ' ').trim();
    const by = e.getAttribute('aria-labelledby');
    const labelled = by && squash(by.split(/\s+/).map((id) => document.getElementById(id)?.innerText || '').join(' '));
    if (labelled) return labelled;
    if (squash(e.getAttribute('aria-label'))) return squash(e.getAttribute('aria-label'));
    if (e.tagName === 'INPUT' && /^(button|submit|reset)$/.test(e.type)) return squash(e.value);
    const labels = e.labels ? squash([...e.labels].map((l) => l.innerText).join(' ')) : '';
    if (labels) return labels;
    const field = /^(INPUT|TEXTAREA|SELECT)$/.test(e.tagName);
    return squash((!field && e.innerText) || e.title || e.getAttribute('placeholder') || '');
  };
"#
    };
}

/// Describes an element and what surrounds it: its own label, and the text of the form,
/// dialog or composer it belongs to (what would be sent or confirmed). Used to judge
/// whether acting on it is consequential and to show the user what they are approving.
const ACTION_CONTEXT: &str = concat!(
    "function () {",
    name_of!(),
    r#"
  const squash = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const label = nameOf(this);
  const scope = this.closest('form, [role=dialog], dialog, [role=form], [contenteditable=true], article, section') || this.parentElement;
  const fields = scope ? [...scope.querySelectorAll('input, textarea, [contenteditable=true]')]
    .filter((f) => f.type !== 'password' && f.type !== 'hidden')
    .map((f) => squash(f.value !== undefined && f.tagName !== 'DIV' ? f.value : f.innerText))
    .filter(Boolean) : [];
  // Where a link leads, so a judgment isn't made from its words alone ("Freeze a card"
  // may open an article about it). Forms get no such hint: script-driven forms ignore
  // their method, so a form's GET says nothing about whether submitting it commits.
  const link = this.closest('a[href]');
  const leads = link && /^https?:/.test(link.href) ? ` -> opens ${link.href.slice(0, 150)}` : '';
  return {
    element: `${this.tagName.toLowerCase()} "${label.slice(0, 120)}"${leads}`,
    surrounding: scope ? squash(scope.innerText).slice(0, 600) : '',
    entered: fields.join(' | ').slice(0, 600),
  };
}"#
);

/// A short name for an element as a person would refer to it: `button "Accept"`.
/// Only http(s) links open a page in a new tab; `javascript:` and the like don't.
const LINK_HREF: &str = r#"function () {
  const a = this.closest('a[href]');
  return a && /^https?:/.test(a.href) ? a.href : null;
}"#;

const ELEMENT_LABEL: &str = concat!(
    "function () {",
    name_of!(),
    r#"
  const label = nameOf(this);
  const role = this.getAttribute('role') || this.tagName.toLowerCase();
  return label ? `${role} "${label.slice(0, 60)}"` : role;
}"#
);

/// Whether text typed into an element is a secret: a password field, or a field whose
/// `autocomplete` asks for a password, one-time code or card number / security code.
/// A frame element counts as secret, because what has focus inside it can't be seen
/// from here and the safe answer is the one that never leaks.
const SECRET_INPUT: &str = r#"function () {
  if (this.tagName === 'IFRAME' || this.tagName === 'FRAME') return true;
  const type = (this.getAttribute('type') || '').toLowerCase();
  const complete = (this.getAttribute('autocomplete') || '').toLowerCase();
  return type === 'password' || /password|one-time-code|cc-number|cc-csc/.test(complete);
}"#;

/// What an action would act on, for consequence checks and approval prompts.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ActionContext {
    pub url: String,
    pub title: String,
    /// e.g. `button "Send"`.
    pub element: String,
    /// Text of the enclosing form, dialog or post.
    pub surrounding: String,
    /// What has been typed into that form (never passwords).
    pub entered: String,
}

/// Where agent JavaScript runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum World {
    /// Shares the DOM but not the page's JavaScript; invisible to the page.
    Isolated,
    /// The page's own context: sees page globals, and the page can observe it.
    Main,
}

impl Tab {
    /// Evaluates an expression (promises are awaited) and returns its JSON value.
    pub async fn evaluate(&mut self, expression: &str, world: World) -> Result<Value> {
        let value = match world {
            World::Isolated => {
                let session = self.session.clone();
                self.world().await?.evaluate(&session, expression).await?
            }
            World::Main => {
                // No contextId: the main world's default context. Runtime stays disabled.
                let response = self
                    .session
                    .call(
                        "Runtime.evaluate",
                        json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
                    )
                    .await?;
                if let Some(details) = response.get("exceptionDetails") {
                    let message = details["exception"]["description"]
                        .as_str()
                        .or_else(|| details["text"].as_str())
                        .unwrap_or("script threw");
                    return Err(BrowserError::Element(format!("script error: {message}")));
                }
                response["result"]["value"].clone()
            }
        };
        Ok(truncate(value))
    }

    /// Evaluates in the isolated world without the size cap, for internal extractors.
    pub async fn evaluate_isolated_raw(&mut self, expression: &str) -> Result<Value> {
        let session = self.session.clone();
        self.world().await?.evaluate(&session, expression).await
    }

    /// Chooses an option of a native `<select>`.
    ///
    /// Native dropdowns open an OS-level menu that page input events can't reach, so this
    /// sets the selection directly and fires `input`/`change` (those events are untrusted,
    /// a minor tell accepted until the Chromium patch set offers a native path).
    pub async fn select(&mut self, element: ElementRef, choice: &str) -> Result<ActionResult> {
        let start = self.begin();
        let (session, object_id) = self.resolve_in_frame(element).await?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": SELECT_OPTION,
                    "objectId": object_id,
                    "arguments": [{ "value": choice }],
                    "returnByValue": true,
                }),
            )
            .await?;
        let outcome = &response["result"]["value"];
        if let Some(error) = outcome["error"].as_str() {
            let options = outcome["options"]
                .as_array()
                .map(|o| {
                    o.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            return Err(BrowserError::Element(if options.is_empty() {
                format!("cannot select {choice:?}: {error}")
            } else {
                format!("cannot select {choice:?}: {error}. Options: {options}")
            }));
        }
        let notes = vec![format!(
            "selected {:?}",
            outcome["selected"].as_str().unwrap_or(choice)
        )];
        self.finish("select", start, Tab::settle_timeout(), notes)
            .await
    }
}

impl Tab {
    /// The context of acting on `element`: what it is and what it would submit.
    /// How to name `element` in action descriptions; `None` if it can't be resolved.
    /// The absolute URL of the link at `element` (or the link it sits in), if it is one.
    pub(crate) async fn link_href(&mut self, element: ElementRef) -> Option<String> {
        let (session, object_id) = self.resolve_in_frame(element).await.ok()?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": LINK_HREF, "objectId": object_id, "returnByValue": true }),
            )
            .await
            .ok()?;
        response["result"]["value"].as_str().map(str::to_owned)
    }

    pub(crate) async fn element_label(&mut self, element: ElementRef) -> Option<String> {
        let (session, object_id) = self.resolve_in_frame(element).await.ok()?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": ELEMENT_LABEL, "objectId": object_id, "returnByValue": true }),
            )
            .await
            .ok()?;
        response["result"]["value"].as_str().map(str::to_owned)
    }

    pub async fn action_context(&mut self, element: ElementRef) -> Result<ActionContext> {
        let (session, object_id) = self.resolve_in_frame(element).await?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": ACTION_CONTEXT, "objectId": object_id, "returnByValue": true }),
            )
            .await?;
        let value = &response["result"]["value"];
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
        Ok(ActionContext {
            url: self.url(),
            title: self.title().await,
            element: text("element"),
            surrounding: text("surrounding"),
            entered: text("entered"),
        })
    }

    /// The context of pressing a key: the focused element, if any.
    pub async fn focused_context(&mut self) -> Result<Option<ActionContext>> {
        let focused = self
            .evaluate_isolated_raw(
                "(() => { let e = document.activeElement; return e && e !== document.body ? true : null; })()",
            )
            .await?;
        if focused.is_null() {
            return Ok(None);
        }
        let session = self.session.clone();
        let world = self.world().await?;
        let response = world
            .evaluate(
                &session,
                &format!("(() => {{ const f = {ACTION_CONTEXT}; return f.call(document.activeElement); }})()"),
            )
            .await?;
        let text = |key: &str| response[key].as_str().unwrap_or_default().to_owned();
        Ok(Some(ActionContext {
            url: self.url(),
            title: self.title().await,
            element: text("element"),
            surrounding: text("surrounding"),
            entered: text("entered"),
        }))
    }

    /// Whether text typed into `element` (or, with `None`, into whatever has focus) must
    /// be kept secret. Fails closed: when the element can't be inspected, it counts as
    /// secret, so callers that record typed text never record a password by accident.
    pub async fn is_secret_input(&mut self, element: Option<ElementRef>) -> bool {
        let secret = match element {
            Some(element) => {
                let Ok((session, object_id)) = self.resolve_in_frame(element).await else {
                    return true;
                };
                session
                    .call(
                        "Runtime.callFunctionOn",
                        json!({ "functionDeclaration": SECRET_INPUT, "objectId": object_id, "returnByValue": true }),
                    )
                    .await
                    .map(|response| response["result"]["value"].clone())
                    .map_err(BrowserError::from)
            }
            // Focus may sit inside shadow roots; follow it down to the element itself.
            None => self
                .evaluate_isolated_raw(&format!(
                    "(() => {{ let e = document.activeElement; while (e && e.shadowRoot && e.shadowRoot.activeElement) e = e.shadowRoot.activeElement; return e ? ({SECRET_INPUT}).call(e) : false; }})()"
                ))
                .await,
        };
        secret.map_or(true, |value| value.as_bool() != Some(false))
    }

    pub async fn title(&mut self) -> String {
        self.evaluate_isolated_raw("document.title")
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Resolves an element into an isolated world of whichever frame holds it, returning
    /// the session and remote object id. Node ids don't say which frame they belong to,
    /// so each frame of the element's target is tried.
    pub(crate) async fn resolve_in_frame(
        &mut self,
        element: ElementRef,
    ) -> Result<(aibrowse_cdp::Session, String)> {
        self.refuse_document_root(element).await?;
        let session = self.session_for(element.target)?;
        let frames = self.frames().await?;
        for frame in frames.iter().filter(|f| f.target == element.target) {
            let Ok(world) = IsolatedWorld::create(&session, &frame.frame_id, 0).await else {
                continue;
            };
            if let Ok(object_id) = world.resolve(&session, element.backend).await
                && !object_id.is_empty()
            {
                return Ok((session, object_id));
            }
        }
        Err(stale(element))
    }
}

fn truncate(value: Value) -> Value {
    let text = value.to_string();
    if text.len() <= MAX_RESULT_CHARS {
        return value;
    }
    let cut: String = text.chars().take(MAX_RESULT_CHARS).collect();
    Value::String(format!("{cut}… (truncated from {} chars)", text.len()))
}
