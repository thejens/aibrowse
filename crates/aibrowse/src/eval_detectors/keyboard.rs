//! Keystroke dynamics, as a behavioral detector would measure them.

use serde::Serialize;

use super::analysis::{Event, Findings, cv, median, quantile, round2};

/// Gaps longer than this end a typing burst; they are thinking, not typing rhythm.
const BURST_GAP_MS: f64 = 2000.0;

#[derive(Debug, Default, Serialize)]
pub struct KeyboardStats {
    pub keystrokes: usize,
    /// keydown → keyup of the same key.
    pub hold_ms_median: f64,
    pub hold_ms_min: f64,
    pub hold_cv: f64,
    /// keydown → next keydown within a burst (inter-key interval).
    pub iki_ms_median: f64,
    pub iki_cv: f64,
    /// Typing speed implied by the median interval (5 characters per word).
    pub wpm: f64,
}

const MODIFIERS: &[&str] = &["Shift", "Control", "Alt", "Meta"];

pub fn analyze(events: &[Event], findings: &mut Findings) -> KeyboardStats {
    let mut stats = KeyboardStats::default();
    let keys: Vec<&Event> = events
        .iter()
        .filter(|e| {
            (e.kind == "keydown" || e.kind == "keyup") && !MODIFIERS.contains(&e.key.as_str())
        })
        .collect();
    let mut holds = Vec::new();
    let mut downs = Vec::new();
    for (i, down) in keys.iter().enumerate().filter(|(_, e)| e.kind == "keydown") {
        downs.push(down.t);
        if let Some(up) = keys[i + 1..]
            .iter()
            .find(|e| e.kind == "keyup" && e.code == down.code)
        {
            holds.push(up.t - down.t);
        }
    }
    let iki: Vec<f64> = downs
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|dt| *dt <= BURST_GAP_MS)
        .collect();
    stats.keystrokes = downs.len();
    stats.hold_ms_median = round2(median(&holds));
    stats.hold_ms_min = round2(quantile(&holds, 0.0));
    stats.hold_cv = round2(cv(&holds));
    stats.iki_ms_median = round2(median(&iki));
    stats.iki_cv = round2(cv(&iki));
    if stats.iki_ms_median > 0.0 {
        stats.wpm = round2(60_000.0 / stats.iki_ms_median / 5.0);
    }

    consistency(events, findings);
    if holds.is_empty() {
        return stats;
    }
    if stats.hold_ms_min < 5.0 {
        findings.tell(
            "zero_duration_keypress",
            format!(
                "a key was released {:.1}ms after it was pressed",
                stats.hold_ms_min
            ),
        );
    }
    if iki.len() < 10 {
        return stats;
    }
    // Dhakal et al., "Observations on Typing from 136 Million Keystrokes" (CHI 2018):
    // mean inter-key interval 238ms (≈52 WPM), the fastest typists around 120ms, key
    // holds of roughly 70–130ms, and widely spread, right-skewed intervals.
    if !(60.0..=1000.0).contains(&stats.iki_ms_median) {
        findings.outlier(
            "typing_speed",
            format!(
                "median inter-key interval {:.0}ms ({:.0} WPM; people: ~120–500ms)",
                stats.iki_ms_median, stats.wpm
            ),
        );
    }
    if stats.iki_cv < 0.2 {
        findings.outlier(
            "typing_rhythm",
            format!(
                "inter-key intervals are too regular (CV {:.2}; people: 0.4–0.8)",
                stats.iki_cv
            ),
        );
    }
    if !(40.0..=200.0).contains(&stats.hold_ms_median) || stats.hold_cv < 0.1 {
        findings.outlier(
            "key_holds",
            format!(
                "key holds median {:.0}ms, CV {:.2} (people: 70–130ms, varied)",
                stats.hold_ms_median, stats.hold_cv
            ),
        );
    }
    stats
}

/// Properties real keyboards always have: a key event behind each typed character, a
/// key code, and Shift held for capitals.
fn consistency(events: &[Event], findings: &mut Findings) {
    let mut keyless = 0;
    let mut zero_code = 0;
    let mut unshifted = 0;
    for (i, event) in events.iter().enumerate() {
        match event.kind.as_str() {
            "input" if event.input_type == "insertText" && event.data.is_ascii() => {
                // The keydown that produced the character precedes its input event
                // within the same task.
                let typed = events[..i]
                    .iter()
                    .rev()
                    .take_while(|e| event.t - e.t < 50.0)
                    .any(|e| e.kind == "keydown");
                if !typed {
                    keyless += 1;
                }
            }
            "keydown" => {
                if event.key_code == 0 {
                    zero_code += 1;
                }
                let upper =
                    event.key.len() == 1 && event.key.chars().all(|c| c.is_ascii_uppercase());
                if upper && !event.shift {
                    unshifted += 1;
                }
            }
            _ => {}
        }
    }
    if keyless > 0 {
        findings.tell(
            "input_without_key",
            format!("{keyless} characters were inserted with no key press"),
        );
    }
    if zero_code > 0 {
        findings.tell(
            "key_code_zero",
            format!("{zero_code} keydown events have keyCode 0"),
        );
    }
    if unshifted > 0 {
        findings.tell(
            "capital_without_shift",
            format!("{unshifted} capitals were typed without Shift held"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::analysis::tests::ev;
    use super::super::analysis::{Event, Severity, WindowContext, analyze};

    fn key(kind: &str, t: f64, ch: char) -> Event {
        let mut e = ev(kind, t);
        e.key = ch.to_string();
        e.code = format!("Key{}", ch.to_ascii_uppercase());
        e.key_code = ch.to_ascii_uppercase() as u32;
        e.shift = ch.is_ascii_uppercase();
        e
    }

    /// Types `text` with the given gaps and holds (cycled).
    fn typed(text: &str, gaps: &[f64], holds: &[f64]) -> Vec<Event> {
        let mut events = Vec::new();
        let mut t = 0.0;
        for (i, ch) in text.chars().enumerate() {
            t += gaps[i % gaps.len()];
            events.push(key("keydown", t, ch));
            let mut input = ev("input", t + 1.0);
            input.input_type = "insertText".into();
            input.data = ch.to_string();
            events.push(input);
            events.push(key("keyup", t + holds[i % holds.len()], ch));
        }
        events
    }

    fn checks(events: &[Event]) -> Vec<(&'static str, Severity)> {
        analyze(events, &WindowContext::default())
            .findings
            .iter()
            .map(|f| (f.check, f.severity))
            .collect()
    }

    #[test]
    fn human_rhythm_passes() {
        let events = typed(
            "Hellothereworld",
            &[180.0, 95.0, 260.0, 140.0, 410.0, 120.0, 210.0],
            &[85.0, 110.0, 70.0, 130.0, 95.0],
        );
        assert!(checks(&events).is_empty(), "{:?}", checks(&events));
        let stats = analyze(&events, &WindowContext::default()).keyboard;
        assert_eq!(stats.keystrokes, 15);
        assert!(stats.wpm > 20.0 && stats.wpm < 120.0, "{stats:?}");
    }

    #[test]
    fn metronomic_and_instant_typing_is_flagged() {
        let events = typed("abcdefghijklmnop", &[30.0], &[0.0]);
        let found = checks(&events);
        assert!(
            found.contains(&("zero_duration_keypress", Severity::Tell)),
            "{found:?}"
        );
        assert!(
            found.contains(&("typing_speed", Severity::Outlier)),
            "{found:?}"
        );
        assert!(
            found.contains(&("typing_rhythm", Severity::Outlier)),
            "{found:?}"
        );
    }

    #[test]
    fn inserted_text_and_unshifted_capitals_are_tells() {
        let mut input = ev("input", 500.0);
        input.input_type = "insertText".into();
        input.data = "hello".into();
        let mut capital = key("keydown", 900.0, 'Q');
        capital.shift = false;
        let found = checks(&[input, capital]);
        assert!(
            found.contains(&("input_without_key", Severity::Tell)),
            "{found:?}"
        );
        assert!(
            found.contains(&("capital_without_shift", Severity::Tell)),
            "{found:?}"
        );
    }
}
