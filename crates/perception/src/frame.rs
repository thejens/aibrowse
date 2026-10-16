//! One frame's accessibility tree with its DOM snapshot facts, and the lookups the
//! fusion step makes in it (descendants, text, name hints).

use std::collections::{HashMap, HashSet};

use crate::FrameCapture;
use crate::ax::AxNode;
use crate::formless;
use crate::snapshot::{ElementFacts, FrameFacts};
use crate::text::{MAX_NAME_CHARS, clean};

/// A label beside a field is a few words ("Subjects", "Date of birth"); longer text is
/// content, not a label.
const MAX_LABEL_CHARS: usize = 40;

pub(crate) struct FrameCtx<'a> {
    pub capture: &'a FrameCapture,
    pub by_id: HashMap<&'a str, &'a AxNode>,
    pub facts: &'a FrameFacts,
    /// Elements that hold a form without being a `<form>` (formless.rs).
    pub forms: HashSet<&'a str>,
    /// `<label for>` elements by the id they name: several where a page repeats ids.
    labels: HashMap<&'a str, Vec<i64>>,
    by_backend: HashMap<i64, &'a AxNode>,
}

impl<'a> FrameCtx<'a> {
    pub fn new(capture: &'a FrameCapture, facts: &'a FrameFacts) -> Self {
        let by_id: HashMap<&str, &AxNode> =
            capture.ax.iter().map(|n| (n.node_id.as_str(), n)).collect();
        let forms = formless::containers(&capture.ax, &by_id, |ax| {
            ax.backend_dom_node_id
                .and_then(|id| facts.elements.get(&id))
        });
        let mut labels: HashMap<&str, Vec<i64>> = HashMap::new();
        for (&backend, fact) in &facts.elements {
            if fact.tag == "label"
                && let Some(target) = fact.attribute("for").filter(|t| !t.is_empty())
            {
                labels.entry(target).or_default().push(backend);
            }
        }
        let by_backend = capture
            .ax
            .iter()
            .filter_map(|n| Some((n.backend_dom_node_id?, n)))
            .collect();
        Self {
            capture,
            by_id,
            facts,
            forms,
            labels,
            by_backend,
        }
    }

    pub fn root(&self) -> Option<&'a AxNode> {
        self.capture.ax.iter().find(|n| n.parent_id.is_none())
    }

    pub fn fact(&self, ax: &AxNode) -> Option<&'a ElementFacts> {
        ax.backend_dom_node_id
            .and_then(|id| self.facts.elements.get(&id))
    }

    /// Whether any non-ignored descendant passes `test`.
    pub fn has_inside(&self, ax: &AxNode, test: impl Fn(&AxNode) -> bool) -> bool {
        let mut stack: Vec<&str> = ax.child_ids.iter().map(String::as_str).collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.by_id.get(id) else {
                continue;
            };
            if !node.ignored && test(node) {
                return true;
            }
            stack.extend(node.child_ids.iter().map(String::as_str));
        }
        false
    }

    /// The label a person reads beside an unnamed field: short text just before it in the
    /// same row, laid out as a label but not linked to the field (`<label>` without `for`,
    /// a grid column's text). Looked for among the earlier siblings of the field and its
    /// ancestors, nearest first, while the ancestor holds no other field, so a row's
    /// label never names the next row's field. A wrapper holding only this field can
    /// also name it with its own `aria-label`: Google Sheets' Name Box is a bare `<input>`
    /// inside `<div aria-label="Name box (⌘ + J)">`, which Chrome doesn't pass down.
    pub fn nearby_label(&self, ax: &AxNode) -> Option<String> {
        let is_field = |n: &AxNode| crate::control::has_value(n.role());
        let mut node = ax;
        for _ in 0..6 {
            let parent = self.by_id.get(node.parent_id.as_deref()?)?;
            let fields = parent
                .child_ids
                .iter()
                .filter_map(|id| self.by_id.get(id.as_str()))
                .filter(|c| is_field(c) || self.has_inside(c, is_field))
                .count();
            if fields > 1 {
                return None;
            }
            if let Some(label) = self.wrapper_label(parent) {
                return Some(label);
            }
            let at = parent.child_ids.iter().position(|id| *id == node.node_id)?;
            for id in parent.child_ids[..at].iter().rev().take(2) {
                let Some(sibling) = self.by_id.get(id.as_str()) else {
                    continue;
                };
                if self.has_inside(sibling, |n| crate::roles::is_interactive(n.role())) {
                    return None;
                }
                let text = clean(
                    &self.subtree_text(sibling, MAX_LABEL_CHARS * 2),
                    MAX_LABEL_CHARS,
                );
                let text = if text.is_empty() && sibling.role() == "StaticText" {
                    clean(sibling.name(), MAX_LABEL_CHARS)
                } else {
                    text
                };
                if !text.is_empty() && text.chars().count() < MAX_LABEL_CHARS {
                    return Some(text);
                }
            }
            node = parent;
        }
        None
    }

    /// A plain wrapper's own `aria-label`. Landmarks and dialogs are left out: their
    /// label names a whole part of the page ("Menu bar", "Subscribe"), not the one field
    /// that happens to be in it.
    fn wrapper_label(&self, wrapper: &AxNode) -> Option<String> {
        if !matches!(
            wrapper.role(),
            "generic" | "group" | "none" | "presentation"
        ) {
            return None;
        }
        let label = clean(
            self.fact(wrapper)?.attribute("aria-label")?,
            MAX_LABEL_CHARS,
        );
        (!label.is_empty()).then_some(label)
    }

    /// Icon-only controls often carry their meaning in a `title`, `aria-label` or `alt`
    /// on the element or a descendant that Chrome didn't fold into the accessible name,
    /// or on their `<label for>` (see `own_label`). A radio button or checkbox with
    /// none of these is at least its `value` ("foot"), which is what it submits.
    pub fn hint_name(&self, ax: &AxNode) -> Option<String> {
        self.hint_inside(ax)
            .or_else(|| {
                let label = self.own_label(ax)?;
                let text = clean(
                    &self.subtree_text(label, MAX_NAME_CHARS * 2),
                    MAX_NAME_CHARS,
                );
                if text.is_empty() {
                    self.hint_inside(label)
                } else {
                    Some(text)
                }
            })
            .or_else(|| {
                let fact = self.fact(ax)?;
                let toggle = fact.tag == "input"
                    && fact.attribute("type").is_some_and(|t| {
                        t.eq_ignore_ascii_case("radio") || t.eq_ignore_ascii_case("checkbox")
                    });
                // "on" is what a checkbox without a value submits: no name at all.
                let value = fact
                    .attribute("value")
                    .map(str::trim)
                    .filter(|v| !v.is_empty() && *v != "on");
                value.filter(|_| toggle).map(|v| clean(v, MAX_NAME_CHARS))
            })
    }

    /// A hint attribute on the node or the first descendant that has one.
    fn hint_inside(&self, ax: &AxNode) -> Option<String> {
        let mut stack = vec![ax];
        let mut visited = 0;
        while let Some(node) = stack.pop() {
            visited += 1;
            if visited > 50 {
                break;
            }
            if let Some(fact) = self.fact(node) {
                for attr in ["title", "aria-label", "alt", "data-tooltip"] {
                    if let Some(hint) = fact.attribute(attr).filter(|v| !v.trim().is_empty()) {
                        return Some(clean(hint, MAX_NAME_CHARS));
                    }
                }
            }
            stack.extend(
                node.child_ids
                    .iter()
                    .rev()
                    .filter_map(|id| self.by_id.get(id.as_str()).copied()),
            );
        }
        None
    }

    /// The `<label for>` of a control that Chrome didn't name from it. Matched by the
    /// id the label names rather than through the DOM: where a page repeats an id
    /// (OpenStreetMap draws its route form twice, one copy hidden), Chrome gives the
    /// label to the first element with that id, and the control people see is left
    /// unnamed. Of several such labels, the one beside the control is its own (Bootstrap
    /// puts the input and its label next to each other), else a visible one.
    fn own_label(&self, ax: &AxNode) -> Option<&'a AxNode> {
        let id = self.fact(ax)?.attribute("id")?;
        let candidates: Vec<&AxNode> = self
            .labels
            .get(id)?
            .iter()
            .filter_map(|backend| self.by_backend.get(backend).copied())
            .collect();
        candidates
            .iter()
            .find(|label| label.parent_id.is_some() && label.parent_id == ax.parent_id)
            .or_else(|| {
                candidates
                    .iter()
                    .find(|label| self.fact(label).is_some_and(|f| !f.is_hidden()))
            })
            .copied()
    }

    /// Visible text under a node, for elements whose accessible name is empty (typical of
    /// div-based clickable cards) and for live regions' messages.
    pub fn subtree_text(&self, ax: &AxNode, limit: usize) -> String {
        let mut out = String::new();
        let mut stack: Vec<&str> = ax.child_ids.iter().rev().map(String::as_str).collect();
        while let Some(id) = stack.pop() {
            if out.len() >= limit {
                break;
            }
            let Some(node) = self.by_id.get(id) else {
                continue;
            };
            if node.role() == "StaticText" {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(node.name());
            } else {
                stack.extend(node.child_ids.iter().rev().map(String::as_str));
            }
        }
        out
    }
}
