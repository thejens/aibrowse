//! What one accessibility node, with its DOM facts, says about a control: whether a
//! generic element is really clickable, which states to show, and frames to skip.

use crate::ax::AxNode;
use crate::snapshot::ElementFacts;

/// Div- and span-based controls that SPAs use instead of buttons. The AX tree calls them
/// "generic", but the page shows a pointer cursor or attaches a click listener.
///
/// The pointer cursor is the strong signal: it is what tells a person "this is
/// clickable". A listener alone is weak: frameworks attach delegated listeners to
/// containers (React to its root, others to whole sections), so listener-only elements
/// also need a sign of being a control (focusable, or an inline handler), must be small,
/// and must not wrap other controls.
pub(crate) fn is_custom_clickable(
    ax: &AxNode,
    fact: Option<&ElementFacts>,
    viewport_area: f64,
    has_controls_inside: bool,
) -> bool {
    // Labels are excluded: they forward clicks to their control, which is already listed
    // under the same name.
    let generic = matches!(
        ax.role(),
        "generic" | "group" | "image" | "img" | "listitem" | "cell" | "gridcell"
    );
    let Some(fact) = fact else { return false };
    let Some([_, _, w, h]) = fact.bounds else {
        return false;
    };
    if !generic || w < 8.0 || h < 8.0 {
        return false;
    }
    // A pointer-cursor wrapper around a real control is decoration; the control is the
    // thing to act on (and is listed itself).
    if fact.cursor == "pointer" {
        return !has_controls_inside;
    }
    let looks_like_control =
        fact.attribute("tabindex").is_some() || fact.attribute("onclick").is_some();
    fact.clickable && looks_like_control && !has_controls_inside && w * h < viewport_area / 4.0
}

/// Ad slots: an agent never needs to act inside one, and they are long.
pub(crate) fn is_ad_frame(title: &str) -> bool {
    let title = title.to_ascii_lowercase();
    title.contains("advert")
        || title.contains("sponsor")
        || title == "ad"
        || title.starts_with("ad ")
}

/// Live regions: where SPAs report what an action did ("Saved", "Wrong password", "3
/// results"). Shown as `alert` (assertive: errors, warnings) or `status` (polite), with
/// their text, so the outline (and its diff after an action) carries the message.
/// Chat logs, timers and marquees are live too, but chatter rather than outcomes.
pub(crate) fn live_role(ax: &AxNode) -> Option<&'static str> {
    match ax.role() {
        "alert" => return Some("alert"),
        "status" => return Some("status"),
        "log" | "timer" | "marquee" => return None,
        _ => {}
    }
    match ax.property("live").and_then(|v| v.as_str()) {
        Some("assertive") => Some("alert"),
        Some("polite") => Some("status"),
        _ => None,
    }
}

pub(crate) fn is_iframe(role: &str) -> bool {
    matches!(role, "Iframe" | "IframePresentational")
}

pub(crate) fn has_value(role: &str) -> bool {
    matches!(
        role,
        "textbox" | "searchbox" | "combobox" | "spinbutton" | "slider"
    )
}

pub(crate) fn states(ax: &AxNode) -> Vec<String> {
    let mut out = Vec::new();
    let flag = |name: &str| ax.property(name).and_then(|v| v.as_bool()) == Some(true);
    for name in [
        "focused", "disabled", "required", "selected", "readonly", "modal",
    ] {
        if flag(name) {
            out.push(name.to_owned());
        }
    }
    // `invalid` is a token, not a boolean: "false", "true", "grammar" or "spelling".
    // Only a failed validation matters to an agent filling a form.
    if ax.property("invalid").and_then(|v| v.as_str()) == Some("true") {
        out.push("invalid".into());
    }
    if let Some(checked) = ax.property("checked").and_then(|v| v.as_str())
        && checked != "false"
    {
        out.push(if checked == "mixed" {
            "mixed".into()
        } else {
            "checked".into()
        });
    }
    if flag("pressed") {
        out.push("pressed".into());
    }
    if let Some(expanded) = ax.property("expanded").and_then(|v| v.as_bool()) {
        out.push(if expanded {
            "expanded".into()
        } else {
            "collapsed".into()
        });
    }
    if let Some(level) = ax.property("level").and_then(|v| v.as_i64()) {
        out.push(format!("level={level}"));
    }
    out
}
