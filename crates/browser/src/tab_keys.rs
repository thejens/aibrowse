//! Keyboard input: typing text with a human rhythm, and pressing keys and chords with
//! their modifiers held the way fingers hold them.

use std::time::Duration;

use aibrowse_input::{KeySpec, Modifiers, Pace, Stroke, parse_chord, plan_typing};
use serde_json::json;
use tokio::time::sleep;

use crate::error::Result;
use crate::tab::Tab;

impl Tab {
    pub(crate) async fn type_strokes(&mut self, text: &str) -> Result<()> {
        for stroke in plan_typing(text, &self.persona, self.pace, &mut self.rng) {
            match stroke {
                Stroke::Key {
                    spec,
                    delay_before,
                    hold,
                } => {
                    sleep(delay_before).await;
                    self.key_with_modifiers(&spec, hold).await?;
                }
                Stroke::Text { text, delay_before } => {
                    sleep(delay_before).await;
                    self.dispatch_input("Input.insertText", json!({ "text": text }))
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn press_spec(&mut self, spec: &KeySpec) -> Result<()> {
        let hold = match self.pace {
            Pace::Fast => Duration::ZERO,
            Pace::Human => Duration::from_millis(self.rng_range(60, 120)),
        };
        self.key_with_modifiers(spec, hold).await
    }

    /// Presses modifier keys down first and releases them last, as fingers do; a capital
    /// letter is Shift-down, key-down, key-up, Shift-up, not a lone "A" event.
    async fn key_with_modifiers(&mut self, spec: &KeySpec, hold: Duration) -> Result<()> {
        let held: Vec<(u32, KeySpec)> = MODIFIER_KEYS
            .iter()
            .filter(|(flag, ..)| spec.modifiers.has(*flag))
            .map(|&(flag, key, code, key_code)| {
                (
                    flag,
                    KeySpec {
                        key: key.to_owned(),
                        code: code.to_owned(),
                        key_code,
                        text: None,
                        modifiers: Modifiers::default(),
                    },
                )
            })
            .collect();
        let mut active = 0;
        for (flag, modifier) in &held {
            active |= flag;
            self.key_event("rawKeyDown", modifier, active, &[]).await?;
        }
        let commands = mac_editing_commands(spec);
        let down = if spec.text.is_some() {
            "keyDown"
        } else {
            "rawKeyDown"
        };
        self.key_event(down, spec, active, &commands).await?;
        sleep(hold).await;
        self.key_event("keyUp", spec, active, &[]).await?;
        for (flag, modifier) in held.iter().rev() {
            active &= !flag;
            self.key_event("keyUp", modifier, active, &[]).await?;
        }
        Ok(())
    }

    /// Presses (`down`) or releases a lone modifier key (a `Modifiers` flag) and keeps
    /// `held` in step, so mouse events carry it while it is down.
    pub(crate) async fn hold_modifier(&mut self, flag: u32, down: bool) -> Result<()> {
        let &(_, key, code, key_code) = MODIFIER_KEYS
            .iter()
            .find(|(f, ..)| *f == flag)
            .expect("a known modifier flag");
        let spec = KeySpec {
            key: key.to_owned(),
            code: code.to_owned(),
            key_code,
            text: None,
            modifiers: Modifiers::default(),
        };
        if down {
            self.held |= flag;
            self.key_event("rawKeyDown", &spec, self.held, &[]).await
        } else {
            self.held &= !flag;
            self.key_event("keyUp", &spec, self.held, &[]).await
        }
    }

    async fn key_event(
        &self,
        kind: &str,
        spec: &KeySpec,
        modifiers: u32,
        commands: &[&str],
    ) -> Result<()> {
        let mut params = json!({
            "type": kind,
            "modifiers": modifiers,
            "key": spec.key,
            "code": spec.code,
            "windowsVirtualKeyCode": spec.key_code,
        });
        if kind == "keyDown"
            && let Some(text) = &spec.text
        {
            params["text"] = json!(text);
            params["unmodifiedText"] = json!(text);
        }
        if !commands.is_empty() {
            params["commands"] = json!(commands);
        }
        self.dispatch_input("Input.dispatchKeyEvent", params)
            .await?;
        Ok(())
    }
}

const MODIFIER_KEYS: &[(u32, &str, &str, u32)] = &[
    (Modifiers::META, "Meta", "MetaLeft", 91),
    (Modifiers::CONTROL, "Control", "ControlLeft", 17),
    (Modifiers::ALT, "Alt", "AltLeft", 18),
    (Modifiers::SHIFT, "Shift", "ShiftLeft", 16),
];

pub(crate) fn select_all_chord() -> KeySpec {
    let chord = if cfg!(target_os = "macos") {
        "Meta+a"
    } else {
        "Control+a"
    };
    parse_chord(chord).expect("select-all chord is valid")
}

/// On macOS, editing shortcuts are dispatched by the OS menu layer, which synthetic CDP
/// key events bypass. Chrome accepts the equivalent editing commands alongside the key
/// event so that, e.g., Cmd+A really selects all.
fn mac_editing_commands(spec: &KeySpec) -> Vec<&'static str> {
    if !cfg!(target_os = "macos") || !spec.modifiers.has(Modifiers::META) {
        return Vec::new();
    }
    let shift = spec.modifiers.has(Modifiers::SHIFT);
    match (spec.code.as_str(), shift) {
        ("KeyA", false) => vec!["selectAll"],
        ("KeyC", false) => vec!["copy"],
        ("KeyX", false) => vec!["cut"],
        ("KeyV", false) => vec!["paste"],
        ("KeyZ", false) => vec!["undo"],
        ("KeyZ", true) => vec!["redo"],
        _ => Vec::new(),
    }
}
