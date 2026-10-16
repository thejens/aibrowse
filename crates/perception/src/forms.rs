//! Forms: what each field asks for, and a one-line signature per form.
//!
//! A form is rendered as a single line that reads like a function call:
//!
//! ```text
//! - form (login) → fill(username: "Username or email address" [e10] required, password: [e24] required) submit: button "Sign in" [e38]
//!   - link "Forgot password?" [e265]
//! ```
//!
//! The signature *replaces* the field lines rather than being added on top of them:
//! repeating each field would cost tokens for nothing, and the line carries everything
//! the field lines did (refs, values, states, positions), plus what they couldn't say:
//! which fields belong together, what each one expects, and which button sends them.
//! The rest of the form (links, secondary buttons) stays in the tree below it. ARIA
//! `form` and `search` landmarks fold the same way, and so do forms built without a
//! `<form>` element (formless.rs); other fields keep their own lines.

use std::collections::HashMap;
use std::fmt::Write;

use crate::model::{Field, FormPart, Node, NodeKind};
use crate::render::position_suffix;
use crate::snapshot::ElementFacts;
use crate::text::clean;

/// Dropdown options listed in a signature; the rest are counted.
const MAX_OPTIONS: usize = 4;
/// Dropdown options kept on a field, and the characters kept of each: enough for a
/// sort order or a page size to be chosen by name; a country list is cut short.
pub(crate) const MAX_KEPT_OPTIONS: usize = 30;
pub(crate) const OPTION_CHARS: usize = 80;
/// The characters of an option a signature shows.
const SHOWN_OPTION_CHARS: usize = 30;

/// Types a field by what it asks for. `autocomplete` is the page's own statement of
/// that, so it wins; then the input type; then, for plain text boxes, the label.
pub(crate) fn field(role: &str, name: &str, fact: Option<&ElementFacts>) -> Option<Field> {
    let widget = widget_kind(role, fact)?;
    let attr = |key: &str| fact.and_then(|f| f.attribute(key)).unwrap_or("");
    let kind = autocomplete_kind(attr("autocomplete"))
        .or_else(|| input_type_kind(&attr("type").to_ascii_lowercase()))
        .filter(|_| matches!(widget, "text" | "search"))
        .or_else(|| (widget == "text").then(|| label_kind(name)).flatten())
        .unwrap_or(widget);
    Some(Field {
        kind,
        options: Vec::new(),
        option_count: 0,
        group: (kind == "radio")
            .then(|| attr("name").to_owned())
            .filter(|g| !g.is_empty()),
        unset: false,
        paging: false,
    })
}

/// Whether a button submits its form: `<button>` defaults to type=submit.
pub(crate) fn is_submit(role: &str, fact: Option<&ElementFacts>) -> bool {
    let Some(fact) = fact.filter(|_| role == "button") else {
        return false;
    };
    let kind = fact.attribute("type").unwrap_or("").to_ascii_lowercase();
    match fact.tag.as_str() {
        "button" => kind.is_empty() || kind == "submit",
        "input" => kind == "submit" || kind == "image",
        _ => false,
    }
}

/// Buttons whose name says they send what was filled in. Used where there is no native
/// submit to go by: forms built without `<form>`, and SPA buttons with `type=button`.
/// The name must *be* the verb phrase (optionally followed by an object, "Save changes"),
/// not merely contain it: "Add to list" counts, a paragraph-long name doesn't.
pub(crate) fn is_submit_like(name: &str) -> bool {
    const VERBS: [&str; 34] = [
        "submit",
        "send",
        "save",
        "sign in",
        "sign up",
        "log in",
        "login",
        "log on",
        "register",
        "create",
        "continue",
        "next",
        "search",
        "go",
        "subscribe",
        "join",
        "apply",
        "confirm",
        "update",
        "post",
        "reply",
        "comment",
        "add",
        "ok",
        "done",
        "find",
        "get started",
        "request",
        "book",
        "pay",
        "checkout",
        "check out",
        "place order",
        "verify",
    ];
    let name = name.trim().to_lowercase();
    if name.is_empty() || name.chars().count() > 40 {
        return false;
    }
    VERBS.iter().any(|verb| {
        name == *verb
            || name
                .strip_prefix(verb)
                .is_some_and(|rest| rest.starts_with([' ', '\u{a0}']))
    })
}

fn widget_kind(role: &str, fact: Option<&ElementFacts>) -> Option<&'static str> {
    let tag = fact.map_or("", |f| f.tag.as_str());
    Some(match role {
        "textbox" if tag == "textarea" => "textarea",
        "textbox" => "text",
        "searchbox" => "search",
        "combobox" | "PopUpButton" if tag == "select" => "select",
        "combobox" | "PopUpButton" => "combobox",
        "listbox" => "listbox",
        "checkbox" => "checkbox",
        "radio" => "radio",
        "switch" => "switch",
        "slider" => "slider",
        "spinbutton" => "number",
        _ => return None,
    })
}

/// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#autofill
fn autocomplete_kind(value: &str) -> Option<&'static str> {
    value
        .split_whitespace()
        .rev()
        .find_map(|token| match token.to_ascii_lowercase().as_str() {
            "username" => Some("username"),
            "email" => Some("email"),
            "current-password" => Some("password"),
            "new-password" => Some("new-password"),
            "one-time-code" => Some("otp"),
            "tel" | "tel-national" => Some("tel"),
            "cc-number" => Some("card-number"),
            "cc-exp" | "cc-exp-month" | "cc-exp-year" => Some("card-expiry"),
            "cc-csc" => Some("card-cvc"),
            "name" | "given-name" | "family-name" | "cc-name" => Some("name"),
            "street-address" | "address-line1" | "address-line2" => Some("address"),
            "postal-code" => Some("postal-code"),
            "bday" => Some("date"),
            _ => None,
        })
}

fn input_type_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "password" => "password",
        "email" => "email",
        "tel" => "tel",
        "url" => "url",
        "number" => "number",
        "date" | "datetime-local" | "month" | "week" | "time" => "date",
        "search" => "search",
        "file" => "file",
        _ => return None,
    })
}

/// Plain text boxes whose label gives away what they want. Only codes and email: the
/// ones whose kind changes what the agent does (a code comes from the user; an email
/// address is often the account name).
fn label_kind(name: &str) -> Option<&'static str> {
    let name = name.to_lowercase();
    let code = [
        "verification code",
        "security code",
        "one-time",
        "2fa",
        "authenticator",
    ];
    if code.iter().any(|w| name.contains(w)) || name == "code" || name == "enter code" {
        Some("otp")
    } else if name.contains("email") || name.contains("e-mail") {
        Some("email")
    } else {
        None
    }
}

/// What a form is for, from the kinds of its fields.
pub(crate) fn purpose(kinds: &[&str]) -> Option<&'static str> {
    let count = |k: &str| kinds.iter().filter(|&&kind| kind == k).count();
    if count("new-password") > 0 || count("password") > 1 {
        Some("signup")
    } else if count("password") == 1 {
        Some("login")
    } else if count("otp") > 0 {
        Some("verification")
    } else if count("card-number") > 0 {
        Some("payment")
    } else if !kinds.is_empty() && kinds.iter().all(|&k| k == "search") {
        Some("search")
    } else {
        None
    }
}

/// Forms rendered as signatures: the signature line for each form node, and which
/// nodes it stands in for (so the renderer skips them).
pub(crate) struct Folded {
    pub signatures: HashMap<usize, String>,
    pub skip: Vec<bool>,
    /// The nodes that are a form's submit button.
    pub submits: Vec<usize>,
}

pub(crate) fn fold(nodes: &[Node]) -> Folded {
    let mut folded = Folded {
        signatures: HashMap::new(),
        skip: vec![false; nodes.len()],
        submits: Vec::new(),
    };
    for (i, form) in nodes.iter().enumerate() {
        let is_form = matches!(form.role.as_str(), "form" | "search");
        if form.kind != NodeKind::Structure || !is_form || folded.skip[i] {
            continue;
        }
        let end = i
            + 1
            + nodes[i + 1..]
                .iter()
                .take_while(|n| n.depth > form.depth)
                .count();
        let fields: Vec<usize> = (i + 1..end)
            .filter(|&j| matches!(nodes[j].form_part, Some(FormPart::Field(_))))
            .collect();
        if fields.is_empty() {
            continue;
        }
        let submit = submit_button(nodes, i + 1..end);
        folded.submits.extend(submit);
        folded
            .signatures
            .insert(i, signature(form, nodes, &fields, submit));
        for j in fields.iter().copied().chain(submit) {
            folded.skip[j] = true;
        }
        // Groups (fieldsets, radio groups) left empty by folding would be noise.
        for j in (i + 1..end).rev() {
            if nodes[j].kind == NodeKind::Structure {
                let depth = nodes[j].depth;
                let kept = (j + 1..end)
                    .take_while(|&k| nodes[k].depth > depth)
                    .any(|k| !folded.skip[k]);
                folded.skip[j] = !kept;
            }
        }
    }
    folded
}

/// The button that sends the form: a native submit named like one, else the first
/// native submit (what Enter would press), else a button named like a submit (SPA forms,
/// where buttons are `type=button` with a click handler).
fn submit_button(nodes: &[Node], range: std::ops::Range<usize>) -> Option<usize> {
    let native = |j: &usize| nodes[*j].form_part == Some(FormPart::Submit);
    let named = |j: &usize| {
        nodes[*j].kind == NodeKind::Actionable
            && nodes[*j].role == "button"
            && is_submit_like(&nodes[*j].name)
    };
    range
        .clone()
        .find(|j| native(j) && named(j))
        .or_else(|| range.clone().find(native))
        .or_else(|| range.clone().find(named))
}

fn signature(form: &Node, nodes: &[Node], fields: &[usize], submit: Option<usize>) -> String {
    let kinds: Vec<&str> = fields.iter().filter_map(|&j| kind_of(&nodes[j])).collect();
    let mut out = form.role.clone();
    if !form.name.is_empty() {
        let _ = write!(out, " {:?}", form.name);
    }
    if let Some(purpose) = purpose(&kinds).filter(|p| *p != form.role.as_str()) {
        let _ = write!(out, " ({purpose})");
    }
    out.push_str(" → fill(");
    let mut parts: Vec<String> = Vec::new();
    let mut radio_groups: HashMap<String, usize> = HashMap::new();
    for &j in fields {
        let node = &nodes[j];
        let Some(FormPart::Field(field)) = &node.form_part else {
            continue;
        };
        // Radio buttons of one group read as one choice: `radio(plan): "Free" [e1] | …`.
        if let Some(group) = &field.group {
            let choice = describe(node, None);
            if let Some(&at) = radio_groups.get(group) {
                let _ = write!(parts[at], " | {choice}");
            } else {
                radio_groups.insert(group.clone(), parts.len());
                match group_name(group, nodes, j, form.depth) {
                    Some(name) => parts.push(format!("radio({name}): {choice}")),
                    None => parts.push(format!("radio: {choice}")),
                }
            }
            continue;
        }
        parts.push(format!("{}: {}", field.kind, describe(node, Some(field))));
    }
    out.push_str(&parts.join(", "));
    out.push(')');
    if let Some(j) = submit {
        let button = &nodes[j];
        let _ = write!(out, " submit: button {:?}", button.name);
        if let Some(reference) = button.reference {
            let _ = write!(out, " [{reference}]");
        }
        push_tail(&mut out, button);
    }
    out
}

/// What a radio group is called: its `name` when that reads as one ("plan"), else the
/// label of the group around its first button (a radiogroup or fieldset), else nothing.
/// Some sites generate the name (Avanza's `0.43064796021664253`), and shown, it only
/// costs tokens and suggests a meaning it doesn't have.
fn group_name(group: &str, nodes: &[Node], first: usize, form_depth: usize) -> Option<String> {
    if !generated(group) {
        return Some(group.to_owned());
    }
    let depth = nodes[first].depth;
    nodes[..first]
        .iter()
        .rev()
        .take_while(|n| n.depth > form_depth)
        .filter(|n| n.depth < depth && n.kind == NodeKind::Structure)
        .find(|n| matches!(n.role.as_str(), "radiogroup" | "group"))
        .map(|n| n.name.trim())
        .filter(|name| !name.is_empty())
        .map(|name| format!("{name:?}"))
}

/// Whether a name looks made by a machine rather than a person: no letters at all
/// (a random number), or a run of digits or hex long enough to be an id or a hash
/// (`mui-728401`, `f3a9c2e1`).
fn generated(name: &str) -> bool {
    if !name.chars().any(char::is_alphabetic) {
        return true;
    }
    let digits = name.chars().filter(char::is_ascii_digit).count();
    let hex_run = name
        .split(|c: char| !c.is_ascii_hexdigit())
        .any(|run| run.len() >= 8 && run.chars().any(|c| c.is_ascii_digit()));
    digits >= 5 || hex_run
}

fn kind_of(node: &Node) -> Option<&'static str> {
    match &node.form_part {
        Some(FormPart::Field(field)) => Some(field.kind),
        _ => None,
    }
}

/// `"Label" [ref] value="…" states (position)`, leaving out a label that only repeats
/// the field's kind ("Password" on a password field).
fn describe(node: &Node, field: Option<&Field>) -> String {
    let mut out = String::new();
    let label = node.name.trim_end_matches([':', '*', ' ']);
    let repeats_kind = field.is_some_and(|f| label.eq_ignore_ascii_case(f.kind));
    if !label.is_empty() && !repeats_kind {
        let _ = write!(out, "{label:?} ");
    }
    if let Some(reference) = node.reference {
        let _ = write!(out, "[{reference}]");
    }
    if let Some(value) = node.value.as_deref().filter(|v| !v.is_empty()) {
        let _ = write!(out, " value={value:?}");
    }
    push_tail(&mut out, node);
    if let Some(field) = field.filter(|f| !f.options.is_empty()) {
        let shown: Vec<String> = field
            .options
            .iter()
            .take(MAX_OPTIONS)
            .map(|o| clean(o, SHOWN_OPTION_CHARS))
            .collect();
        let _ = write!(out, " options: {}", shown.join("|"));
        if field.option_count > shown.len() {
            let _ = write!(out, "|+{}", field.option_count - shown.len());
        }
    }
    out
}

fn push_tail(out: &mut String, node: &Node) {
    for word in node
        .states
        .iter()
        .map(String::as_str)
        .chain(node.effects.iter().copied())
    {
        let _ = write!(out, " {word}");
    }
    out.push_str(position_suffix(node.position));
}

#[cfg(test)]
#[path = "forms_tests.rs"]
mod tests;
