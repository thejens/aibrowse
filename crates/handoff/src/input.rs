//! Translating the live view's browser events into CDP input. The user's input is real
//! human input, so it is dispatched as-is: no humanizing.

use serde::Deserialize;
use serde_json::{Value, json};

/// Messages from the live view page.
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ClientEvent {
    Mouse {
        /// mousePressed, mouseReleased or mouseMoved.
        kind: String,
        x: f64,
        y: f64,
        button: String,
        buttons: u32,
        #[serde(rename = "clickCount")]
        click_count: u32,
        modifiers: u32,
        /// The page's `event.timeStamp` (ms), for kinematics recording.
        #[serde(default)]
        ts: Option<f64>,
        /// `PointerEvent.pointerType`: mouse, pen or touch.
        #[serde(default, rename = "pt")]
        pointer: Option<String>,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        #[serde(default)]
        ts: Option<f64>,
    },
    Key {
        /// keyDown or keyUp.
        kind: String,
        key: String,
        code: String,
        #[serde(rename = "keyCode")]
        key_code: u32,
        modifiers: u32,
        #[serde(default)]
        ts: Option<f64>,
    },
    /// Text committed by a mobile keyboard, IME, paste, or the text box.
    Text {
        text: String,
    },
    /// A one-time code from the code relay page.
    Code {
        code: String,
    },
    Done,
    Cancel,
    /// From the watch view: the user takes the browser without being asked.
    Takeover,
    /// Reload the agent's page: the live view's Reload button, Cmd/Ctrl+R or F5 in it,
    /// or the live view page itself reloaded. Reloading only the live view would
    /// change nothing the user sees (a sheet just shared with them still says "You
    /// need access").
    Reload,
    /// The live view's picture size in device pixels: frames are made no larger.
    View {
        w: u32,
        h: u32,
    },
}

/// The CDP command for an input event, or `None` for control messages.
pub fn to_cdp(event: &ClientEvent) -> Option<(&'static str, Value)> {
    match event {
        ClientEvent::Mouse {
            kind,
            x,
            y,
            button,
            buttons,
            click_count,
            modifiers,
            ..
        } => Some((
            "Input.dispatchMouseEvent",
            json!({
                "type": kind, "x": x, "y": y, "button": button, "buttons": buttons,
                "clickCount": click_count, "modifiers": modifiers, "pointerType": "mouse",
            }),
        )),
        ClientEvent::Wheel { x, y, dx, dy, .. } => Some((
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy, "pointerType": "mouse" }),
        )),
        ClientEvent::Key {
            kind,
            key,
            code,
            key_code,
            modifiers,
            ..
        } => {
            // A single-character key without Ctrl/Meta inserts that character.
            let shortcut = modifiers & (2 | 4) != 0;
            let text =
                (kind == "keyDown" && key.chars().count() == 1 && !shortcut).then(|| key.clone());
            let text =
                text.or_else(|| (kind == "keyDown" && key == "Enter").then(|| "\r".to_owned()));
            let cdp_kind = match (kind.as_str(), &text) {
                ("keyDown", Some(_)) => "keyDown",
                ("keyDown", None) => "rawKeyDown",
                _ => "keyUp",
            };
            let mut params = json!({
                "type": cdp_kind, "key": key, "code": code,
                "windowsVirtualKeyCode": key_code, "modifiers": modifiers,
            });
            if let Some(text) = text {
                params["text"] = json!(text);
                params["unmodifiedText"] = json!(text);
            }
            Some(("Input.dispatchKeyEvent", params))
        }
        ClientEvent::Text { text } => Some(("Input.insertText", json!({ "text": text }))),
        ClientEvent::Reload => Some(("Page.reload", json!({}))),
        ClientEvent::View { .. } => None,
        ClientEvent::Done
        | ClientEvent::Cancel
        | ClientEvent::Takeover
        | ClientEvent::Code { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(kind: &str, key: &str, modifiers: u32) -> Value {
        let event = ClientEvent::Key {
            kind: kind.into(),
            key: key.into(),
            code: "KeyA".into(),
            key_code: 65,
            modifiers,
            ts: None,
        };
        to_cdp(&event).unwrap().1
    }

    #[test]
    fn printable_keys_insert_text_but_shortcuts_do_not() {
        assert_eq!(key("keyDown", "a", 0)["text"], "a");
        assert_eq!(key("keyDown", "a", 0)["type"], "keyDown");
        assert!(key("keyDown", "a", 4).get("text").is_none());
        assert_eq!(key("keyDown", "a", 4)["type"], "rawKeyDown");
        assert_eq!(key("keyDown", "Enter", 0)["text"], "\r");
        assert_eq!(key("keyUp", "a", 0)["type"], "keyUp");
    }

    #[test]
    fn a_reload_in_the_live_view_reloads_the_agents_page() {
        let event: ClientEvent = serde_json::from_str(r#"{"t":"reload"}"#).unwrap();
        assert_eq!(to_cdp(&event).unwrap().0, "Page.reload");
    }
}
