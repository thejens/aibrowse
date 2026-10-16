//! Kinematics of the real user's own input, recorded during handoffs (opt-in) so the
//! persona can be fitted to them (docs/design.md §5.9, "Learn it from the real user").
//!
//! Privacy is structural, not a filter applied afterwards: what goes in is a sanitized
//! [`Sample`] stream (positions and times; keys only as a coarse [`KeyClass`] plus an
//! opaque id for pairing a press with its release), and what comes out and is stored is
//! [`Kinematics`]: histograms and regression sums with no order, no positions and no
//! key identities. Nothing typed can be reconstructed from it, not even its length per
//! field: flights from every handoff are pooled into one histogram.

mod extract;
mod fit;
mod stats;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use extract::Extractor;
pub use fit::{Fit, ParamFit, fit};
pub use stats::{Histogram, Regression};

/// One input event from the user, already reduced to what kinematics need. Times are
/// the page's own event timestamps in milliseconds, so network jitter between the user's
/// browser and ours doesn't distort them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sample {
    Move {
        t: f64,
        x: f64,
        y: f64,
    },
    /// A primary-button press. `target_width` is the smaller side of the element under
    /// the press, when it could be determined: the W of Fitts's law.
    Press {
        t: f64,
        x: f64,
        y: f64,
        target_width: Option<f64>,
    },
    Release {
        t: f64,
    },
    Wheel {
        t: f64,
        dx: f64,
        dy: f64,
    },
    KeyDown {
        t: f64,
        key: KeyId,
        class: KeyClass,
    },
    KeyUp {
        t: f64,
        key: KeyId,
    },
}

/// An opaque per-recording key id, used only to pair a key's press with its release
/// (keys overlap in fast typing). The recorder derives it with a per-session random
/// salt, and it is never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyId(pub u64);

/// The most a recording learns about which key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    /// Anything that types one character (letters, digits, punctuation).
    Character,
    Space,
    /// Backspace, Delete, Enter, Tab: corrections and field changes, which break the
    /// typing rhythm rather than belong to it.
    Edit,
    Modifier,
    /// Arrows, function keys, Escape and the like.
    Other,
}

impl KeyClass {
    /// Classifies a `KeyboardEvent.key` value. The key itself goes no further.
    pub fn of(key: &str) -> Self {
        match key {
            " " => Self::Space,
            "Backspace" | "Delete" | "Enter" | "Tab" => Self::Edit,
            "Shift" | "Control" | "Alt" | "Meta" | "CapsLock" | "AltGraph" | "Fn" => Self::Modifier,
            _ if key.chars().count() == 1 => Self::Character,
            _ => Self::Other,
        }
    }
}

/// Everything recorded for one profile, accumulated over handoffs (`kinematics.json` in
/// the profile directory).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kinematics {
    /// Handoffs that contributed samples.
    pub handoffs: u64,
    pub pointer: PointerKinematics,
    pub keys: KeyKinematics,
    pub wheel: WheelKinematics,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointerKinematics {
    /// Movement time (ms) against Fitts's index of difficulty, log2(D/W + 1), over aimed
    /// strokes that ended in a click on a target of known size.
    pub fitts: Regression,
    /// Peak sideways deviation from the start→end line over the distance (strokes ≥100px).
    pub bow: Histogram,
    /// When peak speed occurs, as a fraction of the stroke's duration.
    pub peak_at: Histogram,
    /// Aimed strokes ending in a click, and how many of them went past the target first.
    pub aimed: u64,
    pub overshoots: u64,
    /// Last movement to button press, and press to release (ms).
    pub dwell_ms: Histogram,
    pub press_ms: Histogram,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyKinematics {
    /// How long typing keys are held (ms).
    pub hold_ms: Histogram,
    /// Press to next press within a word, and after a space (ms).
    pub flight_ms: Histogram,
    pub flight_after_space_ms: Histogram,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WheelKinematics {
    /// Scroll gestures by the device that produced them, judged from deltas and cadence.
    pub trackpad_gestures: u64,
    pub notched_gestures: u64,
    pub unclassified_gestures: u64,
    /// |delta| (px, rounded) of each event in a notched gesture, by count: one value for
    /// a fixed-step wheel, several when the OS accelerates fast spins.
    pub notch_px: BTreeMap<u32, u64>,
    /// Gaps between events within notched and within trackpad gestures (ms).
    pub notch_gap_ms: Histogram,
    pub trackpad_gap_ms: Histogram,
    /// Events per trackpad gesture (finger scroll plus momentum).
    pub trackpad_events: Histogram,
}

impl Kinematics {
    /// Adds another recording's statistics to these.
    pub fn merge(&mut self, other: &Kinematics) {
        self.handoffs += other.handoffs;
        let (p, o) = (&mut self.pointer, &other.pointer);
        p.fitts.merge(&o.fitts);
        p.bow.merge(&o.bow);
        p.peak_at.merge(&o.peak_at);
        p.aimed += o.aimed;
        p.overshoots += o.overshoots;
        p.dwell_ms.merge(&o.dwell_ms);
        p.press_ms.merge(&o.press_ms);
        let (k, o) = (&mut self.keys, &other.keys);
        k.hold_ms.merge(&o.hold_ms);
        k.flight_ms.merge(&o.flight_ms);
        k.flight_after_space_ms.merge(&o.flight_after_space_ms);
        let (w, o) = (&mut self.wheel, &other.wheel);
        w.trackpad_gestures += o.trackpad_gestures;
        w.notched_gestures += o.notched_gestures;
        w.unclassified_gestures += o.unclassified_gestures;
        for (px, n) in &o.notch_px {
            *w.notch_px.entry(*px).or_default() += n;
        }
        w.notch_gap_ms.merge(&o.notch_gap_ms);
        w.trackpad_gap_ms.merge(&o.trackpad_gap_ms);
        w.trackpad_events.merge(&o.trackpad_events);
    }

    /// Whether anything at all was recorded.
    pub fn is_empty(&self) -> bool {
        self.pointer.fitts.n == 0
            && self.pointer.bow.count() == 0
            && self.pointer.press_ms.count() == 0
            && self.keys.hold_ms.count() == 0
            && self.keys.flight_ms.count() == 0
            && self.wheel.trackpad_gestures + self.wheel.notched_gestures == 0
    }

    /// The most common notch size and the share of notched events that had it.
    pub fn notch_mode(&self) -> Option<(f64, f64)> {
        let total: u64 = self.wheel.notch_px.values().sum();
        let (px, n) = self.wheel.notch_px.iter().max_by_key(|(_, n)| **n)?;
        Some((f64::from(*px), *n as f64 / total as f64))
    }

    /// One line per family of the medians, for people reading a report.
    pub fn summary(&self) -> Vec<String> {
        let p = &self.pointer;
        let k = &self.keys;
        let w = &self.wheel;
        let fitts = match p.fitts.line() {
            Some((a, b)) => format!("MT ≈ {a:.0} + {b:.0}·ID ms"),
            None => "MT not fittable yet".to_owned(),
        };
        let notch = self
            .notch_mode()
            .map(|(px, share)| {
                format!(
                    ", notch {px:.0}px ({:.0}% of notched events)",
                    share * 100.0
                )
            })
            .unwrap_or_default();
        vec![
            format!(
                "pointer: {} aimed strokes ({fitts}), bow {:.1}% (n={}), peak speed at {:.0}%, overshoot {}/{}",
                p.fitts.n,
                p.bow.median().unwrap_or(0.0) * 100.0,
                p.bow.count(),
                p.peak_at.median().unwrap_or(0.0) * 100.0,
                p.overshoots,
                p.aimed,
            ),
            format!(
                "clicks: dwell {:.0}ms (n={}), press {:.0}ms (n={})",
                p.dwell_ms.median().unwrap_or(0.0),
                p.dwell_ms.count(),
                p.press_ms.median().unwrap_or(0.0),
                p.press_ms.count(),
            ),
            format!(
                "keys: hold {:.0}ms (n={}), flight {:.0}ms (σ {:.2}, n={}), after space {:.0}ms (n={})",
                k.hold_ms.median().unwrap_or(0.0),
                k.hold_ms.count(),
                k.flight_ms.median().unwrap_or(0.0),
                k.flight_ms.log_sigma().unwrap_or(0.0),
                k.flight_ms.count(),
                k.flight_after_space_ms.median().unwrap_or(0.0),
                k.flight_after_space_ms.count(),
            ),
            format!(
                "wheel: {} trackpad / {} notched / {} unclassified gestures{notch}, notch gap {:.0}ms, trackpad gap {:.1}ms",
                w.trackpad_gestures,
                w.notched_gestures,
                w.unclassified_gestures,
                w.notch_gap_ms.median().unwrap_or(0.0),
                w.trackpad_gap_ms.median().unwrap_or(0.0),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_reduce_to_classes() {
        assert_eq!(KeyClass::of("a"), KeyClass::Character);
        assert_eq!(KeyClass::of("7"), KeyClass::Character);
        assert_eq!(KeyClass::of("é"), KeyClass::Character);
        assert_eq!(KeyClass::of(" "), KeyClass::Space);
        assert_eq!(KeyClass::of("Backspace"), KeyClass::Edit);
        assert_eq!(KeyClass::of("Shift"), KeyClass::Modifier);
        assert_eq!(KeyClass::of("ArrowLeft"), KeyClass::Other);
    }

    #[test]
    fn merging_adds_up() {
        let mut a = Kinematics {
            handoffs: 1,
            ..Kinematics::default()
        };
        a.keys.hold_ms.add(80.0);
        a.wheel.notch_px.insert(100, 3);
        let mut b = a.clone();
        b.wheel.notch_px.insert(120, 1);
        a.merge(&b);
        assert_eq!(a.handoffs, 2);
        assert_eq!(a.keys.hold_ms.count(), 2);
        assert_eq!(a.wheel.notch_px[&100], 6);
        assert_eq!(a.notch_mode(), Some((100.0, 6.0 / 7.0)));
    }
}
