use std::time::Duration;

use rand::Rng;

use crate::Pace;
use crate::motion::jitter_ms;
use crate::persona::{Device, Persona};

/// One `mouseWheel` event, dispatched `after` the previous tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelTick {
    pub after: Duration,
    pub delta_x: f64,
    pub delta_y: f64,
}

/// Plans wheel events that scroll by roughly (`dx`, `dy`) CSS pixels.
///
/// Trackpads emit a burst of small deltas that ramp up and then decay (inertial
/// momentum). Wheels emit evenly sized notches with irregular gaps. Both must match the
/// persona's device; a profile that alternates between the two is suspicious.
pub fn plan_scroll<R: Rng + ?Sized>(
    dx: f64,
    dy: f64,
    persona: &Persona,
    pace: Pace,
    rng: &mut R,
) -> Vec<WheelTick> {
    if dx.abs() < 1.0 && dy.abs() < 1.0 {
        return Vec::new();
    }
    if pace == Pace::Fast {
        return vec![WheelTick {
            after: Duration::ZERO,
            delta_x: dx,
            delta_y: dy,
        }];
    }
    match persona.device {
        Device::Trackpad => trackpad(dx, dy, rng),
        Device::Wheel => wheel(dx, dy, persona, rng),
    }
}

fn trackpad<R: Rng + ?Sized>(dx: f64, dy: f64, rng: &mut R) -> Vec<WheelTick> {
    let distance = dx.hypot(dy);
    let ticks = (distance / 40.0).round().clamp(6.0, 60.0) as usize;
    // Two ramp-up ticks while the fingers accelerate, then exponential momentum decay.
    let decay = ticks as f64 / 3.0;
    let weights: Vec<f64> = (0..ticks)
        .map(|i| match i {
            0 => 0.35,
            1 => 0.75,
            _ => (-((i - 2) as f64) / decay).exp(),
        })
        .collect();
    let total: f64 = weights.iter().sum();

    // Deltas are whole pixels; carry rounding error forward so the total is exact.
    let (mut carry_x, mut carry_y) = (0.0, 0.0);
    weights
        .iter()
        .map(|w| {
            let raw_x = dx * w / total + carry_x;
            let raw_y = dy * w / total + carry_y;
            let (delta_x, delta_y) = (raw_x.round(), raw_y.round());
            carry_x = raw_x - delta_x;
            carry_y = raw_y - delta_y;
            WheelTick {
                after: jitter_ms(16.7, 2.5, rng),
                delta_x,
                delta_y,
            }
        })
        .filter(|tick| tick.delta_x != 0.0 || tick.delta_y != 0.0)
        .collect()
}

/// A notched wheel scrolls the persona's fixed step per notch (100px on macOS Chrome by
/// default; fitted from the user's own wheel when recorded), with irregular gaps around
/// the persona's median spin rhythm.
fn wheel<R: Rng + ?Sized>(dx: f64, dy: f64, persona: &Persona, rng: &mut R) -> Vec<WheelTick> {
    let notch = persona.wheel_notch_px;
    let gap = persona.wheel_gap_median_ms;
    // ±45% of the median, as in the 30–80ms of a steady spin around 55ms.
    let spread = gap * 0.45;
    let notches = |d: f64| {
        (d.abs() / notch)
            .round()
            .max(if d.abs() >= 1.0 { 1.0 } else { 0.0 })
    };
    let (nx, ny) = (notches(dx) as usize, notches(dy) as usize);
    let mut ticks = Vec::with_capacity(nx + ny);
    for _ in 0..ny {
        ticks.push(WheelTick {
            after: jitter_ms(gap, spread, rng),
            delta_x: 0.0,
            delta_y: notch * dy.signum(),
        });
    }
    for _ in 0..nx {
        ticks.push(WheelTick {
            after: jitter_ms(gap, spread, rng),
            delta_x: notch * dx.signum(),
            delta_y: 0.0,
        });
    }
    ticks
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn persona(device: Device) -> Persona {
        Persona {
            device,
            ..Persona::default()
        }
    }

    #[test]
    fn trackpad_scroll_sums_exactly_and_decays() {
        let mut rng = StdRng::seed_from_u64(9);
        let ticks = plan_scroll(
            0.0,
            1234.0,
            &persona(Device::Trackpad),
            Pace::Human,
            &mut rng,
        );
        let sum: f64 = ticks.iter().map(|t| t.delta_y).sum();
        assert_eq!(sum, 1234.0);
        assert!(
            ticks.last().unwrap().delta_y < ticks[2].delta_y,
            "momentum must decay"
        );
    }

    #[test]
    fn wheel_scroll_uses_whole_notches() {
        let mut rng = StdRng::seed_from_u64(9);
        let ticks = plan_scroll(0.0, -420.0, &persona(Device::Wheel), Pace::Human, &mut rng);
        assert_eq!(ticks.len(), 4);
        assert!(ticks.iter().all(|t| t.delta_y == -100.0));
    }
}
