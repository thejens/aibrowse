//! Wheel and scroll behavior, as a behavioral detector would measure it.

use serde::Serialize;

use super::analysis::{Event, Findings, cv, median, round2};

/// Wheel events further apart than this belong to separate gestures.
const GESTURE_GAP_MS: f64 = 300.0;
/// A scroll with no wheel event this recently came from script or keyboard, not a hand.
const SCROLL_CAUSE_MS: f64 = 1000.0;

#[derive(Debug, Default, Serialize)]
pub struct WheelStats {
    pub ticks: usize,
    pub gestures: usize,
    /// Distinct |delta| values: many for a trackpad (momentum), one or two for a wheel.
    pub distinct_deltas: usize,
    pub delta_px_median: f64,
    pub delta_px_max: f64,
    pub interval_ms_median: f64,
    pub interval_cv: f64,
    /// "trackpad", "wheel" or "neither", judged from the deltas.
    pub device: String,
    /// Scroll events with no wheel input shortly before them.
    pub scrolls_without_wheel: usize,
}

pub fn analyze(events: &[Event], findings: &mut Findings) -> WheelStats {
    let mut stats = WheelStats::default();
    let wheels: Vec<&Event> = events.iter().filter(|e| e.kind == "wheel").collect();
    stats.scrolls_without_wheel = events
        .iter()
        .filter(|e| e.kind == "scroll")
        .filter(|s| {
            !wheels
                .iter()
                .any(|w| w.t <= s.t && s.t - w.t <= SCROLL_CAUSE_MS)
        })
        .count();
    if stats.scrolls_without_wheel > 0 {
        findings.outlier(
            "scroll_without_wheel",
            format!(
                "{} scroll events had no wheel input before them (programmatic scrolling)",
                stats.scrolls_without_wheel
            ),
        );
    }
    if wheels.is_empty() {
        return stats;
    }

    let magnitudes: Vec<f64> = wheels
        .iter()
        .map(|w| w.delta_x.abs().max(w.delta_y.abs()))
        .collect();
    let mut distinct: Vec<i64> = magnitudes
        .iter()
        .map(|m| (m * 10.0).round() as i64)
        .collect();
    distinct.sort_unstable();
    distinct.dedup();
    let intervals: Vec<f64> = wheels
        .windows(2)
        .map(|w| w[1].t - w[0].t)
        .filter(|dt| *dt <= GESTURE_GAP_MS)
        .collect();
    stats.ticks = wheels.len();
    stats.gestures = wheels.len() - intervals.len();
    stats.distinct_deltas = distinct.len();
    stats.delta_px_median = round2(median(&magnitudes));
    stats.delta_px_max = round2(magnitudes.iter().copied().fold(0.0, f64::max));
    stats.interval_ms_median = round2(median(&intervals));
    stats.interval_cv = round2(cv(&intervals));
    // Chrome on macOS reports trackpads as bursts of small, varied pixel deltas
    // (momentum decaying to 1–2px) and notched wheels as runs of one fixed step. A
    // stream that is neither, such as one huge delta, has no physical device behind it.
    stats.device = if stats.distinct_deltas >= 5 && stats.delta_px_median < 60.0 {
        "trackpad"
    } else if stats.distinct_deltas <= 2 && stats.delta_px_median >= 40.0 {
        "wheel"
    } else {
        "neither"
    }
    .into();

    if wheels.iter().any(|w| w.delta_mode != 0) {
        findings.tell(
            "wheel_delta_mode",
            "wheel events in line/page mode (Chrome always reports pixels)".into(),
        );
    }
    if stats.device == "neither" {
        findings.outlier(
            "wheel_device",
            format!(
                "wheel deltas fit neither a trackpad nor a notched wheel ({} distinct, median {:.0}px, max {:.0}px)",
                stats.distinct_deltas, stats.delta_px_median, stats.delta_px_max
            ),
        );
    }
    if intervals.len() >= 5 && stats.interval_cv < 0.05 {
        findings.outlier(
            "periodic_wheel",
            format!(
                "wheel events arrive at metronomic intervals (CV {:.3})",
                stats.interval_cv
            ),
        );
    }
    // macOS delivers trackpad scroll (and its momentum phase) once per display frame:
    // every 8–17ms on 60–120Hz panels. Much slower streams are a stuttering replay.
    if stats.device == "trackpad" && intervals.len() >= 5 && stats.interval_ms_median > 25.0 {
        findings.outlier(
            "trackpad_rate",
            format!(
                "trackpad wheel events every {:.0}ms (displays: 8–17ms)",
                stats.interval_ms_median
            ),
        );
    }
    // The wheel scrolls whatever is under the cursor, so its position is the cursor's.
    let mut misplaced = 0;
    for (i, wheel) in events.iter().enumerate().filter(|(_, e)| e.kind == "wheel") {
        let cursor = events[..i]
            .iter()
            .rev()
            .find(|e| matches!(e.kind.as_str(), "mousemove" | "mousedown" | "mouseup"));
        if cursor.is_none_or(|c| (c.x - wheel.x).abs() > 1.0 || (c.y - wheel.y).abs() > 1.0) {
            misplaced += 1;
        }
    }
    if misplaced > 0 {
        findings.tell(
            "wheel_without_cursor",
            format!("{misplaced} wheel events happened away from where the cursor was"),
        );
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::super::analysis::tests::ev;
    use super::super::analysis::{Event, Severity, WindowContext, analyze};

    fn wheel(t: f64, dy: f64) -> Event {
        let mut e = ev("wheel", t);
        (e.x, e.y, e.delta_y) = (300.0, 200.0, dy);
        e
    }

    fn cursor() -> Event {
        let mut e = ev("mousemove", 0.0);
        (e.x, e.y) = (300.0, 200.0);
        e
    }

    fn analyzed(events: &[Event]) -> (String, Vec<(&'static str, Severity)>) {
        let analysis = analyze(events, &WindowContext::default());
        let found = analysis
            .findings
            .iter()
            .map(|f| (f.check, f.severity))
            .collect();
        (analysis.wheel.device, found)
    }

    #[test]
    fn trackpad_momentum_and_wheel_notches_are_recognized() {
        let mut pad = vec![cursor()];
        let deltas = [
            12.0, 31.0, 44.0, 37.0, 28.0, 21.0, 15.0, 10.0, 7.0, 4.0, 2.0, 1.0,
        ];
        let mut t = 100.0;
        for (i, d) in deltas.iter().enumerate() {
            t += 16.7 + if i % 2 == 0 { 2.1 } else { -1.4 };
            pad.push(wheel(t, *d));
        }
        let (device, found) = analyzed(&pad);
        assert_eq!(device, "trackpad");
        assert!(found.is_empty(), "{found:?}");

        let mut notches = vec![cursor()];
        for (i, gap) in [0.0, 48.0, 81.0, 39.0, 66.0].iter().enumerate() {
            notches.push(wheel(100.0 + gap + 10.0 * i as f64, 100.0));
        }
        assert_eq!(analyzed(&notches).0, "wheel");
    }

    #[test]
    fn jump_scrolls_and_detached_wheels_are_flagged() {
        let mut jump = wheel(100.0, 600.0);
        jump.x = 10.0;
        let (device, found) = analyzed(&[cursor(), jump, wheel(105.0, 3.0), wheel(110.0, 5.0)]);
        assert_eq!(device, "neither");
        assert!(
            found.contains(&("wheel_device", Severity::Outlier)),
            "{found:?}"
        );
        assert!(
            found.contains(&("wheel_without_cursor", Severity::Tell)),
            "{found:?}"
        );
    }

    #[test]
    fn slow_trackpad_streams_are_flagged() {
        let mut pad = vec![cursor()];
        let deltas = [12.0, 31.0, 44.0, 37.0, 28.0, 21.0, 15.0, 10.0, 7.0, 4.0];
        for (i, d) in deltas.iter().enumerate() {
            pad.push(wheel(100.0 + 33.0 * i as f64 + (i % 3) as f64 * 2.0, *d));
        }
        let (device, found) = analyzed(&pad);
        assert_eq!(device, "trackpad");
        assert!(
            found.contains(&("trackpad_rate", Severity::Outlier)),
            "{found:?}"
        );
    }

    #[test]
    fn scroll_without_wheel_is_flagged() {
        let (_, found) = analyzed(&[ev("scroll", 5000.0)]);
        assert!(
            found.contains(&("scroll_without_wheel", Severity::Outlier)),
            "{found:?}"
        );
    }
}
