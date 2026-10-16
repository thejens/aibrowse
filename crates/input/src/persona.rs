use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

/// The pointing device the persona "uses". It shapes scrolling (trackpad momentum vs
/// notched wheel) and pointer speed, and must stay fixed for a profile's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Trackpad,
    Wheel,
}

/// Stable behavioral parameters for one browser profile.
///
/// Consistency matters more than any individual value: an account whose motor behavior
/// changes every session looks less human than one that is consistently a bit odd. A
/// persona is therefore derived deterministically from a per-profile seed (and, from
/// phase A2, fitted to the real user's recorded kinematics).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Persona {
    pub seed: u64,
    pub device: Device,
    /// Fitts's law intercept and slope: movement time = a + b * log2(D / W + 1).
    pub fitts_a_ms: f64,
    pub fitts_b_ms: f64,
    /// Median gap between keystrokes, and the log-normal spread around it.
    pub keystroke_median_ms: f64,
    pub keystroke_sigma: f64,
    /// Median time a key is held down.
    pub key_hold_median_ms: f64,
    /// Median hover before pressing, and median button-down duration.
    pub click_dwell_median_ms: f64,
    pub click_press_median_ms: f64,
    /// Median sideways bow of a pointing path, as a fraction of the distance moved.
    /// Profiles created before path bowing was a parameter get the default.
    #[serde(default = "default_path_bow")]
    pub path_bow: f64,
    /// How far one notch of a mouse wheel scrolls, in CSS px (wheel device only).
    #[serde(default = "default_wheel_notch_px")]
    pub wheel_notch_px: f64,
    /// Median gap between wheel notches while scrolling (wheel device only).
    #[serde(default = "default_wheel_gap_median_ms")]
    pub wheel_gap_median_ms: f64,
    /// Share of aimed movements that land past the target and come back.
    #[serde(default = "default_overshoot_rate")]
    pub overshoot_rate: f64,
}

/// Mid-range of the 2–7% reported for human pointing paths (MacKenzie et al., CHI 2001).
pub(crate) const DEFAULT_PATH_BOW: f64 = 0.045;
/// Chrome on macOS scrolls 100px per notch of a classic mouse wheel.
pub(crate) const DEFAULT_WHEEL_NOTCH_PX: f64 = 100.0;
pub(crate) const DEFAULT_WHEEL_GAP_MEDIAN_MS: f64 = 55.0;
/// Aimed mouse movements often end in a corrective submovement after the primary one
/// lands short or long (Meyer et al. 1988's optimized-submovement model); overshoots
/// are a minority of them. A fifth is a conservative population default; fitting to a
/// user's recording replaces it.
pub(crate) const DEFAULT_OVERSHOOT_RATE: f64 = 0.2;

fn default_overshoot_rate() -> f64 {
    DEFAULT_OVERSHOOT_RATE
}

fn default_path_bow() -> f64 {
    DEFAULT_PATH_BOW
}

fn default_wheel_notch_px() -> f64 {
    DEFAULT_WHEEL_NOTCH_PX
}

fn default_wheel_gap_median_ms() -> f64 {
    DEFAULT_WHEEL_GAP_MEDIAN_MS
}

impl Persona {
    /// Derives a persona from a seed, with every parameter inside ranges reported for
    /// adult desktop users.
    pub fn from_seed(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let device = if rng.random_bool(0.6) {
            Device::Trackpad
        } else {
            Device::Wheel
        };
        // Trackpads are measurably slower to point with than mice.
        let device_slowdown = match device {
            Device::Trackpad => 1.15,
            Device::Wheel => 1.0,
        };
        Self {
            seed,
            device,
            fitts_a_ms: rng.random_range(70.0..150.0) * device_slowdown,
            fitts_b_ms: rng.random_range(90.0..150.0) * device_slowdown,
            keystroke_median_ms: rng.random_range(110.0..190.0),
            keystroke_sigma: rng.random_range(0.25..0.45),
            key_hold_median_ms: rng.random_range(65.0..105.0),
            click_dwell_median_ms: rng.random_range(90.0..200.0),
            click_press_median_ms: rng.random_range(60.0..110.0),
            // Not drawn from the seed: adding draws would change every existing seed's
            // persona. These start at population defaults and are learned by fitting.
            path_bow: DEFAULT_PATH_BOW,
            wheel_notch_px: DEFAULT_WHEEL_NOTCH_PX,
            wheel_gap_median_ms: DEFAULT_WHEEL_GAP_MEDIAN_MS,
            overshoot_rate: DEFAULT_OVERSHOOT_RATE,
        }
    }
}

impl Default for Persona {
    fn default() -> Self {
        Self::from_seed(0x00a1_b0a5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_persona() {
        assert_eq!(Persona::from_seed(42), Persona::from_seed(42));
    }

    #[test]
    fn different_seeds_differ() {
        assert_ne!(Persona::from_seed(1), Persona::from_seed(2));
    }

    #[test]
    fn personas_saved_before_the_newer_fields_still_load() {
        let mut json = serde_json::to_value(Persona::from_seed(9)).unwrap();
        for field in ["path_bow", "wheel_notch_px", "wheel_gap_median_ms"] {
            json.as_object_mut().unwrap().remove(field);
        }
        let loaded: Persona = serde_json::from_value(json).unwrap();
        assert_eq!(loaded, Persona::from_seed(9));
    }
}
