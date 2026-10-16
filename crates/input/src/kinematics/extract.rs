//! Turns a live [`Sample`] stream into [`Kinematics`], incrementally: only the stroke,
//! wheel gesture and keys in progress are held, never the stream.

use std::collections::HashMap;

use super::{KeyClass, KeyId, Kinematics, Sample};

/// A pause this long ends a stroke: the next movement is a new aimed movement. Matches
/// the detector harness's stroke split, so both measure the same thing.
const STROKE_GAP_MS: f64 = 120.0;
/// Path shape is only meaningful for strokes this long with this many samples; short
/// corrective nudges are legitimately straight.
const MIN_SHAPE_PX: f64 = 100.0;
const MIN_SHAPE_SAMPLES: usize = 6;
/// A click counts as the end of an aimed stroke only if the stroke covered at least this
/// distance and the cursor arrived at most this long before the press.
const MIN_AIMED_PX: f64 = 30.0;
const MAX_AIM_DWELL_MS: f64 = 1500.0;
/// Movement times outside this range aren't single aimed movements (a hesitation the
/// stroke split didn't catch, or a flick); a path this much longer than its chord
/// wandered rather than aimed.
const AIMED_MT_MS: std::ops::RangeInclusive<f64> = 80.0..=3000.0;
const MAX_PATH_OVER_CHORD: f64 = 2.5;
/// The first sample of a movement arrives about a frame after the hand started moving.
const FRAME_MS: f64 = 1000.0 / 60.0;
/// Presses held longer than this are drags or long-presses, not clicks.
const MAX_PRESS_MS: f64 = 1000.0;
/// Keys held longer than this are deliberate (auto-repeat, holding a key), not typing.
const MAX_HOLD_MS: f64 = 1000.0;
/// Gaps longer than this are pauses between bursts of typing, not keystroke flights.
const MAX_FLIGHT_MS: f64 = 1500.0;
/// Wheel events further apart than this are separate gestures.
const GESTURE_GAP_MS: f64 = 300.0;
/// Trackpads (and their momentum phase) report once per display frame; notched wheels
/// report once per notch, tens of milliseconds apart even when spun fast.
const FRAME_CADENCE_MS: f64 = 25.0;

#[derive(Debug, Clone, Copy)]
struct Point3 {
    t: f64,
    x: f64,
    y: f64,
}

/// Accumulates statistics from samples fed in time order.
#[derive(Debug, Default)]
pub struct Extractor {
    out: Kinematics,
    /// Samples of the stroke in progress.
    stroke: Vec<Point3>,
    /// Where the cursor rested before the current stroke.
    rest: Option<Point3>,
    /// Time of the button press awaiting its release.
    pressed_at: Option<f64>,
    held: HashMap<KeyId, f64>,
    last_typed: Option<(f64, KeyClass)>,
    gesture: Vec<(f64, f64)>,
}

impl Extractor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, sample: Sample) {
        match sample {
            Sample::Move { t, x, y } => self.on_move(Point3 { t, x, y }),
            Sample::Press {
                t,
                x,
                y,
                target_width,
            } => self.on_press(Point3 { t, x, y }, target_width),
            Sample::Release { t } => {
                if let Some(down) = self.pressed_at.take()
                    && (0.0..=MAX_PRESS_MS).contains(&(t - down))
                {
                    self.out.pointer.press_ms.add(t - down);
                }
            }
            Sample::Wheel { t, dx, dy } => {
                if self
                    .gesture
                    .last()
                    .is_some_and(|(last, _)| t - last > GESTURE_GAP_MS)
                {
                    self.end_gesture();
                }
                self.gesture.push((t, dx.abs().max(dy.abs())));
            }
            Sample::KeyDown { t, key, class } => self.on_key_down(t, key, class),
            Sample::KeyUp { t, key } => {
                if let Some(down) = self.held.remove(&key)
                    && (0.0..=MAX_HOLD_MS).contains(&(t - down))
                {
                    self.out.keys.hold_ms.add(t - down);
                }
            }
        }
    }

    /// Ends whatever is in progress and returns the statistics.
    pub fn finish(mut self) -> Kinematics {
        self.end_stroke();
        self.end_gesture();
        self.out
    }

    fn on_move(&mut self, point: Point3) {
        if self
            .stroke
            .last()
            .is_some_and(|last| point.t - last.t > STROKE_GAP_MS)
        {
            self.end_stroke();
        }
        self.stroke.push(point);
    }

    fn on_press(&mut self, at: Point3, target_width: Option<f64>) {
        self.pressed_at = Some(at.t);
        let stroke = std::mem::take(&mut self.stroke);
        let origin = self.rest.replace(at);
        let Some(arrival) = stroke.last().copied() else {
            return;
        };
        let dwell = at.t - arrival.t;
        if !(0.0..=MAX_AIM_DWELL_MS).contains(&dwell) {
            self.shape(origin, &stroke);
            return;
        }
        self.out.pointer.dwell_ms.add(dwell);
        self.shape(origin, &stroke);
        // Without a resting point (the cursor just entered the view) the movement's
        // true start, and so its distance, is unknown.
        let Some(origin) = origin else {
            return;
        };
        let points = with_origin(Some(origin), &stroke);
        let start = points[0];
        let distance = (at.x - start.x).hypot(at.y - start.y);
        if distance < MIN_AIMED_PX {
            return;
        }
        let path = path_length(&points);
        let movement_ms = arrival.t - start.t;
        if path / distance > MAX_PATH_OVER_CHORD || !AIMED_MT_MS.contains(&movement_ms) {
            return;
        }
        self.out.pointer.aimed += 1;
        if overshot(&points, at, distance) {
            self.out.pointer.overshoots += 1;
        }
        if let Some(width) = target_width.filter(|w| *w >= 1.0) {
            let id = (distance / width.max(4.0) + 1.0).log2();
            self.out.pointer.fitts.add(id, movement_ms);
        }
    }

    /// A stroke ended without a click: only its shape counts.
    fn end_stroke(&mut self) {
        let stroke = std::mem::take(&mut self.stroke);
        let Some(last) = stroke.last().copied() else {
            return;
        };
        let origin = self.rest.replace(last);
        self.shape(origin, &stroke);
    }

    /// Path shape: how far the stroke bows off its chord, and where its speed peaks.
    fn shape(&mut self, origin: Option<Point3>, stroke: &[Point3]) {
        let points = with_origin(origin, stroke);
        if points.len() < MIN_SHAPE_SAMPLES || path_length(&points) < MIN_SHAPE_PX {
            return;
        }
        let (start, end) = (points[0], points[points.len() - 1]);
        let chord = (end.x - start.x).hypot(end.y - start.y);
        if chord < MIN_SHAPE_PX {
            return;
        }
        let deviation = points
            .iter()
            .map(|p| {
                ((end.x - start.x) * (start.y - p.y) - (start.x - p.x) * (end.y - start.y)).abs()
                    / chord
            })
            .fold(0.0, f64::max);
        self.out.pointer.bow.add(deviation / chord);
        let duration = end.t - start.t;
        let peak = points
            .windows(2)
            .filter(|w| w[1].t > w[0].t)
            .map(|w| {
                (
                    w[1].t,
                    (w[1].x - w[0].x).hypot(w[1].y - w[0].y) / (w[1].t - w[0].t),
                )
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((t, _)) = peak
            && duration > 0.0
        {
            self.out.pointer.peak_at.add((t - start.t) / duration);
        }
    }

    fn on_key_down(&mut self, t: f64, key: KeyId, class: KeyClass) {
        // Auto-repeat sends further keydowns for a held key: not new keystrokes.
        if self.held.contains_key(&key) {
            return;
        }
        match class {
            KeyClass::Character | KeyClass::Space => {
                self.held.insert(key, t);
                if let Some((previous, previous_class)) = self.last_typed {
                    let flight = t - previous;
                    if (0.0..=MAX_FLIGHT_MS).contains(&flight) {
                        if previous_class == KeyClass::Space {
                            self.out.keys.flight_after_space_ms.add(flight);
                        } else if class == KeyClass::Character {
                            self.out.keys.flight_ms.add(flight);
                        }
                    }
                }
                self.last_typed = Some((t, class));
            }
            // Shift for a capital belongs to the rhythm; it neither counts nor breaks it.
            KeyClass::Modifier => {}
            KeyClass::Edit | KeyClass::Other => self.last_typed = None,
        }
    }

    fn end_gesture(&mut self) {
        let gesture = std::mem::take(&mut self.gesture);
        if gesture.is_empty() {
            return;
        }
        let gaps: Vec<f64> = gesture.windows(2).map(|w| w[1].0 - w[0].0).collect();
        let median_gap = median(&gaps);
        let mut magnitudes: Vec<u32> = gesture.iter().map(|(_, m)| m.round() as u32).collect();
        let w = &mut self.out.wheel;
        // A trackpad streams many varied deltas at frame rate (momentum decaying to a
        // pixel or two); a notched wheel sends runs of one step, notches apart.
        let mut distinct = magnitudes.clone();
        distinct.sort_unstable();
        distinct.dedup();
        let mode_share = {
            let mode = distinct
                .iter()
                .map(|d| magnitudes.iter().filter(|m| *m == d).count())
                .max()
                .unwrap_or(0);
            mode as f64 / magnitudes.len() as f64
        };
        if gesture.len() >= 5
            && median_gap.is_some_and(|g| g < FRAME_CADENCE_MS)
            && distinct.len() >= 4
        {
            w.trackpad_gestures += 1;
            w.trackpad_events.add(gesture.len() as f64);
            gaps.iter().for_each(|g| w.trackpad_gap_ms.add(*g));
        } else if mode_share >= 0.5 && distinct.last().is_some_and(|m| *m >= 1) {
            w.notched_gestures += 1;
            magnitudes.retain(|m| *m > 0);
            for m in magnitudes {
                *w.notch_px.entry(m).or_default() += 1;
            }
            gaps.iter().for_each(|g| w.notch_gap_ms.add(*g));
        } else {
            w.unclassified_gestures += 1;
        }
    }
}

/// The stroke's samples, preceded by where it started from: the resting point, placed a
/// frame before the first sample (which is when the hand started moving).
fn with_origin(origin: Option<Point3>, stroke: &[Point3]) -> Vec<Point3> {
    let mut points = Vec::with_capacity(stroke.len() + 1);
    if let (Some(rest), Some(first)) = (origin, stroke.first()) {
        points.push(Point3 {
            t: first.t - FRAME_MS,
            ..rest
        });
    }
    points.extend_from_slice(stroke);
    points
}

fn path_length(points: &[Point3]) -> f64 {
    points
        .windows(2)
        .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
        .sum()
}

/// Whether the path went past the click point along the movement direction before
/// settling on it (by more than 2% of the distance and at least 3px).
fn overshot(points: &[Point3], target: Point3, distance: f64) -> bool {
    let start = points[0];
    let (ux, uy) = (
        (target.x - start.x) / distance,
        (target.y - start.y) / distance,
    );
    let furthest = points
        .iter()
        .map(|p| (p.x - start.x) * ux + (p.y - start.y) * uy)
        .fold(f64::MIN, f64::max);
    furthest - distance > (distance * 0.02).max(3.0)
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[sorted.len() / 2])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{Pace, Persona, Point, plan_move};
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    /// Feeds a planned movement from `from` to `to` starting at `t0`, then a click on a
    /// `width`-wide target. Returns the time after the release.
    pub fn click_stroke(
        x: &mut Extractor,
        persona: &Persona,
        rng: &mut StdRng,
        t0: f64,
        from: Point,
        to: Point,
        width: f64,
    ) -> f64 {
        let path = plan_move(from, to, width, persona, Pace::Human, rng);
        let mut t = t0;
        for step in &path {
            t = t0 + step.at.as_secs_f64() * 1000.0;
            x.feed(Sample::Move {
                t,
                x: step.point.x,
                y: step.point.y,
            });
        }
        let press = t + persona.click_dwell_median_ms;
        x.feed(Sample::Press {
            t: press,
            x: to.x,
            y: to.y,
            target_width: Some(width),
        });
        x.feed(Sample::Release {
            t: press + persona.click_press_median_ms,
        });
        press + persona.click_press_median_ms
    }

    #[test]
    fn planned_strokes_are_measured_as_planned() {
        let persona = Persona::from_seed(4);
        let mut rng = StdRng::seed_from_u64(1);
        let mut x = Extractor::new();
        let mut t = 0.0;
        // Distances of 250–500px stay under the planner's 40px bow cap.
        let targets = [
            (400.0, 300.0, 20.0),
            (100.0, 120.0, 80.0),
            (450.0, 60.0, 10.0),
        ];
        let mut at = Point::new(10.0, 10.0);
        for i in 0..60 {
            let (tx, ty, w) = targets[i % targets.len()];
            let to = Point::new(tx, ty);
            t = click_stroke(&mut x, &persona, &mut rng, t + 500.0, at, to, w);
            at = to;
        }
        let k = x.finish();
        assert_eq!(k.pointer.aimed, 59, "the first press has no known origin");
        assert_eq!(k.pointer.fitts.n, 59);
        let (a, b) = k.pointer.fitts.line().unwrap();
        // The planner's own log-normal noise (σ 0.15) blurs the line a little.
        assert!(
            (a - persona.fitts_a_ms).abs() < 60.0,
            "a {a} vs {}",
            persona.fitts_a_ms
        );
        assert!(
            (b - persona.fitts_b_ms).abs() < 30.0,
            "b {b} vs {}",
            persona.fitts_b_ms
        );
        let bow = k.pointer.bow.median().unwrap();
        assert!((bow - persona.path_bow).abs() < 0.012, "bow {bow}");
        let dwell = k.pointer.dwell_ms.median().unwrap();
        assert!((dwell - persona.click_dwell_median_ms).abs() < 3.0);
        let peak = k.pointer.peak_at.median().unwrap();
        assert!(
            (0.35..0.65).contains(&peak),
            "minimum jerk peaks mid-way: {peak}"
        );
        // The recorder sees the planner's overshoots at about the persona's rate.
        let rate = k.pointer.overshoots as f64 / k.pointer.aimed.max(1) as f64;
        assert!(
            (rate - persona.overshoot_rate).abs() < 0.12,
            "overshoot rate {rate} (n={})",
            k.pointer.aimed
        );
    }

    #[test]
    fn typing_yields_holds_and_flights_and_skips_edits_and_repeats() {
        let mut x = Extractor::new();
        let mut t = 0.0;
        // "ab cd" typed with 150ms flights and 90ms holds, then a Backspace, then "e"
        // 2s later (a pause, not a flight); "a" auto-repeats once.
        let keys = [
            (1, KeyClass::Character),
            (2, KeyClass::Character),
            (3, KeyClass::Space),
            (4, KeyClass::Character),
            (5, KeyClass::Character),
        ];
        for (id, class) in keys {
            x.feed(Sample::KeyDown {
                t,
                key: KeyId(id),
                class,
            });
            if id == 1 {
                x.feed(Sample::KeyDown {
                    t: t + 40.0,
                    key: KeyId(id),
                    class,
                });
            }
            x.feed(Sample::KeyUp {
                t: t + 90.0,
                key: KeyId(id),
            });
            t += 150.0;
        }
        x.feed(Sample::KeyDown {
            t,
            key: KeyId(6),
            class: KeyClass::Edit,
        });
        x.feed(Sample::KeyUp {
            t: t + 80.0,
            key: KeyId(6),
        });
        x.feed(Sample::KeyDown {
            t: t + 2000.0,
            key: KeyId(7),
            class: KeyClass::Character,
        });
        let k = x.finish();
        assert_eq!(k.keys.hold_ms.count(), 5, "edit keys aren't typing holds");
        assert!((k.keys.hold_ms.median().unwrap() - 90.0).abs() < 1.0);
        // a→b and c→d within words; space→c after a space; b→space is neither.
        assert_eq!(k.keys.flight_ms.count(), 2);
        assert_eq!(k.keys.flight_after_space_ms.count(), 1);
    }

    #[test]
    fn wheel_gestures_are_told_apart() {
        let mut x = Extractor::new();
        let mut t = 0.0;
        // A trackpad flick: frame-rate events, momentum decaying.
        for (i, d) in [
            12.0, 31.0, 44.0, 37.0, 28.0, 21.0, 15.0, 10.0, 7.0, 4.0, 2.0, 1.0,
        ]
        .iter()
        .enumerate()
        {
            t += 16.7 + if i % 2 == 0 { 1.5 } else { -1.2 };
            x.feed(Sample::Wheel { t, dx: 0.0, dy: *d });
        }
        // Two wheel spins, a second apart: 100px notches, tens of ms apart.
        for spin in 0..2 {
            t += 1000.0;
            for gap in [0.0, 48.0, 81.0, 39.0] {
                t += gap;
                x.feed(Sample::Wheel {
                    t,
                    dx: 0.0,
                    dy: if spin == 0 { 100.0 } else { -100.0 },
                });
            }
        }
        let k = x.finish();
        assert_eq!(k.wheel.trackpad_gestures, 1);
        assert_eq!(k.wheel.notched_gestures, 2);
        assert_eq!(k.notch_mode(), Some((100.0, 1.0)));
        assert_eq!(k.wheel.notch_gap_ms.count(), 6);
        assert!((k.wheel.trackpad_gap_ms.median().unwrap() - 16.7).abs() < 2.0);
    }
}
