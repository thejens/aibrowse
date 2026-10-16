use std::time::Duration;

use rand::{Rng, RngExt};
use rand_distr::{Distribution, LogNormal, Normal};

use crate::Pace;
use crate::geometry::{Point, Rect};
use crate::persona::Persona;

/// A cursor position to dispatch `at` this offset from the start of the movement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedPoint {
    pub at: Duration,
    pub point: Point,
}

/// Mouse events arrive at roughly display refresh rate; real streams are never perfectly
/// periodic because of OS coalescing, so each interval is jittered.
const FRAME_MS: f64 = 1000.0 / 60.0;
const FRAME_JITTER_MS: f64 = 3.0;
const MIN_MOVE_MS: f64 = 80.0;

/// Plans a pointer movement from `from` to `to`, where `target_width` is the size of the
/// thing being aimed at (smaller targets take longer, per Fitts's law).
///
/// The path follows a minimum-jerk velocity profile, the bell-shaped speed curve of
/// human reaching movements, and bows to one side of the straight line: wrist and
/// elbow rotation make real pointing paths arcs, and a perfectly straight path is a
/// classic automation tell (the detector harness flags it). Some movements overshoot:
/// the primary submovement lands a little past the target, and a short corrective one
/// comes back (`persona.overshoot_rate`). Tremor is still to come.
pub fn plan_move<R: Rng + ?Sized>(
    from: Point,
    to: Point,
    target_width: f64,
    persona: &Persona,
    pace: Pace,
    rng: &mut R,
) -> Vec<TimedPoint> {
    let distance = from.distance(to);
    if pace == Pace::Fast || distance < 1.0 {
        return vec![TimedPoint {
            at: Duration::ZERO,
            point: to,
        }];
    }

    let index_of_difficulty = (distance / target_width.max(1.0) + 1.0).log2();
    let nominal = persona.fitts_a_ms + persona.fitts_b_ms * index_of_difficulty;
    let noise = LogNormal::new(0.0, 0.15).expect("valid log-normal parameters");
    let total_ms = (nominal * noise.sample(rng)).max(MIN_MOVE_MS);

    let bow = plan_bow(from, to, persona.path_bow, rng);
    let overshoot = (distance >= OVERSHOOT_MIN_PX
        && rng.random_bool(persona.overshoot_rate.clamp(0.0, 1.0)))
    .then(|| {
        let extra = (distance * rng.random_range(OVERSHOOT_FRACTION))
            .clamp(*OVERSHOOT_PX.start(), *OVERSHOOT_PX.end());
        (from.lerp(to, 1.0 + extra / distance), extra)
    });
    let Some((beyond, extra)) = overshoot else {
        let mut points = Vec::new();
        segment(&mut points, from, to, &bow, 0.0, total_ms, rng);
        return points;
    };
    // The primary submovement lands past the target, a little early; a short, fast
    // correction comes back onto it.
    let primary_ms = total_ms * PRIMARY_SHARE;
    let correction_ms = (persona.fitts_a_ms * 0.5
        + persona.fitts_b_ms * (extra / target_width.max(1.0) + 1.0).log2())
    .clamp(*CORRECTION_MS.start(), *CORRECTION_MS.end());
    let mut points = Vec::new();
    segment(&mut points, from, beyond, &bow, 0.0, primary_ms, rng);
    let straight = Bow {
        offset: Point::new(0.0, 0.0),
    };
    segment(
        &mut points,
        beyond,
        to,
        &straight,
        primary_ms,
        correction_ms,
        rng,
    );
    points
}

/// Movements shorter than this don't overshoot measurably.
const OVERSHOOT_MIN_PX: f64 = 80.0;
/// How far past the target an overshoot lands: a few percent of the distance, within
/// a few to a couple of dozen px. More than the 2% (and 3px) the recorder counts as one.
const OVERSHOOT_FRACTION: std::ops::Range<f64> = 0.03..0.07;
const OVERSHOOT_PX: std::ops::RangeInclusive<f64> = 4.0..=24.0;
/// The primary submovement's share of the movement time it would have had alone.
const PRIMARY_SHARE: f64 = 0.88;
const CORRECTION_MS: std::ops::RangeInclusive<f64> = 90.0..=250.0;

/// Appends a minimum-jerk movement from `from` to `to` lasting `duration_ms`, starting
/// `start_ms` into the whole movement, sampled at frame rate; it always ends on `to`.
fn segment<R: Rng + ?Sized>(
    points: &mut Vec<TimedPoint>,
    from: Point,
    to: Point,
    bow: &Bow,
    start_ms: f64,
    duration_ms: f64,
    rng: &mut R,
) {
    let jitter = Normal::new(0.0, FRAME_JITTER_MS / 2.0).expect("valid normal parameters");
    let mut t_ms = 0.0;
    loop {
        t_ms += (FRAME_MS + jitter.sample(rng))
            .clamp(FRAME_MS - FRAME_JITTER_MS, FRAME_MS + FRAME_JITTER_MS);
        if t_ms >= duration_ms {
            break;
        }
        let tau = minimum_jerk(t_ms / duration_ms);
        points.push(TimedPoint {
            at: Duration::from_secs_f64((start_ms + t_ms) / 1000.0),
            point: bow.apply(from.lerp(to, tau), tau),
        });
    }
    points.push(TimedPoint {
        at: Duration::from_secs_f64((start_ms + duration_ms) / 1000.0),
        point: to,
    });
}

/// How far, and to which side, a movement's path bows off the straight line.
struct Bow {
    /// Unit normal to the movement direction, scaled by the bow's peak offset in px.
    offset: Point,
}

/// Human pointing paths deviate from the chord by a few percent of the distance
/// (MacKenzie et al., CHI 2001, report movement offsets of several px over a few hundred
/// px); the side varies from movement to movement. Each move's bow is drawn uniformly
/// around the persona's median: ±5/9 of it, which spans 2–7% for the 4.5% default.
const BOW_SPREAD: f64 = 5.0 / 9.0;
/// Long moves don't bow proportionally further; the arm's arc has a limited radius.
const MAX_BOW_PX: f64 = 40.0;

fn plan_bow<R: Rng + ?Sized>(from: Point, to: Point, median: f64, rng: &mut R) -> Bow {
    let distance = from.distance(to);
    let fraction = rng.random_range(median * (1.0 - BOW_SPREAD)..median * (1.0 + BOW_SPREAD));
    let magnitude = (distance * fraction).min(MAX_BOW_PX);
    let side = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
    let (nx, ny) = ((from.y - to.y) / distance, (to.x - from.x) / distance);
    Bow {
        offset: Point::new(nx * magnitude * side, ny * magnitude * side),
    }
}

impl Bow {
    /// Displaces the on-line point at `progress` (0..1) sideways. The displacement is 0
    /// at both ends, so the movement still starts at the cursor and ends on target. The
    /// viewport's top-left edge is a hard limit: moves never leave the page there.
    fn apply(&self, on_line: Point, progress: f64) -> Point {
        let k = (std::f64::consts::PI * progress).sin();
        Point::new(
            (on_line.x + self.offset.x * k).max(0.0),
            (on_line.y + self.offset.y * k).max(0.0),
        )
    }
}

/// Normalized position along a minimum-jerk trajectory: 10τ³ − 15τ⁴ + 6τ⁵.
fn minimum_jerk(tau: f64) -> f64 {
    let t3 = tau * tau * tau;
    t3 * (10.0 - 15.0 * tau + 6.0 * tau * tau)
}

/// Picks where inside `rect` to click. People aim near the middle but rarely hit the
/// exact center; a click on the precise geometric center every time is a classic tell.
pub fn pick_click_point<R: Rng + ?Sized>(rect: &Rect, rng: &mut R) -> Point {
    let center = rect.center();
    // Stay inside the inner 70% so the point is robustly on the element even when its
    // edges are rounded or partly covered by borders.
    let half_w = rect.width * 0.35;
    let half_h = rect.height * 0.35;
    let dx = Normal::new(0.0, (rect.width * 0.12).max(0.01))
        .expect("valid normal parameters")
        .sample(rng)
        .clamp(-half_w, half_w);
    let dy = Normal::new(0.0, (rect.height * 0.12).max(0.01))
        .expect("valid normal parameters")
        .sample(rng)
        .clamp(-half_h, half_h);
    Point::new(center.x + dx, center.y + dy)
}

/// How long to hover before pressing, and how long to hold the button.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickTiming {
    pub dwell: Duration,
    pub press: Duration,
}

pub fn plan_click<R: Rng + ?Sized>(persona: &Persona, pace: Pace, rng: &mut R) -> ClickTiming {
    if pace == Pace::Fast {
        return ClickTiming {
            dwell: Duration::ZERO,
            press: Duration::ZERO,
        };
    }
    ClickTiming {
        dwell: log_normal_ms(persona.click_dwell_median_ms, 0.4, 50.0, 450.0, rng),
        press: log_normal_ms(persona.click_press_median_ms, 0.3, 35.0, 180.0, rng),
    }
}

/// A clamped log-normal duration with the given median. Human reaction and motor times
/// are right-skewed, which a log-normal captures and a uniform or normal does not.
pub(crate) fn log_normal_ms<R: Rng + ?Sized>(
    median_ms: f64,
    sigma: f64,
    min_ms: f64,
    max_ms: f64,
    rng: &mut R,
) -> Duration {
    let dist = LogNormal::new(median_ms.ln(), sigma).expect("valid log-normal parameters");
    Duration::from_secs_f64(dist.sample(rng).clamp(min_ms, max_ms) / 1000.0)
}

/// Uniform jitter helper shared by the planners.
pub(crate) fn jitter_ms<R: Rng + ?Sized>(base_ms: f64, spread_ms: f64, rng: &mut R) -> Duration {
    Duration::from_secs_f64((base_ms + rng.random_range(-spread_ms..=spread_ms)).max(1.0) / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far past `to` the path got, along the movement from `from`.
    fn beyond(points: &[TimedPoint], from: Point, to: Point) -> f64 {
        let d = from.distance(to);
        let (ux, uy) = ((to.x - from.x) / d, (to.y - from.y) / d);
        points
            .iter()
            .map(|p| (p.point.x - from.x) * ux + (p.point.y - from.y) * uy - d)
            .fold(f64::MIN, f64::max)
    }

    #[test]
    fn an_overshoot_lands_past_the_target_and_comes_back() {
        let mut rng = StdRng::seed_from_u64(7);
        let (from, to) = (Point::new(100.0, 100.0), Point::new(500.0, 300.0));
        let always = Persona {
            overshoot_rate: 1.0,
            ..Persona::from_seed(1)
        };
        let path = plan_move(from, to, 24.0, &always, Pace::Human, &mut rng);
        assert_eq!(path.last().unwrap().point, to);
        let past = beyond(&path, from, to);
        assert!((4.0..=25.0).contains(&past), "went {past}px past");
        // Time keeps running forward through the correction.
        assert!(path.windows(2).all(|w| w[1].at > w[0].at));

        let never = Persona {
            overshoot_rate: 0.0,
            ..Persona::from_seed(1)
        };
        for _ in 0..50 {
            let path = plan_move(from, to, 24.0, &never, Pace::Human, &mut rng);
            assert!(beyond(&path, from, to) < 1.0);
        }
        // Short moves don't overshoot at all.
        let near = Point::new(140.0, 100.0);
        let path = plan_move(from, near, 24.0, &always, Pace::Human, &mut rng);
        assert!(beyond(&path, from, near) < 1.0);
    }
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn move_ends_exactly_on_target_with_increasing_time() {
        let mut rng = StdRng::seed_from_u64(7);
        let from = Point::new(10.0, 10.0);
        let to = Point::new(600.0, 400.0);
        let path = plan_move(from, to, 40.0, &Persona::default(), Pace::Human, &mut rng);
        assert!(
            path.len() > 5,
            "a long move must be many events, got {}",
            path.len()
        );
        assert_eq!(path.last().unwrap().point, to);
        assert!(path.windows(2).all(|w| w[0].at < w[1].at));
    }

    #[test]
    fn velocity_is_bell_shaped() {
        let mut rng = StdRng::seed_from_u64(3);
        let path = plan_move(
            Point::new(0.0, 0.0),
            Point::new(800.0, 0.0),
            20.0,
            &Persona::default(),
            Pace::Human,
            &mut rng,
        );
        let speeds: Vec<f64> = path
            .windows(2)
            .map(|w| (w[1].point.x - w[0].point.x) / (w[1].at - w[0].at).as_secs_f64())
            .collect();
        let peak = speeds.iter().copied().fold(0.0, f64::max);
        assert!(speeds[0] < peak * 0.5, "movement must start slow");
        assert!(
            speeds[speeds.len() - 2] < peak * 0.5,
            "movement must end slow"
        );
    }

    #[test]
    fn smaller_targets_take_longer() {
        let persona = Persona::default();
        let mean_duration = |width: f64| {
            let mut rng = StdRng::seed_from_u64(11);
            (0..200)
                .map(|_| {
                    plan_move(
                        Point::new(0.0, 0.0),
                        Point::new(500.0, 0.0),
                        width,
                        &persona,
                        Pace::Human,
                        &mut rng,
                    )
                    .last()
                    .unwrap()
                    .at
                    .as_secs_f64()
                })
                .sum::<f64>()
        };
        assert!(mean_duration(8.0) > mean_duration(120.0));
    }

    #[test]
    fn click_points_stay_inside_and_avoid_exact_center() {
        let mut rng = StdRng::seed_from_u64(5);
        let rect = Rect::new(100.0, 200.0, 80.0, 30.0);
        let points: Vec<Point> = (0..500)
            .map(|_| pick_click_point(&rect, &mut rng))
            .collect();
        assert!(points.iter().all(|p| rect.contains(*p)));
        let exact = points.iter().filter(|p| **p == rect.center()).count();
        assert_eq!(exact, 0);
    }

    #[test]
    fn paths_bow_off_the_straight_line() {
        let mut rng = StdRng::seed_from_u64(13);
        for _ in 0..50 {
            let path = plan_move(
                Point::new(100.0, 300.0),
                Point::new(700.0, 300.0),
                40.0,
                &Persona::default(),
                Pace::Human,
                &mut rng,
            );
            let deviation = path
                .iter()
                .map(|p| (p.point.y - 300.0).abs())
                .fold(0.0, f64::max);
            // 2–7% of 600px, peaking at mid-path; sampled frames may miss the exact peak.
            assert!(
                (5.0..=MAX_BOW_PX).contains(&deviation),
                "deviation {deviation}"
            );
            assert_eq!(path.last().unwrap().point, Point::new(700.0, 300.0));
        }
    }

    #[test]
    fn fast_pace_is_a_single_move() {
        let mut rng = StdRng::seed_from_u64(1);
        let path = plan_move(
            Point::new(0.0, 0.0),
            Point::new(100.0, 100.0),
            10.0,
            &Persona::default(),
            Pace::Fast,
            &mut rng,
        );
        assert_eq!(path.len(), 1);
    }
}
