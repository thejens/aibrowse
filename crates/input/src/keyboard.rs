use std::time::Duration;

use rand::{Rng, RngExt};

use crate::Pace;
use crate::motion::log_normal_ms;
use crate::persona::Persona;

/// A physical key on a US layout, as CDP `Input.dispatchKeyEvent` wants it described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    /// `KeyboardEvent.key`, e.g. "a", "A", "Enter".
    pub key: String,
    /// `KeyboardEvent.code`, e.g. "KeyA", "Enter".
    pub code: String,
    /// Windows virtual key code; Chrome needs it for keys to behave natively.
    pub key_code: u32,
    /// Text the key inserts, if any ("\r" for Enter).
    pub text: Option<String>,
    pub modifiers: Modifiers,
}

/// Modifier bitmask in CDP's encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers(pub u32);

impl Modifiers {
    pub const ALT: u32 = 1;
    pub const CONTROL: u32 = 2;
    pub const META: u32 = 4;
    pub const SHIFT: u32 = 8;

    pub fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }
}

/// One step of a typing plan.
#[derive(Debug, Clone, PartialEq)]
pub enum Stroke {
    /// Press and release a physical key.
    Key {
        spec: KeySpec,
        delay_before: Duration,
        hold: Duration,
    },
    /// Characters with no key on the layout (accents, emoji, CJK) are committed as text,
    /// which is how an IME or the macOS character picker delivers them.
    Text {
        text: String,
        delay_before: Duration,
    },
}

/// Plans keystrokes for `text`, with log-normal inter-key gaps and longer pauses at word
/// boundaries, where people naturally hesitate.
pub fn plan_typing<R: Rng + ?Sized>(
    text: &str,
    persona: &Persona,
    pace: Pace,
    rng: &mut R,
) -> Vec<Stroke> {
    let mut strokes = Vec::with_capacity(text.len());
    let mut previous: Option<char> = None;
    for ch in text.chars() {
        let delay_before = match (pace, previous) {
            (Pace::Fast, _) | (Pace::Human, None) => Duration::ZERO,
            (Pace::Human, Some(prev)) => {
                let mut median = persona.keystroke_median_ms;
                if prev.is_whitespace() || prev.is_ascii_punctuation() {
                    median *= 1.4;
                }
                if ch.is_ascii_uppercase() || SHIFTED.contains(ch) {
                    median += 35.0;
                }
                log_normal_ms(median, persona.keystroke_sigma, 30.0, 900.0, rng)
            }
        };
        let stroke = match char_key(ch) {
            Some(spec) => Stroke::Key {
                spec,
                delay_before,
                hold: match pace {
                    Pace::Fast => Duration::ZERO,
                    Pace::Human => {
                        log_normal_ms(persona.key_hold_median_ms, 0.25, 30.0, 200.0, rng)
                    }
                },
            },
            None => Stroke::Text {
                text: ch.to_string(),
                delay_before,
            },
        };
        strokes.push(stroke);
        previous = Some(ch);
    }
    // A rare longer pause mid-text: people glance away or think.
    if pace == Pace::Human && strokes.len() > 12 && rng.random_bool(0.3) {
        let at = rng.random_range(4..strokes.len() - 4);
        let (Stroke::Key { delay_before, .. } | Stroke::Text { delay_before, .. }) =
            &mut strokes[at];
        *delay_before += log_normal_ms(600.0, 0.5, 250.0, 2000.0, rng);
    }
    strokes
}

const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

/// Maps a character to the US-layout key that produces it.
fn char_key(ch: char) -> Option<KeySpec> {
    let plain = |key: &str, code: &str, key_code: u32, shift: bool| KeySpec {
        key: key.to_owned(),
        code: code.to_owned(),
        key_code,
        text: Some(ch.to_string()),
        modifiers: Modifiers(if shift { Modifiers::SHIFT } else { 0 }),
    };
    match ch {
        'a'..='z' => Some(plain(
            &ch.to_string(),
            &format!("Key{}", ch.to_ascii_uppercase()),
            ch.to_ascii_uppercase() as u32,
            false,
        )),
        'A'..='Z' => Some(plain(&ch.to_string(), &format!("Key{ch}"), ch as u32, true)),
        '0'..='9' => Some(plain(
            &ch.to_string(),
            &format!("Digit{ch}"),
            ch as u32,
            false,
        )),
        ' ' => Some(plain(" ", "Space", 32, false)),
        '\n' => named_key("Enter"),
        '\t' => named_key("Tab"),
        _ => PUNCTUATION
            .iter()
            .find(|(unshifted, shifted, ..)| *unshifted == ch || *shifted == ch)
            .map(|&(unshifted, _, code, key_code)| {
                plain(&ch.to_string(), code, key_code, ch != unshifted)
            }),
    }
}

/// (unshifted, shifted, code, windows key code) for the US layout's non-letter keys.
const PUNCTUATION: &[(char, char, &str, u32)] = &[
    ('1', '!', "Digit1", 49),
    ('2', '@', "Digit2", 50),
    ('3', '#', "Digit3", 51),
    ('4', '$', "Digit4", 52),
    ('5', '%', "Digit5", 53),
    ('6', '^', "Digit6", 54),
    ('7', '&', "Digit7", 55),
    ('8', '*', "Digit8", 56),
    ('9', '(', "Digit9", 57),
    ('0', ')', "Digit0", 48),
    ('-', '_', "Minus", 189),
    ('=', '+', "Equal", 187),
    ('[', '{', "BracketLeft", 219),
    (']', '}', "BracketRight", 221),
    ('\\', '|', "Backslash", 220),
    (';', ':', "Semicolon", 186),
    ('\'', '"', "Quote", 222),
    (',', '<', "Comma", 188),
    ('.', '>', "Period", 190),
    ('/', '?', "Slash", 191),
    ('`', '~', "Backquote", 192),
];

/// Non-printing keys by their `KeyboardEvent.key` name.
fn named_key(name: &str) -> Option<KeySpec> {
    let (key, code, key_code, text) = match name {
        "Enter" => ("Enter", "Enter", 13, Some("\r")),
        "Tab" => ("Tab", "Tab", 9, None),
        "Backspace" => ("Backspace", "Backspace", 8, None),
        "Delete" => ("Delete", "Delete", 46, None),
        "Escape" => ("Escape", "Escape", 27, None),
        "ArrowUp" => ("ArrowUp", "ArrowUp", 38, None),
        "ArrowDown" => ("ArrowDown", "ArrowDown", 40, None),
        "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37, None),
        "ArrowRight" => ("ArrowRight", "ArrowRight", 39, None),
        "Home" => ("Home", "Home", 36, None),
        "End" => ("End", "End", 35, None),
        "PageUp" => ("PageUp", "PageUp", 33, None),
        "PageDown" => ("PageDown", "PageDown", 34, None),
        "Space" => (" ", "Space", 32, Some(" ")),
        _ => return None,
    };
    Some(KeySpec {
        key: key.to_owned(),
        code: code.to_owned(),
        key_code,
        text: text.map(str::to_owned),
        modifiers: Modifiers::default(),
    })
}

/// Parses a chord such as "Enter", "Shift+Tab", "Meta+a" or "Control+Shift+K".
pub fn parse_chord(chord: &str) -> Option<KeySpec> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    // "Control++" means the plus key: the final empty segments collapse into one "+".
    if chord.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }
    let (key_part, modifier_parts) = parts.split_last()?;
    let mut modifiers = 0;
    for part in modifier_parts {
        modifiers |= match *part {
            "Alt" | "Option" => Modifiers::ALT,
            "Control" | "Ctrl" => Modifiers::CONTROL,
            "Meta" | "Cmd" | "Command" => Modifiers::META,
            "Shift" => Modifiers::SHIFT,
            _ => return None,
        };
    }
    let mut chars = key_part.chars();
    let mut spec = match (chars.next(), chars.next()) {
        (Some(ch), None) => char_key(ch)?,
        _ => named_key(key_part)?,
    };
    spec.modifiers = Modifiers(spec.modifiers.0 | modifiers);
    // With Control/Meta held, keys trigger shortcuts instead of inserting text.
    if spec.modifiers.has(Modifiers::CONTROL) || spec.modifiers.has(Modifiers::META) {
        spec.text = None;
    }
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn maps_ascii_and_falls_back_to_text_for_the_rest() {
        let mut rng = StdRng::seed_from_u64(1);
        let strokes = plan_typing("Hi!é", &Persona::default(), Pace::Human, &mut rng);
        let Stroke::Key { spec, .. } = &strokes[0] else {
            panic!("H is a key")
        };
        assert_eq!(
            (spec.code.as_str(), spec.modifiers.has(Modifiers::SHIFT)),
            ("KeyH", true)
        );
        let Stroke::Key { spec, .. } = &strokes[2] else {
            panic!("! is a key")
        };
        assert_eq!(spec.code, "Digit1");
        assert!(matches!(&strokes[3], Stroke::Text { text, .. } if text == "é"));
    }

    #[test]
    fn first_keystroke_is_immediate_then_gaps_are_human() {
        let mut rng = StdRng::seed_from_u64(2);
        let strokes = plan_typing("hello world", &Persona::default(), Pace::Human, &mut rng);
        let gaps: Vec<Duration> = strokes
            .iter()
            .map(|s| match s {
                Stroke::Key { delay_before, .. } | Stroke::Text { delay_before, .. } => {
                    *delay_before
                }
            })
            .collect();
        assert_eq!(gaps[0], Duration::ZERO);
        assert!(gaps[1..].iter().all(|g| *g >= Duration::from_millis(30)));
        assert!(gaps[1..].windows(2).any(|w| w[0] != w[1]), "gaps must vary");
    }

    #[test]
    fn parses_chords() {
        let spec = parse_chord("Meta+a").unwrap();
        assert!(spec.modifiers.has(Modifiers::META));
        assert_eq!(spec.text, None);
        assert_eq!(parse_chord("Shift+Tab").unwrap().code, "Tab");
        assert_eq!(parse_chord("Enter").unwrap().text.as_deref(), Some("\r"));
        assert!(parse_chord("Hyper+x").is_none());
    }
}
