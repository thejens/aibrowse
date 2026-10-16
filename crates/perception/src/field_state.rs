//! What a form field says beyond its type: a dropdown's options, and whether what it
//! shows is a setting at all. A dropdown still on its prompt ("Vælg kommune",
//! "Choose…"), a price bound still at the control's own minimum or maximum, or a page
//! number in a pagination bar holds a value, but nobody set it to anything: told among
//! what a page is set to, it reads as a choice that was made.

use serde_json::Value;

use crate::ax::AxNode;
use crate::forms::{MAX_KEPT_OPTIONS, OPTION_CHARS};
use crate::frame::FrameCtx;
use crate::model::Field;
use crate::snapshot::ElementFacts;
use crate::text::clean;

/// How far up from a field a pagination bar is looked for.
const PAGING_REACH: usize = 6;

impl FrameCtx<'_> {
    /// Fills in what `forms::field` can't tell from the element alone: a dropdown's
    /// options and whether it shows its prompt, whether a number or range sits at one of
    /// its own bounds, and whether the field pages through a list.
    pub(crate) fn complete_field(
        &self,
        ax: &AxNode,
        fact: Option<&ElementFacts>,
        field: &mut Field,
    ) {
        let shown = ax.value_text().unwrap_or_default();
        if matches!(field.kind, "select" | "combobox" | "listbox") {
            let mut stack: Vec<&str> = ax.child_ids.iter().rev().map(String::as_str).collect();
            while let Some(id) = stack.pop() {
                let Some(node) = self.by_id.get(id) else {
                    continue;
                };
                if matches!(node.role(), "option" | "MenuListOption") {
                    field.option_count += 1;
                    if field.options.len() < MAX_KEPT_OPTIONS {
                        field.options.push(clean(node.name(), OPTION_CHARS));
                    }
                    let selected = flag(node, "selected")
                        || (node.property("selected").is_none() && node.name() == shown);
                    if selected && self.is_prompt(node) {
                        field.unset = true;
                    }
                } else {
                    stack.extend(node.child_ids.iter().rev().map(String::as_str));
                }
            }
        }
        if matches!(field.kind, "number" | "slider") {
            field.unset = at_bound(ax, fact, &shown);
        }
        field.paging = self.in_pagination(ax);
    }

    /// Whether an option is a dropdown's prompt rather than a choice: it can't be chosen
    /// (disabled), or it submits nothing (`value=""`).
    fn is_prompt(&self, option: &AxNode) -> bool {
        flag(option, "disabled")
            || self
                .fact(option)
                .and_then(|f| f.attribute("value"))
                .is_some_and(|v| v.trim().is_empty())
    }

    /// Whether the field is part of a pagination bar: a wrapper (often a navigation
    /// landmark) its page calls one, by class, id or label (`pagination`, `pager`).
    fn in_pagination(&self, ax: &AxNode) -> bool {
        let mut node = ax;
        for _ in 0..PAGING_REACH {
            let Some(parent) = node.parent_id.as_deref().and_then(|id| self.by_id.get(id)) else {
                return false;
            };
            let named = |key: &str| {
                self.fact(parent)
                    .and_then(|f| f.attribute(key))
                    .is_some_and(|v| {
                        let v = v.to_ascii_lowercase();
                        v.contains("paginat") || v.contains("pager")
                    })
            };
            if named("class") || named("id") || named("aria-label") {
                return true;
            }
            node = parent;
        }
        false
    }
}

fn flag(ax: &AxNode, name: &str) -> bool {
    ax.property(name).and_then(Value::as_bool) == Some(true)
}

/// Whether a number or range shows one of its own bounds (`min`/`max`, or the ARIA
/// ones): a price filter's 0 and 5000 before anyone moved it.
fn at_bound(ax: &AxNode, fact: Option<&ElementFacts>, shown: &str) -> bool {
    let Some(now) = number(shown) else {
        return false;
    };
    let bound = |attr: &str, aria: &str, property: &str| {
        fact.and_then(|f| f.attribute(attr).or_else(|| f.attribute(aria)))
            .and_then(number)
            .or_else(|| ax.property(property).and_then(Value::as_f64))
    };
    [
        bound("min", "aria-valuemin", "valuemin"),
        bound("max", "aria-valuemax", "valuemax"),
    ]
    .into_iter()
    .flatten()
    .any(|b| (b - now).abs() < f64::EPSILON)
}

fn number(text: &str) -> Option<f64> {
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(tag: &str, attrs: &[(&str, &str)]) -> ElementFacts {
        ElementFacts {
            tag: tag.into(),
            attributes: attrs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..ElementFacts::default()
        }
    }

    fn input(attrs: &[(&str, &str)]) -> ElementFacts {
        element("input", attrs)
    }

    fn ax() -> AxNode {
        AxNode::default()
    }

    #[test]
    fn a_bound_at_the_controls_own_end_is_unset() {
        let price = input(&[("min", "0"), ("max", "5000")]);
        assert!(at_bound(&ax(), Some(&price), "0"));
        assert!(at_bound(&ax(), Some(&price), "5000"));
        assert!(!at_bound(&ax(), Some(&price), "1200"));
        let aria = input(&[("aria-valuemin", "1"), ("aria-valuemax", "9")]);
        assert!(at_bound(&ax(), Some(&aria), "9"));
        // Without bounds of its own, any number is a setting.
        assert!(!at_bound(&ax(), Some(&input(&[])), "0"));
    }

    /// An AX node: id, role, value, backend id, parent, children, and `selected`.
    fn node(id: &str, role: &str, value: &str, parent: Option<&str>, children: &[&str]) -> AxNode {
        serde_json::from_value(serde_json::json!({
            "nodeId": id,
            "role": { "value": role },
            "name": { "value": value },
            "value": { "value": value },
            "parentId": parent,
            "childIds": children,
            "backendDOMNodeId": id.parse::<i64>().unwrap(),
            "properties": if role == "option" && id.ends_with('1') {
                serde_json::json!([{ "name": "selected", "value": { "value": true } }])
            } else {
                serde_json::json!([])
            },
        }))
        .expect("an AX node")
    }

    fn complete(
        capture: &crate::FrameCapture,
        facts: &crate::snapshot::FrameFacts,
        id: &str,
    ) -> Field {
        let ctx = FrameCtx::new(capture, facts);
        let ax = ctx.by_id[id];
        let mut field = Field {
            kind: "select",
            options: Vec::new(),
            option_count: 0,
            group: None,
            unset: false,
            paging: false,
        };
        ctx.complete_field(ax, ctx.fact(ax), &mut field);
        field
    }

    #[test]
    fn a_dropdown_on_its_prompt_and_a_pager_are_marked() {
        let capture = crate::FrameCapture {
            target: 0,
            frame_id: "f".into(),
            owner: None,
            rect: None,
            ax: vec![
                node("1", "RootWebArea", "", None, &["10", "20", "30"]),
                // "Vælg kommune" is selected and submits nothing; "Aarhus" is a choice.
                node("10", "combobox", "Vælg kommune", Some("1"), &["11", "12"]),
                node("11", "option", "Vælg kommune", Some("10"), &[]),
                node("12", "option", "Aarhus", Some("10"), &[]),
                // A sort order on a real choice.
                node("20", "combobox", "Newest", Some("1"), &["21"]),
                node("21", "option", "Newest", Some("20"), &[]),
                // The page select of a pagination bar.
                node("30", "navigation", "", Some("1"), &["31"]),
                node("31", "combobox", "2", Some("30"), &["41"]),
                node("41", "option", "2", Some("31"), &[]),
            ],
        };
        let facts = crate::snapshot::FrameFacts {
            elements: [
                (11, element("option", &[("value", "")])),
                (12, element("option", &[("value", "aarhus")])),
                (21, element("option", &[("value", "new")])),
                (30, element("nav", &[("class", "search-pagination")])),
                (41, element("option", &[("value", "2")])),
            ]
            .into_iter()
            .collect(),
            scroll: (0.0, 0.0),
        };
        let prompt = complete(&capture, &facts, "10");
        assert!(prompt.unset && !prompt.paging);
        assert_eq!(prompt.options, ["Vælg kommune", "Aarhus"]);
        let sort = complete(&capture, &facts, "20");
        assert!(!sort.unset && !sort.paging);
        let page = complete(&capture, &facts, "31");
        assert!(!page.unset && page.paging);
    }
}
