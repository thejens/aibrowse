//! What a behavioral detector could conclude from the raw events our input produces.
//!
//! The event logger page (event_log.html) records every input event as a page script
//! sees it. This module turns that stream into simple statistics and flags *tells*:
//! properties no human-driven browser produces (untrusted events, clicks without a
//! preceding move) and *outliers*: values outside the ranges reported for people. The
//! ranges, and where they come from, are documented next to each check.

use serde::{Deserialize, Serialize};

use super::{keyboard, pointer, wheel};

/// One recorded event, with the fields the logger captures (absent ones default to 0).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Event {
    /// The DOM event type, e.g. "mousemove".
    pub kind: String,
    /// `event.timeStamp`, in milliseconds.
    pub t: f64,
    pub trusted: bool,
    /// The target element's id (or node name).
    pub target: String,
    pub x: f64,
    pub y: f64,
    pub screen_x: f64,
    pub screen_y: f64,
    pub movement_x: f64,
    pub movement_y: f64,
    pub button: i64,
    pub buttons: i64,
    pub detail: i64,
    pub pointer_type: String,
    pub pressure: f64,
    pub width: f64,
    pub height: f64,
    pub coalesced: u32,
    pub delta_x: f64,
    pub delta_y: f64,
    pub delta_mode: u32,
    pub key: String,
    pub code: String,
    pub key_code: u32,
    pub repeat: bool,
    pub shift: bool,
    pub input_type: String,
    pub data: String,
}

/// The window's geometry as the page sees it, for checking event screen coordinates.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowContext {
    pub screen_x: f64,
    pub screen_y: f64,
    pub outer_width: f64,
    pub outer_height: f64,
    pub inner_width: f64,
    pub inner_height: f64,
    pub device_pixel_ratio: f64,
}

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Something no person's browser produces; a detector can rely on it.
    Tell,
    /// Outside the usual human range; suspicious in aggregate, not proof on its own.
    Outlier,
}

/// One finding, with the numbers behind it.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// A stable name for the check, e.g. "straight_paths".
    pub check: &'static str,
    pub detail: String,
}

/// Statistics for one family of input, as `name -> value` (milliseconds, pixels or
/// ratios, as the name says), plus what was flagged.
#[derive(Debug, Default, Serialize)]
pub struct Analysis {
    pub counts: std::collections::BTreeMap<String, usize>,
    pub pointer: pointer::PointerStats,
    pub keyboard: keyboard::KeyboardStats,
    pub wheel: wheel::WheelStats,
    pub findings: Vec<Finding>,
}

impl Analysis {
    pub fn tells(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Tell)
            .count()
    }

    pub fn outliers(&self) -> usize {
        self.findings.len() - self.tells()
    }
}

/// Collects findings while the per-family analyses run.
#[derive(Default)]
pub struct Findings(pub Vec<Finding>);

impl Findings {
    pub fn tell(&mut self, check: &'static str, detail: String) {
        self.0.push(Finding {
            severity: Severity::Tell,
            check,
            detail,
        });
    }

    pub fn outlier(&mut self, check: &'static str, detail: String) {
        self.0.push(Finding {
            severity: Severity::Outlier,
            check,
            detail,
        });
    }
}

/// Analyzes a recorded event stream (in dispatch order).
pub fn analyze(events: &[Event], window: &WindowContext) -> Analysis {
    let mut findings = Findings::default();
    let mut counts = std::collections::BTreeMap::new();
    for event in events {
        *counts.entry(event.kind.clone()).or_default() += 1;
    }

    // Script-dispatched events (element.click(), dispatchEvent) carry isTrusted=false,
    // which any page can read. Real input never does.
    let untrusted: Vec<&Event> = events.iter().filter(|e| !e.trusted).collect();
    if let Some(first) = untrusted.first() {
        findings.tell(
            "untrusted_events",
            format!(
                "{} events have isTrusted=false (first: {} on {:?})",
                untrusted.len(),
                first.kind,
                first.target
            ),
        );
    }
    // Device input arrives in time order. Scroll events are excluded: they carry the
    // animation frame's time, so they legitimately interleave out of order with the
    // wheel events that caused them, in any browser.
    let input_times: Vec<f64> = events
        .iter()
        .filter(|e| e.kind != "scroll")
        .map(|e| e.t)
        .collect();
    if input_times.windows(2).any(|w| w[1] < w[0]) {
        findings.tell(
            "timestamps_not_monotonic",
            "input event timestamps go backwards".into(),
        );
    }

    let pointer = pointer::analyze(events, window, &mut findings);
    let keyboard = keyboard::analyze(events, &mut findings);
    let wheel = wheel::analyze(events, &mut findings);
    Analysis {
        counts,
        pointer,
        keyboard,
        wheel,
        findings: findings.0,
    }
}

/// The median of `values` (0 when empty).
pub fn median(values: &[f64]) -> f64 {
    quantile(values, 0.5)
}

/// The `q` quantile of `values` by nearest rank (0 when empty).
pub fn quantile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[rank]
}

/// Coefficient of variation (standard deviation over mean), 0 when undefined. It is how
/// "too regular" is measured throughout: human timing is never metronomic.
pub fn cv(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    if mean.abs() < f64::EPSILON {
        return 0.0;
    }
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    var.sqrt() / mean.abs()
}

/// Rounds to two decimals, so reports stay readable.
pub fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic event of `kind` at time `t`.
    pub fn ev(kind: &str, t: f64) -> Event {
        Event {
            kind: kind.into(),
            t,
            trusted: true,
            pointer_type: if kind.starts_with("pointer") {
                "mouse".into()
            } else {
                String::new()
            },
            ..Event::default()
        }
    }

    #[test]
    fn statistics_helpers() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[]), 0.0);
        assert_eq!(cv(&[5.0, 5.0, 5.0]), 0.0);
        assert!((cv(&[1.0, 3.0]) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9);
    }

    #[test]
    fn only_input_timestamps_must_be_monotonic() {
        let flagged = |events: &[Event]| {
            analyze(events, &WindowContext::default())
                .findings
                .iter()
                .any(|f| f.check == "timestamps_not_monotonic")
        };
        // A scroll stamped with its frame's time may precede the wheel that caused it.
        assert!(!flagged(&[
            ev("scroll", 10.0),
            ev("keydown", 12.0),
            ev("scroll", 11.0)
        ]));
        assert!(flagged(&[ev("keydown", 12.0), ev("keyup", 11.0)]));
    }

    #[test]
    fn untrusted_events_are_tells() {
        let mut click = ev("click", 10.0);
        click.trusted = false;
        let analysis = analyze(&[ev("keydown", 0.0), click], &WindowContext::default());
        assert!(
            analysis
                .findings
                .iter()
                .any(|f| f.check == "untrusted_events" && f.severity == Severity::Tell)
        );
    }
}
