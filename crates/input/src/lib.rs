//! Human-like input planning (the Humanizer, docs/design.md §5.9).
//!
//! This crate is pure: it turns intents ("click this rectangle", "type this text",
//! "scroll by 800px") into timed sequences of low-level input events. It never talks to a
//! browser, so every behavioral property can be unit-tested and, later, compared
//! statistically against recorded human traces. The browser crate executes the plans;
//! `kinematics` turns the real user's recorded input into statistics and fits the
//! persona to them.

mod geometry;
mod keyboard;
pub mod kinematics;
mod motion;
mod persona;
mod scroll;

pub use geometry::{Point, Rect};
pub use keyboard::{KeySpec, Modifiers, Stroke, parse_chord, plan_typing};
pub use motion::{ClickTiming, TimedPoint, pick_click_point, plan_click, plan_move};
pub use persona::{Device, Persona};
pub use scroll::{WheelTick, plan_scroll};

/// How input is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pace {
    /// Physically plausible paths and timing. Default for anything a site might watch.
    #[default]
    Human,
    /// Minimal event sequences with no delays, for tests and sites with no detection.
    /// Even here the cursor moves to the target before clicking; nothing teleports-clicks.
    Fast,
}
