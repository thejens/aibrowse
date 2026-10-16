//! Fitting a persona to recorded kinematics.
//!
//! Each parameter is estimated from the recording, then shrunk toward the profile's
//! seeded persona (its prior) with weight n / (n + n_min): at the minimum sample count
//! the data gets half the say, and it takes over as samples accumulate. Fitting always
//! starts from the seeded prior, never from the previous fit, so refitting the same
//! recording is idempotent and the result stays a deterministic function of seed and
//! data. Parameters with fewer than their minimum samples keep their current value, and
//! every result is clamped to ranges reported for adult desktop users, so a handful of
//! odd samples can never produce an inhuman persona.

use std::ops::RangeInclusive;

use super::Kinematics;
use crate::persona::{Device, Persona};

/// Minimum samples per parameter, and how far fitted values may go.
const FITTS_MIN: u64 = 20;
/// Below this spread of index of difficulty (in bits²) the slope can't be told apart
/// from the intercept; only the intercept is fitted, under the prior's slope.
const FITTS_MIN_ID_VARIANCE: f64 = 0.25;
/// Fitts intercepts and slopes reported for mice and trackpads (MacKenzie 1992; Soukoreff
/// & MacKenzie 2004): roughly 0–300ms and 70–300ms/bit.
const FITTS_A: RangeInclusive<f64> = 40.0..=300.0;
const FITTS_B: RangeInclusive<f64> = 60.0..=300.0;
const BOW_MIN: u64 = 20;
/// Aimed movements needed before the overshoot rate is learned, and its bounds.
const OVERSHOOT_MIN: u64 = 30;
const OVERSHOOT: RangeInclusive<f64> = 0.0..=0.5;
const BOW: RangeInclusive<f64> = 0.01..=0.12;
const CLICK_MIN: u64 = 20;
/// The click planner's own clamps; a median outside them could not be produced.
const DWELL: RangeInclusive<f64> = 50.0..=450.0;
const PRESS: RangeInclusive<f64> = 40.0..=180.0;
const KEY_MIN: u64 = 50;
/// Dhakal et al. (CHI 2018): press-to-press intervals from ~120ms (the fastest typists)
/// to ~450ms for slow ones; holds of roughly 70–130ms, rarely outside 45–180ms. The
/// persona's keystroke gap is release-to-press (the typing planner holds each key, then
/// waits), so its range is the interval's less a hold. The spread never goes below the
/// seeded personas' lowest: people's intervals vary by a CV of 0.4–0.8, and a persona
/// fitted to an unusually even recording (σ 0.15) typed at a CV of 0.17, which the
/// detector harness flags as too regular.
const KEY_GAP: RangeInclusive<f64> = 40.0..=350.0;
const KEY_GAP_SIGMA: RangeInclusive<f64> = 0.25..=0.8;
const HOLD: RangeInclusive<f64> = 45.0..=180.0;
/// Scroll gestures needed before the device is decided, and the majority required.
const DEVICE_MIN: u64 = 12;
const DEVICE_MAJORITY: f64 = 0.75;
const NOTCH_MIN: u64 = 30;
const NOTCH: RangeInclusive<f64> = 20.0..=240.0;
const NOTCH_GAP: RangeInclusive<f64> = 25.0..=200.0;

/// One parameter's fit.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamFit {
    pub name: &'static str,
    pub before: f64,
    pub after: f64,
    /// What the recording alone says, when there is enough of it.
    pub observed: Option<f64>,
    pub samples: u64,
    pub needed: u64,
}

/// The fitted persona and how each parameter got there.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub persona: Persona,
    pub params: Vec<ParamFit>,
    /// The pointing device before and after, and the gestures behind the decision.
    pub device: (Device, Device),
    pub gestures: (u64, u64),
}

impl Fit {
    /// Whether fitting changed anything.
    pub fn changed(&self) -> bool {
        self.device.0 != self.device.1 || self.params.iter().any(ParamFit::changed)
    }
}

impl ParamFit {
    /// Whether the value moved by more than storage rounding: persona.json goes through
    /// JSON, which may change a float's last bit.
    pub fn changed(&self) -> bool {
        (self.after - self.before).abs() > 1e-6 * self.before.abs().max(1.0)
    }
}

/// Fits `current` (a profile's persona) to `kinematics`.
pub fn fit(current: &Persona, kinematics: &Kinematics) -> Fit {
    let prior = Persona::from_seed(current.seed);
    let mut persona = current.clone();
    let mut params = Vec::new();
    let mut set = |name, field: &mut f64, prior: f64, observed: Option<f64>, n, needed, bounds| {
        let fitted = fit_one(*field, prior, observed, n, needed, bounds);
        params.push(fitted.clone_named(name));
        *field = fitted.after;
    };

    let (a, b) = fitts(&prior, kinematics);
    let n = kinematics.pointer.fitts.n;
    set(
        "fitts_a_ms",
        &mut persona.fitts_a_ms,
        prior.fitts_a_ms,
        a,
        n,
        FITTS_MIN,
        FITTS_A,
    );
    set(
        "fitts_b_ms",
        &mut persona.fitts_b_ms,
        prior.fitts_b_ms,
        b,
        n,
        FITTS_MIN,
        FITTS_B,
    );

    let p = &kinematics.pointer;
    set(
        "path_bow",
        &mut persona.path_bow,
        prior.path_bow,
        p.bow.median(),
        p.bow.count(),
        BOW_MIN,
        BOW,
    );
    // The share of aimed movements that went past their target before the click.
    let rate = (p.aimed > 0).then(|| p.overshoots as f64 / p.aimed as f64);
    set(
        "overshoot_rate",
        &mut persona.overshoot_rate,
        prior.overshoot_rate,
        rate,
        p.aimed,
        OVERSHOOT_MIN,
        OVERSHOOT,
    );
    set(
        "click_dwell_median_ms",
        &mut persona.click_dwell_median_ms,
        prior.click_dwell_median_ms,
        p.dwell_ms.median(),
        p.dwell_ms.count(),
        CLICK_MIN,
        DWELL,
    );
    set(
        "click_press_median_ms",
        &mut persona.click_press_median_ms,
        prior.click_press_median_ms,
        p.press_ms.median(),
        p.press_ms.count(),
        CLICK_MIN,
        PRESS,
    );

    let k = &kinematics.keys;
    let (gap, gap_sigma) = key_gap(kinematics);
    let n = k.flight_ms.count().min(k.hold_ms.count());
    set(
        "keystroke_median_ms",
        &mut persona.keystroke_median_ms,
        prior.keystroke_median_ms,
        gap,
        n,
        KEY_MIN,
        KEY_GAP,
    );
    set(
        "keystroke_sigma",
        &mut persona.keystroke_sigma,
        prior.keystroke_sigma,
        gap_sigma,
        n,
        KEY_MIN,
        KEY_GAP_SIGMA,
    );
    set(
        "key_hold_median_ms",
        &mut persona.key_hold_median_ms,
        prior.key_hold_median_ms,
        k.hold_ms.median(),
        k.hold_ms.count(),
        KEY_MIN,
        HOLD,
    );

    let w = &kinematics.wheel;
    set(
        "wheel_gap_median_ms",
        &mut persona.wheel_gap_median_ms,
        prior.wheel_gap_median_ms,
        w.notch_gap_ms.median(),
        w.notch_gap_ms.count(),
        NOTCH_MIN,
        NOTCH_GAP,
    );
    // A notch is a fixed step of the user's hardware: blending 100px with a measured
    // 120px would describe no real wheel, so the observed step is taken as is.
    let notched: u64 = w.notch_px.values().sum();
    let notch = kinematics
        .notch_mode()
        .filter(|(_, share)| *share >= 0.5)
        .map(|(px, _)| px);
    let before = persona.wheel_notch_px;
    if let Some(px) = notch.filter(|_| notched >= NOTCH_MIN) {
        persona.wheel_notch_px = px.clamp(*NOTCH.start(), *NOTCH.end());
    }
    params.push(ParamFit {
        name: "wheel_notch_px",
        before,
        after: persona.wheel_notch_px,
        observed: notch,
        samples: notched,
        needed: NOTCH_MIN,
    });

    // The device is what the user's hand is on; the persona should scroll like it.
    let gestures = (w.trackpad_gestures, w.notched_gestures);
    let total = gestures.0 + gestures.1;
    if total >= DEVICE_MIN {
        if gestures.0 as f64 >= total as f64 * DEVICE_MAJORITY {
            persona.device = Device::Trackpad;
        } else if gestures.1 as f64 >= total as f64 * DEVICE_MAJORITY {
            persona.device = Device::Wheel;
        }
    }
    Fit {
        device: (current.device, persona.device),
        persona,
        params,
        gestures,
    }
}

/// The observed release-to-press gap and its log-normal σ. Recordings hold press-to-press
/// flights (well defined even when fast typists overlap keys) and holds; the planner's
/// gap is a flight less a hold. The flight's spread is carried over in absolute terms
/// (σ·median, the delta method), since the hold varies far less than the gap.
fn key_gap(kinematics: &Kinematics) -> (Option<f64>, Option<f64>) {
    let k = &kinematics.keys;
    let (Some(flight), Some(hold), Some(sigma)) = (
        k.flight_ms.median(),
        k.hold_ms.median(),
        k.flight_ms.log_sigma(),
    ) else {
        return (None, None);
    };
    let gap = (flight - hold).max(*KEY_GAP.start());
    (Some(gap), Some(sigma * flight / gap))
}

/// The observed Fitts intercept and slope, when there are enough aimed strokes.
fn fitts(prior: &Persona, kinematics: &Kinematics) -> (Option<f64>, Option<f64>) {
    let r = &kinematics.pointer.fitts;
    if r.n < FITTS_MIN {
        return (None, None);
    }
    // With varied difficulty, least squares; otherwise (or when the slope comes out
    // implausible) keep a slope and pass the line through the data's centroid, which is
    // what the data does pin down.
    let slope = r
        .line()
        .filter(|_| r.var_x() >= FITTS_MIN_ID_VARIANCE)
        .map(|(_, b)| b.clamp(*FITTS_B.start(), *FITTS_B.end()));
    let b = slope.unwrap_or(prior.fitts_b_ms);
    (Some(r.mean_y() - b * r.mean_x()), slope)
}

fn fit_one(
    current: f64,
    prior: f64,
    observed: Option<f64>,
    samples: u64,
    needed: u64,
    bounds: RangeInclusive<f64>,
) -> ParamFit {
    let after = match observed {
        Some(value) if samples >= needed => {
            let weight = samples as f64 / (samples + needed) as f64;
            // Four decimals is far below anything a person could notice, and keeps the
            // stored persona short and stable.
            let fitted = (prior + weight * (value - prior)).clamp(*bounds.start(), *bounds.end());
            (fitted * 1e4).round() / 1e4
        }
        _ => current,
    };
    ParamFit {
        name: "",
        before: current,
        after,
        observed: observed.filter(|_| samples >= needed),
        samples,
        needed,
    }
}

impl ParamFit {
    fn clone_named(&self, name: &'static str) -> Self {
        Self {
            name,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinematics::{Extractor, KeyClass, KeyId, Sample};

    fn param<'a>(fit: &'a Fit, name: &str) -> &'a ParamFit {
        fit.params.iter().find(|p| p.name == name).unwrap()
    }

    /// `n` keystrokes of a typist with constant `flight` and `hold`.
    fn typed(n: usize, flight: f64, hold: f64) -> Kinematics {
        let mut x = Extractor::new();
        for i in 0..n {
            let t = i as f64 * flight;
            let key = KeyId(i as u64);
            x.feed(Sample::KeyDown {
                t,
                key,
                class: KeyClass::Character,
            });
            x.feed(Sample::KeyUp { t: t + hold, key });
        }
        x.finish()
    }

    #[test]
    fn too_few_samples_change_nothing() {
        let persona = Persona::from_seed(3);
        let fit = fit(&persona, &typed(KEY_MIN as usize - 5, 300.0, 150.0));
        assert_eq!(fit.persona, persona);
        assert!(!fit.changed());
        assert!(param(&fit, "key_hold_median_ms").observed.is_none());
    }

    #[test]
    fn fitted_values_move_toward_the_data_with_more_samples() {
        let persona = Persona::from_seed(3);
        let few = fit(&persona, &typed(60, 300.0, 150.0));
        let many = fit(&persona, &typed(2000, 300.0, 150.0));
        // 300ms press to press, 150ms of it held: a 150ms gap, which more data
        // approaches more closely (to within the histograms' 2% bins).
        let off = |value: f64| (value - 150.0).abs();
        let at = |fit: &Fit| fit.persona.keystroke_median_ms;
        assert!(
            off(at(&few)) < off(persona.keystroke_median_ms),
            "{}",
            at(&few)
        );
        assert!(
            off(at(&many)) <= off(at(&few)) && off(at(&many)) < 8.0,
            "{}",
            at(&many)
        );
        assert!((many.persona.key_hold_median_ms - 150.0).abs() < 5.0);
    }

    #[test]
    fn inhuman_data_is_clamped() {
        let persona = Persona::from_seed(3);
        let fit = fit(&persona, &typed(5000, 20.0, 5.0));
        // The observed gap is floored at the bound; the prior's small remaining weight
        // keeps the fit just above it.
        assert!(KEY_GAP.contains(&fit.persona.keystroke_median_ms));
        assert!(fit.persona.keystroke_median_ms < 45.0);
        assert_eq!(fit.persona.key_hold_median_ms, *HOLD.start());
        assert!(KEY_GAP_SIGMA.contains(&fit.persona.keystroke_sigma));
    }

    #[test]
    fn refitting_is_idempotent_and_deterministic() {
        let persona = Persona::from_seed(8);
        let data = typed(400, 220.0, 95.0);
        let once = fit(&persona, &data);
        let twice = fit(&once.persona, &data);
        assert_eq!(once.persona, twice.persona);
        assert_eq!(once.persona.seed, persona.seed);
    }

    #[test]
    fn fitts_parameters_are_recovered_from_strokes() {
        use crate::Point;
        use crate::kinematics::extract::tests::click_stroke;
        use rand::SeedableRng;
        let user = Persona {
            fitts_a_ms: 200.0,
            fitts_b_ms: 180.0,
            path_bow: 0.08,
            ..Persona::from_seed(21)
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(2);
        let mut x = Extractor::new();
        let (mut t, mut at) = (0.0, Point::new(20.0, 20.0));
        let targets = [
            (400.0, 300.0, 12.0),
            (120.0, 140.0, 90.0),
            (480.0, 80.0, 30.0),
        ];
        for i in 0..400 {
            let (tx, ty, w) = targets[i % 3];
            t = click_stroke(
                &mut x,
                &user,
                &mut rng,
                t + 400.0,
                at,
                Point::new(tx, ty),
                w,
            );
            at = Point::new(tx, ty);
        }
        let fitted = fit(&Persona::from_seed(5), &x.finish()).persona;
        assert!(
            (fitted.fitts_a_ms - 200.0).abs() < 50.0,
            "a {}",
            fitted.fitts_a_ms
        );
        assert!(
            (fitted.fitts_b_ms - 180.0).abs() < 30.0,
            "b {}",
            fitted.fitts_b_ms
        );
        assert!(
            (fitted.path_bow - 0.08).abs() < 0.015,
            "bow {}",
            fitted.path_bow
        );
        assert!((fitted.click_dwell_median_ms - user.click_dwell_median_ms).abs() < 10.0);
    }

    #[test]
    fn the_device_and_notch_follow_the_users_wheel() {
        let mut x = Extractor::new();
        let mut t = 0.0;
        for _ in 0..15 {
            t += 1000.0;
            for gap in [0.0, 60.0, 45.0, 70.0] {
                t += gap;
                x.feed(Sample::Wheel {
                    t,
                    dx: 0.0,
                    dy: 120.0,
                });
            }
        }
        let trackpad = Persona {
            device: Device::Trackpad,
            ..Persona::from_seed(1)
        };
        let fit = fit(&trackpad, &x.finish());
        assert_eq!(fit.device, (Device::Trackpad, Device::Wheel));
        assert_eq!(fit.persona.wheel_notch_px, 120.0);
        assert!((fit.persona.wheel_gap_median_ms - 58.0).abs() < 6.0);
    }
}
