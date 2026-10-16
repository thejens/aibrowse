//! Pointer kinematics (path shape, speed, event rate, coordinates), as a behavioral
//! detector would measure them. Click mechanics are checked in `clicks`.

use serde::Serialize;

use super::analysis::{Event, Findings, WindowContext, cv, median, round2};
use super::clicks;

/// Moves further apart than this belong to different strokes: a person who pauses this
/// long has stopped moving, and the next move is a new aimed movement.
const STROKE_GAP_MS: f64 = 120.0;
/// Only strokes at least this long (in px, with this many events) say anything about
/// path shape; short corrective nudges are legitimately straight.
const MIN_STROKE_PX: f64 = 100.0;
const MIN_STROKE_EVENTS: usize = 6;

#[derive(Debug, Default, Serialize)]
pub struct PointerStats {
    /// Aimed movements long enough to judge (≥100px).
    pub strokes: usize,
    /// Chord length over path length per stroke; 1.0 is a perfectly straight line.
    pub straightness_median: f64,
    /// Largest distance (px) of a stroke's samples from its start→end line.
    pub deviation_px_median: f64,
    pub deviation_px_max: f64,
    /// Coefficient of variation of speed within a stroke (0 = constant velocity).
    pub speed_cv_median: f64,
    /// When in the stroke the peak speed occurs, as a fraction of its duration.
    pub peak_at_median: f64,
    pub duration_ms_median: f64,
    pub move_interval_ms_median: f64,
    pub move_interval_cv: f64,
    /// Moves whose movementX/Y disagree with the change in screen position.
    pub movement_mismatch_ratio: f64,
    /// screenX − clientX and screenY − clientY (constant for a real window).
    pub screen_offset: (f64, f64),
    pub clicks: usize,
    /// Last move to button press.
    pub dwell_ms_median: f64,
    /// Button press to release.
    pub press_ms_median: f64,
    pub press_ms_min: f64,
}

struct Stroke<'a> {
    /// Where the cursor rested before the stroke began, when known.
    origin: Option<(f64, f64)>,
    moves: Vec<&'a Event>,
}

pub fn analyze(events: &[Event], window: &WindowContext, findings: &mut Findings) -> PointerStats {
    let mut stats = PointerStats::default();
    let mouse: Vec<&Event> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "mousemove" | "mousedown" | "mouseup" | "wheel"
            )
        })
        .collect();
    kinematics(&mouse, &mut stats, findings);
    move_timing(&mouse, &mut stats, findings);
    coordinates(&mouse, window, &mut stats, findings);
    clicks::clicks(events, &mut stats, findings);
    clicks::pointer_fields(events, findings);
    stats
}

fn strokes<'a>(mouse: &[&'a Event]) -> Vec<Stroke<'a>> {
    let mut strokes = Vec::new();
    let mut current: Option<Stroke<'a>> = None;
    let mut rest: Option<(f64, f64)> = None;
    for event in mouse {
        if event.kind != "mousemove" {
            strokes.extend(current.take());
            rest = Some((event.x, event.y));
            continue;
        }
        match &mut current {
            Some(stroke) if event.t - stroke.moves.last().map_or(0.0, |m| m.t) <= STROKE_GAP_MS => {
                stroke.moves.push(event);
            }
            _ => {
                if let Some(done) = current.take() {
                    rest = done.moves.last().map(|m| (m.x, m.y));
                    strokes.push(done);
                }
                current = Some(Stroke {
                    origin: rest,
                    moves: vec![event],
                });
            }
        }
    }
    strokes.extend(current);
    strokes
}

fn kinematics(mouse: &[&Event], stats: &mut PointerStats, findings: &mut Findings) {
    let mut straightness = Vec::new();
    let mut deviation = Vec::new();
    let mut speed_cv = Vec::new();
    let mut peak_at = Vec::new();
    let mut durations = Vec::new();
    for stroke in strokes(mouse) {
        let mut points: Vec<(f64, f64, f64)> = Vec::new();
        if let (Some((x, y)), Some(first)) = (stroke.origin, stroke.moves.first()) {
            // The resting point has no timestamp of its own; one frame before the first
            // move is where a real movement's first sample would sit.
            points.push((x, y, first.t - 16.7));
        }
        points.extend(stroke.moves.iter().map(|m| (m.x, m.y, m.t)));
        let path: f64 = points
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
            .sum();
        if path < MIN_STROKE_PX || points.len() < MIN_STROKE_EVENTS {
            continue;
        }
        let (start, end) = (points[0], points[points.len() - 1]);
        let chord = (end.0 - start.0).hypot(end.1 - start.1);
        straightness.push(chord / path);
        // Perpendicular distance of each sample from the start→end line.
        let off_line = points
            .iter()
            .map(|p| {
                ((end.0 - start.0) * (start.1 - p.1) - (start.0 - p.0) * (end.1 - start.1)).abs()
                    / chord.max(1.0)
            })
            .fold(0.0, f64::max);
        deviation.push(off_line);
        let speeds: Vec<(f64, f64)> = points
            .windows(2)
            .filter(|w| w[1].2 > w[0].2)
            .map(|w| {
                let d = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
                (w[1].2, d / (w[1].2 - w[0].2))
            })
            .collect();
        let values: Vec<f64> = speeds.iter().map(|s| s.1).collect();
        speed_cv.push(cv(&values));
        let duration = end.2 - start.2;
        durations.push(duration);
        if let Some(peak) = speeds.iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
            peak_at.push((peak.0 - start.2) / duration.max(1.0));
        }
    }
    stats.strokes = straightness.len();
    stats.straightness_median = round2(median(&straightness));
    stats.deviation_px_median = round2(median(&deviation));
    stats.deviation_px_max = round2(deviation.iter().copied().fold(0.0, f64::max));
    stats.speed_cv_median = round2(median(&speed_cv));
    stats.peak_at_median = round2(median(&peak_at));
    stats.duration_ms_median = round2(median(&durations));
    if stats.strokes < 3 {
        return;
    }
    // Human aimed movements curve: the path bows several pixels off the straight line,
    // more for long moves and with a trackpad (MacKenzie, Kauppinen & Silfverberg,
    // "Accuracy measures for evaluating computer pointing devices", CHI 2001, report
    // movement offset and variability of several px over a few hundred px). Integer
    // client coordinates alone explain up to ~1px; nothing more, on every long stroke,
    // means the path was interpolated along a line.
    if stats.deviation_px_max <= 2.0 {
        findings.tell(
            "straight_paths",
            format!(
                "all {} strokes are straight lines (no sample more than {:.1}px off the line)",
                stats.strokes, stats.deviation_px_max
            ),
        );
    }
    // Reaching movements accelerate and decelerate (a bell-shaped speed profile; Flash
    // & Hogan 1985), so speed varies a lot within a stroke: CVs of 0.5–1.0 are typical.
    if stats.speed_cv_median < 0.2 {
        findings.tell(
            "constant_velocity",
            format!(
                "speed barely varies within strokes (median CV {:.2})",
                stats.speed_cv_median
            ),
        );
    }
    // The peak usually comes in the first half (a short acceleration, a long homing-in
    // phase); a peak at the very start or end means no ramp at all.
    if !(0.15..=0.75).contains(&stats.peak_at_median) {
        findings.outlier(
            "speed_peak_position",
            format!(
                "peak speed at {:.0}% of the stroke (people: roughly 30–60%)",
                stats.peak_at_median * 100.0
            ),
        );
    }
}

fn move_timing(mouse: &[&Event], stats: &mut PointerStats, findings: &mut Findings) {
    let intervals: Vec<f64> = mouse
        .windows(2)
        .filter(|w| w[0].kind == "mousemove" && w[1].kind == "mousemove")
        .map(|w| w[1].t - w[0].t)
        .filter(|dt| *dt <= STROKE_GAP_MS)
        .collect();
    stats.move_interval_ms_median = round2(median(&intervals));
    stats.move_interval_cv = round2(cv(&intervals));
    if intervals.len() < 20 {
        return;
    }
    // Chrome delivers mousemove once per frame (rAF-aligned) at the display's refresh
    // rate: 60–120Hz on Macs, so 8–17ms, with the hardware's own sampling jitter on top.
    if !(4.0..=40.0).contains(&stats.move_interval_ms_median) {
        findings.outlier(
            "move_rate",
            format!(
                "median mousemove interval {:.1}ms (displays: 8–17ms)",
                stats.move_interval_ms_median
            ),
        );
    }
    if stats.move_interval_cv < 0.02 {
        findings.outlier(
            "periodic_moves",
            format!(
                "mousemove intervals are metronomic (CV {:.3})",
                stats.move_interval_cv
            ),
        );
    }
}

fn coordinates(
    mouse: &[&Event],
    window: &WindowContext,
    stats: &mut PointerStats,
    findings: &mut Findings,
) {
    let moves: Vec<&&Event> = mouse.iter().filter(|e| e.kind == "mousemove").collect();
    let Some(first) = moves.first() else { return };
    let offset = (first.screen_x - first.x, first.screen_y - first.y);
    stats.screen_offset = (round2(offset.0), round2(offset.1));
    if mouse.iter().any(|e| {
        ((e.screen_x - e.x) - offset.0).abs() > 1.5 || ((e.screen_y - e.y) - offset.1).abs() > 1.5
    }) {
        findings.tell(
            "screen_offset_varies",
            "screenX−clientX is not constant across events".into(),
        );
    }
    // The page's top-left on screen is the window position plus the browser chrome
    // above it. Screen coordinates equal to client coordinates (offset 0) while the
    // window has a toolbar mean the coordinates weren't derived from a real window.
    let chrome_height = window.outer_height - window.inner_height;
    let expected_y = window.screen_y + chrome_height;
    if window.outer_height > 0.0 && (offset.1 - expected_y).abs() > 2.0 {
        findings.tell(
            "screen_coordinates",
            format!(
                "screenY−clientY is {:.0}, but the window says the viewport starts at {expected_y:.0} (screenY {:.0} + {chrome_height:.0}px of browser UI)",
                offset.1, window.screen_y
            ),
        );
    }

    let mut moved = 0usize;
    let mut zero = 0usize;
    let mut mismatch = 0usize;
    for w in moves.windows(2) {
        let (dx, dy) = (w[1].screen_x - w[0].screen_x, w[1].screen_y - w[0].screen_y);
        if dx.abs() < 1.0 && dy.abs() < 1.0 {
            continue;
        }
        moved += 1;
        let (mx, my) = (w[1].movement_x, w[1].movement_y);
        if mx == 0.0 && my == 0.0 {
            zero += 1;
        }
        // movementX/Y are in screen pixels on macOS with HiDPI scaling, so allow either
        // CSS-pixel or device-pixel agreement.
        let dpr = window.device_pixel_ratio.max(1.0);
        let agrees = |m: f64, d: f64| (m - d).abs() <= 2.0 || (m - d * dpr).abs() <= 2.0 * dpr;
        if !(agrees(mx, dx) && agrees(my, dy)) {
            mismatch += 1;
        }
    }
    if moved == 0 {
        return;
    }
    stats.movement_mismatch_ratio = round2(mismatch as f64 / moved as f64);
    // Real mice report the relative motion of every move; CDP-dispatched moves must too.
    if zero * 10 >= moved * 9 {
        findings.tell(
            "movement_zero",
            format!("movementX/Y are 0 on {zero} of {moved} moves that changed position"),
        );
    } else if stats.movement_mismatch_ratio > 0.5 {
        findings.outlier(
            "movement_mismatch",
            format!(
                "movementX/Y disagree with the screen-position change on {mismatch} of {moved} moves"
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::analysis::tests::ev;
    use super::super::analysis::{Event, Severity, WindowContext, analyze};

    fn at(kind: &str, t: f64, x: f64, y: f64) -> Event {
        let mut e = ev(kind, t);
        (e.x, e.y) = (x, y);
        (e.screen_x, e.screen_y) = (x + 100.0, y + 187.0);
        e
    }

    fn window() -> WindowContext {
        WindowContext {
            screen_x: 100.0,
            screen_y: 100.0,
            outer_height: 887.0,
            inner_height: 800.0,
            device_pixel_ratio: 1.0,
            ..WindowContext::default()
        }
    }

    /// A stroke from (x0, 100) to (x0 + 400, 100) at `frame` ms per event, following
    /// `progress` (0..1 → 0..1) and bowing `bow` px off the line at mid-stroke.
    fn stroke(t0: f64, x0: f64, frame: f64, bow: f64, progress: fn(f64) -> f64) -> Vec<Event> {
        let n = 30;
        let mut out = Vec::new();
        let (mut px, mut py) = (x0, 100.0);
        for i in 1..=n {
            let tau = f64::from(i) / f64::from(n);
            let p = progress(tau);
            let jitter = if i % 3 == 0 { 1.3 } else { 0.0 };
            let (x, y) = (
                x0 + 400.0 * p,
                100.0 + bow * (std::f64::consts::PI * p).sin(),
            );
            let mut e = at("mousemove", t0 + f64::from(i) * frame + jitter, x, y);
            (e.movement_x, e.movement_y) = (x - px, y - py);
            (px, py) = (x, y);
            out.push(e);
        }
        out
    }

    fn min_jerk(t: f64) -> f64 {
        t * t * t * (10.0 - 15.0 * t + 6.0 * t * t)
    }

    fn click_at(t: f64, x: f64, y: f64, hold: f64) -> Vec<Event> {
        let mut down = at("mousedown", t, x, y);
        down.buttons = 1;
        let up = at("mouseup", t + hold, x, y);
        vec![down, up]
    }

    fn session(bow: f64, progress: fn(f64) -> f64, hold: f64) -> Vec<Event> {
        let mut events = vec![at("mousemove", 0.0, 0.0, 100.0)];
        for k in 0..4 {
            let t0 = 1000.0 * f64::from(k);
            let x0 = if k % 2 == 0 { 0.0 } else { 400.0 };
            let mut s = stroke(t0, x0, 16.7, bow, progress);
            if k % 2 == 1 {
                // Back leftwards, so strokes chain end to start.
                for e in &mut s {
                    e.x = 800.0 - e.x;
                    e.screen_x = e.x + 100.0;
                    e.movement_x = -e.movement_x;
                }
            }
            let last = s.last().unwrap().clone();
            events.extend(s);
            events.extend(click_at(last.t + 120.0, last.x, last.y, hold));
        }
        events
    }

    fn checks(events: &[Event]) -> Vec<(&'static str, Severity)> {
        analyze(events, &window())
            .findings
            .iter()
            .map(|f| (f.check, f.severity))
            .collect()
    }

    #[test]
    fn humanlike_strokes_pass() {
        let events = session(18.0, min_jerk, 95.0);
        let found = checks(&events);
        assert!(found.is_empty(), "{found:?}");
        let stats = analyze(&events, &window()).pointer;
        assert_eq!(stats.clicks, 4);
        assert!(stats.speed_cv_median > 0.4, "{stats:?}");
    }

    #[test]
    fn straight_constant_velocity_lines_are_tells() {
        let found = checks(&session(0.0, |t| t, 95.0));
        assert!(
            found.contains(&("straight_paths", Severity::Tell)),
            "{found:?}"
        );
        assert!(
            found.contains(&("constant_velocity", Severity::Tell)),
            "{found:?}"
        );
    }

    #[test]
    fn teleport_clicks_and_instant_presses_are_tells() {
        let mut events = session(18.0, min_jerk, 0.0);
        events.extend(click_at(9000.0, 5.0, 5.0, 80.0));
        let found = checks(&events);
        assert!(
            found.contains(&("click_without_move", Severity::Tell)),
            "{found:?}"
        );
        assert!(
            found.contains(&("zero_duration_press", Severity::Tell)),
            "{found:?}"
        );
    }

    #[test]
    fn zero_movement_and_client_equal_screen_coordinates_are_tells() {
        let mut events = session(18.0, min_jerk, 95.0);
        for e in &mut events {
            (e.movement_x, e.movement_y) = (0.0, 0.0);
            (e.screen_x, e.screen_y) = (e.x, e.y);
        }
        let found = checks(&events);
        assert!(
            found.contains(&("movement_zero", Severity::Tell)),
            "{found:?}"
        );
        assert!(
            found.contains(&("screen_coordinates", Severity::Tell)),
            "{found:?}"
        );
    }
}
