//! Controls as widget libraries draw them (fixtures/html/controls.html): named, valued
//! and clickable the way a person sees them. Ignored by default; run with
//! `make test-live`.

mod common;

use common::{Client, find_ref, serve_pages, set_policy};
use serde_json::json;

const CONTROLS: &str = include_str!("../../../fixtures/html/controls.html");

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn widget_controls_read_as_people_see_them() {
    let port = serve_pages(&[("/", CONTROLS)]);
    let mut c = Client::spawn("test-controls", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    // Icon-only toggles: named by their own label's title (not the hidden copy's, which
    // shares the id), else by the value they submit.
    assert!(outline.contains("radio \"Foot\" ["), "{outline}");
    assert!(outline.contains("radio \"bicycle\" ["), "{outline}");
    assert!(!outline.contains("small screens"), "{outline}");
    // Spinbuttons hold what they show: a text input's value, a value in words.
    for field in [
        "spinbutton \"Tag\" [",
        "spinbutton \"Monat\" [",
        "spinbutton \"Jahr\" [",
    ] {
        assert!(outline.contains(field), "{field} in {outline}");
    }
    assert!(
        outline.contains("value=\"14\"")
            && outline.contains("value=\"10\"")
            && outline.contains("value=\"2026\""),
        "{outline}"
    );
    assert!(outline.contains("value=\"Wednesday\""), "{outline}");
    assert!(!outline.contains("value=\"0\""), "{outline}");
    // A calendar's days are under the month they are in.
    assert!(outline.contains("table \"November 2026\""), "{outline}");
    // A jQuery UI range slider's handles are sliders, named as handles of what they set.
    assert!(
        outline.contains("slider \"Price range: slider handle, lower end\"")
            && outline.contains("slider \"Price range: slider handle, upper end\""),
        "{outline}"
    );
    // Checkboxes shrunk to nothing are where their labels are.
    assert!(
        outline.contains("checkbox \"Nike\" [") && outline.contains("checkbox \"Puma\" ["),
        "{outline}"
    );
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn controls_are_reached_through_their_own_labels_and_overlays() {
    set_policy("test-controls-click", r#"{"confirm": "off"}"#);
    let port = serve_pages(&[("/", CONTROLS)]);
    let mut c = Client::spawn("test-controls-click", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    // A hidden radio drawn by its label: the click on the label is the click on it.
    c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "radio \"Foot\"") }),
    );
    let checked = c.ok(
        "evaluate",
        json!({ "expression": "document.querySelector('#route input[value=foot]').checked" }),
    );
    assert!(checked.contains("true"), "{checked}");
    // Its siblings are clipped away at the same spot, under the first one's label: each
    // is aimed at through its own label, not at its box.
    c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "radio \"bicycle\"") }),
    );
    let checked = c.ok(
        "evaluate",
        json!({ "expression": "document.querySelector('#route input:checked').value" }),
    );
    assert!(checked.contains("bicycle"), "{checked}");
    // A checkbox without a box is clicked at its label.
    c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "checkbox \"Puma\"") }),
    );
    let checked = c.ok(
        "evaluate",
        json!({ "expression": "document.getElementById('brand-puma').checked" }),
    );
    assert!(checked.contains("true"), "{checked}");

    // The date field's own overlay passes a click on: clicking the day reaches it.
    c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "spinbutton \"Tag\"") }),
    );
    let focused = c.ok(
        "evaluate",
        json!({ "expression": "document.activeElement.getAttribute('aria-label')" }),
    );
    assert!(focused.contains("Tag"), "{focused}");
    // Filling the month focuses the month itself, wherever the overlay sends clicks.
    let filled = c.ok(
        "act",
        json!({ "action": "fill", "ref": find_ref(&outline, "spinbutton \"Monat\""), "text": "11" }),
    );
    assert!(filled.contains("focused from the keyboard"), "{filled}");
    let values = c.ok(
        "evaluate",
        json!({ "expression": "[...document.querySelectorAll('#datefield input')].map(i => i.value).join('.')" }),
    );
    assert!(values.contains("14.11.2026"), "{values}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_drawn_range_sliders_handles_are_set_in_the_pages_units() {
    set_policy("test-controls-slider", r#"{"confirm": "off"}"#);
    let port = serve_pages(&[("/", CONTROLS)]);
    let mut c = Client::spawn("test-controls-slider", true);
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    // The handles say where they are only by position; "$75 - $300" beside them says
    // what that is in dollars.
    for (handle, value) in [("lower end", "100"), ("upper end", "400")] {
        let name = format!("slider \"Price range: slider handle, {handle}\"");
        let set = c.ok(
            "act",
            json!({ "action": "set", "ref": find_ref(&outline, &name), "text": value }),
        );
        assert!(set.contains(&format!("now at {value}")), "{set}");
    }
    let amount = c.ok(
        "evaluate",
        json!({ "expression": "document.getElementById('amount').value" }),
    );
    assert!(amount.contains("$100 - $400"), "{amount}");
    c.ok("session_close", json!({}));
}
