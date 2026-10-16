//! The persona as a page sees it: the logged events measured with the same extractor
//! that measures the real user during handoffs (aibrowse_input::kinematics), next to
//! the persona parameters that produced them. A fitted persona should reproduce its own
//! parameters here, and so the user's recorded statistics.

use std::hash::{BuildHasher, RandomState};

use aibrowse_input::Persona;
use aibrowse_input::kinematics::{Extractor, KeyClass, KeyId, Kinematics, Sample};

use super::analysis::Event;

/// The logged events' kinematics. The logger doesn't know target sizes, so there is no
/// Fitts regression; everything else is measured exactly as for the user.
pub fn measure(events: &[Event]) -> Kinematics {
    let salt = RandomState::new();
    let mut extractor = Extractor::new();
    for e in events {
        let sample = match e.kind.as_str() {
            "mousemove" => Sample::Move {
                t: e.t,
                x: e.x,
                y: e.y,
            },
            "mousedown" if e.button == 0 => Sample::Press {
                t: e.t,
                x: e.x,
                y: e.y,
                target_width: None,
            },
            "mouseup" if e.button == 0 => Sample::Release { t: e.t },
            "wheel" => Sample::Wheel {
                t: e.t,
                dx: e.delta_x,
                dy: e.delta_y,
            },
            "keydown" => Sample::KeyDown {
                t: e.t,
                key: KeyId(salt.hash_one(&e.code)),
                class: KeyClass::of(&e.key),
            },
            "keyup" => Sample::KeyUp {
                t: e.t,
                key: KeyId(salt.hash_one(&e.code)),
            },
            _ => continue,
        };
        extractor.feed(sample);
    }
    extractor.finish()
}

/// "measured (persona)" pairs for the parameters a page can observe. Measured values run
/// a little above the persona's: each event also takes CDP's dispatch time.
pub fn compare(persona: &Persona, measured: &Kinematics) -> String {
    let p = &measured.pointer;
    let k = &measured.keys;
    let median = |h: &aibrowse_input::kinematics::Histogram| h.median().unwrap_or(0.0);
    let mut out = format!(
        "bow {:.1}% ({:.1}%), dwell {:.0}ms ({:.0}), press {:.0}ms ({:.0}), hold {:.0}ms ({:.0}), flight {:.0}ms ({:.0})",
        median(&p.bow) * 100.0,
        persona.path_bow * 100.0,
        median(&p.dwell_ms),
        persona.click_dwell_median_ms,
        median(&p.press_ms),
        persona.click_press_median_ms,
        median(&k.hold_ms),
        persona.key_hold_median_ms,
        median(&k.flight_ms),
        // Press to press: the planner holds each key, then waits the keystroke gap.
        persona.keystroke_median_ms + persona.key_hold_median_ms,
    );
    match measured.notch_mode() {
        Some((px, _)) => out.push_str(&format!(
            ", notch {px:.0}px ({:.0}), notch gap {:.0}ms ({:.0})",
            persona.wheel_notch_px,
            median(&measured.wheel.notch_gap_ms),
            persona.wheel_gap_median_ms
        )),
        None => out.push_str(&format!(
            ", trackpad gap {:.1}ms",
            median(&measured.wheel.trackpad_gap_ms)
        )),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::analysis::tests::ev;
    use super::*;

    #[test]
    fn logged_typing_is_measured_by_the_shared_extractor() {
        let mut events = Vec::new();
        for (i, ch) in "hello".chars().enumerate() {
            let t = i as f64 * 200.0;
            for (kind, at) in [("keydown", t), ("keyup", t + 100.0)] {
                let mut e = ev(kind, at);
                e.key = ch.to_string();
                e.code = format!("Key{}", ch.to_ascii_uppercase());
                events.push(e);
            }
        }
        let k = measure(&events);
        assert_eq!(k.keys.hold_ms.count(), 5);
        assert_eq!(k.keys.flight_ms.count(), 4);
        assert!((k.keys.flight_ms.median().unwrap() - 200.0).abs() < 2.0);
    }
}
