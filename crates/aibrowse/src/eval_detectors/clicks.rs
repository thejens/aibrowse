//! Click mechanics and pointer-event fields, as a behavioral detector would check them.

use super::analysis::{Event, Findings, median, quantile, round2};
use super::pointer::PointerStats;

pub fn clicks(events: &[Event], stats: &mut PointerStats, findings: &mut Findings) {
    let mut dwell = Vec::new();
    let mut press = Vec::new();
    let mut teleports = 0;
    let mut bad_buttons = 0;
    for (i, down) in events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == "mousedown")
    {
        let last_move = events[..i].iter().rev().find(|e| e.kind == "mousemove");
        match last_move {
            Some(m) if (m.x - down.x).abs() <= 1.0 && (m.y - down.y).abs() <= 1.0 => {
                dwell.push(down.t - m.t);
            }
            _ => teleports += 1,
        }
        if down.buttons == 0 {
            bad_buttons += 1;
        }
        if let Some(up) = events[i..].iter().find(|e| e.kind == "mouseup") {
            press.push(up.t - down.t);
            if up.buttons != 0 {
                bad_buttons += 1;
            }
        }
    }
    stats.clicks = press.len();
    stats.dwell_ms_median = round2(median(&dwell));
    stats.press_ms_median = round2(median(&press));
    stats.press_ms_min = round2(quantile(&press, 0.0));
    if teleports > 0 {
        findings.tell(
            "click_without_move",
            format!("{teleports} presses happened where no mousemove had brought the cursor"),
        );
    }
    if bad_buttons > 0 {
        findings.tell(
            "button_state",
            format!("{bad_buttons} mousedown/up events have inconsistent `buttons`"),
        );
    }
    if press.is_empty() {
        return;
    }
    // A press under ~10ms is mechanically impossible for a finger on a switch; people
    // hold a button about 60–150ms (commonly reported in mouse-dynamics studies, e.g.
    // Ahmed & Traore, "A new biometric technology based on mouse dynamics", 2007).
    if stats.press_ms_min < 10.0 {
        findings.tell(
            "zero_duration_press",
            format!("a button was held only {:.1}ms", stats.press_ms_min),
        );
    } else if !(40.0..=250.0).contains(&stats.press_ms_median) {
        findings.outlier(
            "press_duration",
            format!(
                "median press {:.0}ms (people: 60–150ms)",
                stats.press_ms_median
            ),
        );
    }
    // People stop, verify they are on target, then press: tens to hundreds of ms.
    if !dwell.is_empty() && stats.dwell_ms_median < 15.0 {
        findings.outlier(
            "no_dwell",
            format!(
                "median {:.0}ms between the last move and the press",
                stats.dwell_ms_median
            ),
        );
    }
}

pub fn pointer_fields(events: &[Event], findings: &mut Findings) {
    let pointers: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind.starts_with("pointer"))
        .collect();
    if let Some(odd) = pointers.iter().find(|e| e.pointer_type != "mouse") {
        findings.tell(
            "pointer_type",
            format!("{} has pointerType {:?}", odd.kind, odd.pointer_type),
        );
    }
    // Per the Pointer Events spec, a mouse reports pressure 0.5 while a button is down
    // and 0 otherwise; anything else is a synthesized pointer.
    let bad_pressure = pointers
        .iter()
        .filter(|e| {
            let expected = if e.buttons != 0 { 0.5 } else { 0.0 };
            (e.pressure - expected).abs() > 1e-6
        })
        .count();
    if bad_pressure > 0 {
        findings.tell(
            "pressure",
            format!("{bad_pressure} pointer events have a pressure a mouse can't report"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::analysis::tests::ev;
    use super::super::analysis::{Event, Severity, WindowContext, analyze};

    fn checks(events: &[Event]) -> Vec<(&'static str, Severity)> {
        analyze(events, &WindowContext::default())
            .findings
            .iter()
            .map(|f| (f.check, f.severity))
            .collect()
    }

    fn at(kind: &str, t: f64, x: f64, buttons: i64) -> Event {
        let mut e = ev(kind, t);
        (e.x, e.y, e.buttons) = (x, 50.0, buttons);
        e
    }

    #[test]
    fn a_click_needs_a_move_to_its_point() {
        let moved = [
            at("mousemove", 0.0, 10.0, 0),
            at("mousedown", 150.0, 10.0, 1),
            at("mouseup", 240.0, 10.0, 0),
        ];
        assert!(checks(&moved).is_empty(), "{:?}", checks(&moved));
        let teleported = [
            at("mousemove", 0.0, 10.0, 0),
            at("mousedown", 150.0, 90.0, 1),
            at("mouseup", 240.0, 90.0, 0),
        ];
        assert!(checks(&teleported).contains(&("click_without_move", Severity::Tell)));
    }

    #[test]
    fn pointer_pressure_must_match_buttons() {
        let mut down = ev("pointerdown", 0.0);
        down.buttons = 1;
        down.pressure = 1.0;
        let found = checks(&[down]);
        assert!(found.contains(&("pressure", Severity::Tell)), "{found:?}");
    }
}
