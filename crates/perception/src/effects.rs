//! Effect hints: what acting on an element does beyond what its role says, shown only
//! when that changes what an agent would do next.
//!
//! - `opens-menu`, `opens-dialog`, `opens-listbox`, ... (`aria-haspopup`): the click
//!   reveals more choices rather than doing something, so observing after it is the
//!   point. Left out on comboboxes and selects, whose role already says it.
//! - `new-tab` (`target=_blank`): the result lands in another tab, which becomes active.
//! - `download` (the `download` attribute): the link saves a file instead of navigating.
//!
//! Links that leave the site already show their destination (`-> docs.github.com`), and
//! a form's submit button is part of its signature, so neither needs a hint. Destructive
//! wording ("Delete", "Remove") gets none: a word list misfires on "Remove filter", and
//! consequential clicks go through the approval gate, which judges them in context.

use crate::ax::AxNode;
use crate::snapshot::ElementFacts;

pub(crate) fn effects(ax: &AxNode, fact: Option<&ElementFacts>) -> Vec<&'static str> {
    let mut out = Vec::new();
    let role = ax.role();
    let implied_popup = matches!(
        role,
        "combobox" | "PopUpButton" | "listbox" | "textbox" | "searchbox"
    );
    if !implied_popup && let Some(popup) = popup(ax) {
        out.push(popup);
    }
    let attr = |name: &str| fact.and_then(|f| f.attribute(name));
    if fact.is_some_and(|f| f.tag == "a" || f.tag == "area") {
        if attr("download").is_some() {
            out.push("download");
        } else if attr("target").is_some_and(|t| t.eq_ignore_ascii_case("_blank")) {
            out.push("new-tab");
        }
    }
    out
}

/// `aria-haspopup` as Chrome reports it ("true" means a menu, by the ARIA spec).
fn popup(ax: &AxNode) -> Option<&'static str> {
    let value = ax.property("hasPopup")?.as_str()?;
    Some(match value {
        "true" | "menu" => "opens-menu",
        "dialog" => "opens-dialog",
        "listbox" => "opens-listbox",
        "tree" => "opens-tree",
        "grid" => "opens-grid",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ax(role: &str, props: serde_json::Value) -> AxNode {
        serde_json::from_value(json!({
            "nodeId": "1",
            "role": { "value": role },
            "properties": props,
        }))
        .unwrap()
    }

    fn anchor(attrs: &[(&str, &str)]) -> ElementFacts {
        ElementFacts {
            tag: "a".into(),
            attributes: attrs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..ElementFacts::default()
        }
    }

    #[test]
    fn popups_tabs_and_downloads() {
        let menu = ax(
            "button",
            json!([{ "name": "hasPopup", "value": { "value": "menu" } }]),
        );
        assert_eq!(effects(&menu, None), ["opens-menu"]);
        let dialog = ax(
            "button",
            json!([{ "name": "hasPopup", "value": { "value": "dialog" } }]),
        );
        assert_eq!(effects(&dialog, None), ["opens-dialog"]);
        // A combobox always pops up a list; saying so is noise.
        let combo = ax(
            "combobox",
            json!([{ "name": "hasPopup", "value": { "value": "listbox" } }]),
        );
        assert!(effects(&combo, None).is_empty());

        let link = ax("link", json!([]));
        assert_eq!(
            effects(&link, Some(&anchor(&[("target", "_blank")]))),
            ["new-tab"]
        );
        assert_eq!(
            effects(
                &link,
                Some(&anchor(&[("download", ""), ("target", "_blank")]))
            ),
            ["download"]
        );
        assert!(effects(&link, Some(&anchor(&[("target", "_self")]))).is_empty());
    }
}
