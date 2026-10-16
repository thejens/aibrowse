//! Forms without a `<form>` element. SPAs often render a login box, a newsletter signup
//! or a comment box as fields and a button in a plain `<div>`, submitted by a click
//! handler. To an agent that is still one form, and it gets the same signature line
//! (forms.rs) as a real one: this module finds the element that plays the form's part.
//!
//! It is deliberately conservative, since merging unrelated inputs into one "form" would
//! mislead more than separate lines do. A container counts only when it is:
//! - the closest ancestor of a field (within a few levels) that holds a submit-like
//!   button ("Sign in", "Subscribe", "Save"...), and holds exactly one such button;
//! - or a `<fieldset>` around two or more fields, which the page itself declares as a
//!   group;
//!
//! and in either case is not a landmark (the page's main, header, a dialog: too broad),
//! holds no real form, and is small. Fields already in a `<form>` or an ARIA form or
//! search landmark are left to those.

use std::collections::{HashMap, HashSet};

use crate::ax::AxNode;
use crate::forms;
use crate::snapshot::ElementFacts;

/// How far above a field its container may be. Component libraries wrap each input in
/// two or three elements (label, field, control); a form is a level or two above that.
const MAX_LEVELS: usize = 6;
/// Containers with more nodes than this are page sections, not forms.
const MAX_NODES: usize = 400;
/// More fields than this in one container is a settings page or a table, not one form.
const MAX_FIELDS: usize = 12;

/// The AX node ids of elements to present as forms, among `nodes` (one frame's tree).
pub(crate) fn containers<'a>(
    nodes: &'a [AxNode],
    by_id: &HashMap<&'a str, &'a AxNode>,
    fact: impl Fn(&AxNode) -> Option<&'a ElementFacts>,
) -> HashSet<&'a str> {
    let tree = Tree { by_id, fact: &fact };
    let mut out = HashSet::new();
    for node in nodes {
        if node.ignored || !tree.is_field(node) || tree.in_form(node) {
            continue;
        }
        if let Some(container) = tree.container_of(node) {
            out.insert(container.node_id.as_str());
        }
    }
    out
}

struct Tree<'a, 'f, F> {
    by_id: &'f HashMap<&'a str, &'a AxNode>,
    fact: &'f F,
}

/// What a container holds: fields, submit-like buttons, and whether it is too big or
/// wraps a real form.
#[derive(Default)]
struct Contents {
    fields: usize,
    submits: usize,
    nodes: usize,
    has_form: bool,
}

impl<'a, F> Tree<'a, '_, F>
where
    F: Fn(&AxNode) -> Option<&'a ElementFacts>,
{
    fn parent(&self, node: &AxNode) -> Option<&'a AxNode> {
        self.by_id.get(node.parent_id.as_deref()?).copied()
    }

    fn tag(&self, node: &AxNode) -> &'a str {
        (self.fact)(node).map_or("", |f| f.tag.as_str())
    }

    fn is_field(&self, node: &AxNode) -> bool {
        let fact = (self.fact)(node);
        fact.is_some_and(|f| !f.is_hidden())
            && forms::field(node.role(), node.name(), fact).is_some()
    }

    fn is_form(&self, node: &AxNode) -> bool {
        matches!(node.role(), "form" | "search") || self.tag(node) == "form"
    }

    fn in_form(&self, node: &AxNode) -> bool {
        std::iter::successors(self.parent(node), |n| self.parent(n)).any(|n| self.is_form(n))
    }

    /// The closest qualifying container above `field`, if any.
    fn container_of(&self, field: &AxNode) -> Option<&'a AxNode> {
        let mut at = self.parent(field)?;
        for _ in 0..MAX_LEVELS {
            if is_boundary(at.role()) || matches!(self.tag(at), "body" | "html") {
                return None;
            }
            let contents = self.contents(at);
            if contents.has_form || contents.nodes > MAX_NODES || contents.fields > MAX_FIELDS {
                return None;
            }
            match contents.submits {
                1 => return Some(at),
                0 if self.tag(at) == "fieldset" && contents.fields >= 2 => return Some(at),
                0 => {}
                // Two "Save" buttons: several forms side by side, or a toolbar; which
                // fields go with which can't be told.
                _ => return None,
            }
            at = self.parent(at)?;
        }
        None
    }

    fn contents(&self, root: &AxNode) -> Contents {
        let mut contents = Contents::default();
        let mut stack: Vec<&str> = root.child_ids.iter().map(String::as_str).collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.by_id.get(id) else {
                continue;
            };
            contents.nodes += 1;
            if contents.nodes > MAX_NODES {
                break;
            }
            if self.is_form(node) {
                contents.has_form = true;
            }
            if !node.ignored {
                if self.is_field(node) {
                    contents.fields += 1;
                } else if node.role() == "button"
                    && (self.fact)(node).is_some_and(|f| !f.is_hidden())
                    && forms::is_submit_like(node.name())
                {
                    contents.submits += 1;
                }
            }
            // A toolbar's buttons are its own commands ("Add Sheet" in a spreadsheet's tab
            // bar), not the submit of a field beside it, and its fields (a zoom box) are
            // settings: a container around a toolbar takes nothing from inside it.
            if !is_command_group(node.role()) {
                stack.extend(node.child_ids.iter().map(String::as_str));
            }
        }
        contents
    }
}

fn is_command_group(role: &str) -> bool {
    matches!(role, "toolbar" | "menubar" | "menu" | "tablist")
}

/// Roles that are whole parts of a page: a form inside one is found below it, and one of
/// them as the "form" would swallow unrelated controls.
fn is_boundary(role: &str) -> bool {
    matches!(
        role,
        "RootWebArea"
            | "main"
            | "banner"
            | "contentinfo"
            | "navigation"
            | "complementary"
            | "region"
            | "dialog"
            | "alertdialog"
            | "list"
            | "listitem"
            | "table"
            | "grid"
            | "row"
            | "menu"
            | "menubar"
            | "toolbar"
            | "tablist"
            | "tabpanel"
            | "article"
    )
}
