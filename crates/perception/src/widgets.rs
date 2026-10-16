//! Widgets that libraries draw from generic elements, read the way a person reads them:
//! a spin button's value as shown, a slider's handles, a calendar's month.

use crate::ax::AxNode;
use crate::control::has_value;
use crate::frame::FrameCtx;
use crate::model::Node;
use crate::roles::is_interactive;
use crate::snapshot::ElementFacts;
use crate::text::clean;

/// The longest text that still titles a table of controls ("November 2026"); longer
/// text before it is content.
const MAX_TITLE_CHARS: usize = 40;
/// Controls named by at most this many characters are told apart by where they are
/// (a day "14", a seat "12C").
const MAX_TERSE_CHARS: usize = 4;
/// The fewest controls a titled grid has: a week of days.
const MIN_GRID_CONTROLS: usize = 7;

/// What a field holds, as the page shows it. A spin button says it in words where its
/// author gave some (`aria-valuetext`: "Wednesday" rather than 3); one drawn from a text
/// input (bahn.de's day, month and year) has no `aria-valuenow`, for which Chrome
/// reports 0, and holds its value as a text field does.
pub(crate) fn field_value(role: &str, ax: &AxNode, fact: Option<&ElementFacts>) -> Option<String> {
    if !has_value(role) {
        return None;
    }
    if role == "spinbutton"
        && let Some(fact) = fact
    {
        if let Some(text) = fact
            .attribute("aria-valuetext")
            .filter(|t| !t.trim().is_empty())
        {
            return Some(text.to_owned());
        }
        if let Some(value) = &fact.input_value {
            return Some(value.clone());
        }
    }
    ax.value_text()
}

/// The name an unnamed slider handle gets until `name_unnamed_sliders` says which.
pub(crate) const SLIDER_HANDLE: &str = "slider handle";

/// A slider's handle drawn without the slider role: jQuery UI's range slider is two
/// `<span tabindex=0 class="ui-slider-handle">` in a `<div class="ui-slider">`, with no
/// ARIA, which would read as two unnamed clickables. The class names say what it is,
/// on the handle or on the track holding it; a handle is focusable, as keyboard use
/// (the arrow keys) needs. `Some(true)` when it can be set like a slider (see
/// `settable_handle`), `Some(false)` when it is only named as a handle and stays a
/// clickable.
pub(crate) fn slider_handle(
    frame: &FrameCtx,
    ax: &AxNode,
    fact: Option<&ElementFacts>,
) -> Option<bool> {
    let fact = fact.filter(|f| f.attribute("tabindex").is_some())?;
    let track = ax
        .parent_id
        .as_deref()
        .and_then(|id| frame.by_id.get(id))
        .and_then(|parent| frame.fact(parent));
    let sliderish = |f: &ElementFacts| class_words(f).any(|w| w.contains("slider"));
    (sliderish(fact) || track.is_some_and(sliderish)).then(|| settable_handle(fact, track))
}

/// Whether a drawn handle is surely one, to be set like a slider (`Tab::set_range`
/// reads it by the same class names): its own class says it is a slider's handle
/// (`ui-slider-handle`), or it is a handle in a track whose class says slider. A
/// focusable thing merely inside a slider's markup (a tooltip, a tick) is neither.
fn settable_handle(fact: &ElementFacts, track: Option<&ElementFacts>) -> bool {
    class_words(fact).any(|w| w.contains("slider") && w.contains("handle"))
        || (class_words(fact).any(|w| w.contains("handle"))
            && track.is_some_and(|t| class_words(t).any(|w| w.contains("slider"))))
}

fn class_words(fact: &ElementFacts) -> impl Iterator<Item = String> + '_ {
    fact.attribute("class")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_ascii_lowercase)
}

impl FrameCtx<'_> {
    /// The title of a table of controls that has no name of its own: the short text
    /// just before it in the same widget, outside any control. A date picker's days are
    /// links "1" to "31" in a table, under a header holding "Prev", "Next" and the
    /// month ("November 2026") as plain text; without the month, a day can't be told
    /// from the same day of another month. Only tables of such tersely named controls
    /// (days, seats, hours) are titled: a table of links that say what they are (a
    /// layout table of stories) needs no title, and the text before it is something
    /// else. A heading before it is in the outline anyway.
    pub fn grid_title(&self, ax: &AxNode) -> Option<String> {
        if !matches!(ax.role(), "table" | "grid" | "treegrid") || !self.terse_controls(ax) {
            return None;
        }
        let mut node = ax;
        for _ in 0..3 {
            let parent = self.by_id.get(node.parent_id.as_deref()?)?;
            let at = parent.child_ids.iter().position(|id| *id == node.node_id)?;
            for id in parent.child_ids[..at].iter().rev().take(2) {
                let Some(sibling) = self.by_id.get(id.as_str()) else {
                    continue;
                };
                if sibling.role() == "heading"
                    || self.has_inside(sibling, |n| n.role() == "heading")
                {
                    return None;
                }
                let text = clean(&self.text_outside_controls(sibling), MAX_TITLE_CHARS + 1);
                if text.chars().count() > MAX_TITLE_CHARS {
                    return None;
                }
                // Separators between a header's links ("|") say nothing.
                if text.chars().any(char::is_alphanumeric) {
                    return Some(text);
                }
            }
            node = parent;
        }
        None
    }

    /// Whether a table holds a grid of controls named by a few characters each: a week
    /// of days at least, two thirds of them terse.
    fn terse_controls(&self, ax: &AxNode) -> bool {
        let (mut controls, mut terse) = (0, 0);
        let mut stack: Vec<&str> = ax.child_ids.iter().map(String::as_str).collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.by_id.get(id) else {
                continue;
            };
            if is_interactive(node.role()) {
                controls += 1;
                if node.name().trim().chars().count() <= MAX_TERSE_CHARS {
                    terse += 1;
                }
                continue;
            }
            stack.extend(node.child_ids.iter().map(String::as_str));
        }
        controls >= MIN_GRID_CONTROLS && terse * 3 >= controls * 2
    }

    /// The text under a node that isn't part of a control: a header's title between its
    /// buttons.
    fn text_outside_controls(&self, ax: &AxNode) -> String {
        let mut out = String::new();
        let mut stack = vec![ax];
        while let Some(node) = stack.pop() {
            if out.len() > MAX_TITLE_CHARS * 4 {
                break;
            }
            if is_interactive(node.role()) {
                continue;
            }
            if node.role() == "StaticText" {
                out.push(' ');
                out.push_str(node.name());
                continue;
            }
            stack.extend(
                node.child_ids
                    .iter()
                    .rev()
                    .filter_map(|id| self.by_id.get(id.as_str()).copied()),
            );
        }
        out
    }
}

/// Sliders the page left unnamed get a name from where they are: a price filter's two
/// handles under its "PRIS" toggle (intersport.se) came out as `slider ""` twice, and
/// which sets the maximum was anyone's guess. Two in a row are a range's lower and
/// upper ends; each is named after the nearest named element before it. Handles drawn
/// without the slider role (`slider_handle`) are named the same way, as handles.
pub(crate) fn name_unnamed_sliders(nodes: &mut [Node]) {
    const CONTEXT_REACH: usize = 6;
    let handle = |n: &Node| n.name == SLIDER_HANDLE;
    let unnamed = |n: &Node| (n.role == "slider" && n.name.trim().is_empty()) || handle(n);
    let mut i = 0;
    while i < nodes.len() {
        if !unnamed(&nodes[i]) {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < nodes.len() && unnamed(&nodes[end]) {
            end += 1;
        }
        let context = nodes[i.saturating_sub(CONTEXT_REACH)..i]
            .iter()
            .rev()
            .find(|n| !n.name.trim().is_empty() && n.role != "slider" && !handle(n))
            // A label's colon ("Price range:") reads oddly before the part's own.
            .map(|n| n.name.trim().trim_end_matches(':').trim_end().to_owned());
        let ends = end - i == 2;
        for (k, node) in nodes[i..end].iter_mut().enumerate() {
            let part = match (ends, k) {
                (true, 0) => "lower end".to_owned(),
                (true, _) => "upper end".to_owned(),
                (false, _) if end - i > 1 => format!("{} of {}", k + 1, end - i),
                (false, _) => String::new(),
            };
            let part = match (handle(node), part.is_empty()) {
                (true, true) => SLIDER_HANDLE.to_owned(),
                (true, false) => format!("{SLIDER_HANDLE}, {part}"),
                (false, _) => part,
            };
            node.name = match (&context, part.is_empty()) {
                (Some(context), true) => context.clone(),
                (Some(context), false) => format!("{context}: {part}"),
                (None, false) => part,
                (None, true) => continue,
            };
        }
        i = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;

    fn node(role: &str, name: &str) -> Node {
        Node {
            kind: NodeKind::Actionable,
            depth: 2,
            role: role.into(),
            name: name.into(),
            reference: None,
            value: None,
            description: None,
            states: Vec::new(),
            effects: Vec::new(),
            href: None,
            position: None,
            distance: 0.0,
            rect: None,
            form_part: None,
        }
    }

    #[test]
    fn unnamed_sliders_are_named_by_where_they_are() {
        let mut nodes = vec![
            node("button", "PRIS"),
            node("slider", ""),
            node("slider", ""),
            node("button", "STORLEK"),
            node("slider", ""),
            node("slider", "Maximum price"),
            node("textbox", "Price range:"),
            node("slider", SLIDER_HANDLE),
            node("slider", SLIDER_HANDLE),
        ];
        name_unnamed_sliders(&mut nodes);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "PRIS",
                "PRIS: lower end",
                "PRIS: upper end",
                "STORLEK",
                "STORLEK",
                "Maximum price",
                "Price range:",
                "Price range: slider handle, lower end",
                "Price range: slider handle, upper end"
            ]
        );
    }

    fn classed(class: &str) -> ElementFacts {
        ElementFacts {
            tag: "span".into(),
            attributes: vec![
                ("class".into(), class.into()),
                ("tabindex".into(), "0".into()),
            ],
            bounds: None,
            display: "inline-block".into(),
            visibility: "visible".into(),
            opacity: "1".into(),
            cursor: "auto".into(),
            clickable: false,
            input_value: None,
        }
    }

    #[test]
    fn only_a_sure_handle_is_set_like_a_slider() {
        let track = classed("ui-slider ui-slider-horizontal");
        // jQuery UI's handle says it on itself; a generic handle in a slider's track too.
        assert!(settable_handle(
            &classed("ui-slider-handle ui-state-default"),
            None
        ));
        assert!(settable_handle(&classed("handle"), Some(&track)));
        // Something else focusable in a slider's markup is only named.
        assert!(!settable_handle(&classed("slider-tooltip"), Some(&track)));
        assert!(!settable_handle(
            &classed("handle"),
            Some(&classed("panel"))
        ));
    }
}
