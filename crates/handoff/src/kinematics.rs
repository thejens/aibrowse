//! Opt-in recording of the user's input kinematics during a handoff (docs/design.md
//! §5.9), so the profile's persona can be fitted to the person who owns it.
//!
//! Each event is reduced to a [`Sample`] the moment it arrives: positions and the
//! page's timestamps for the pointer and wheel, and for keys only a coarse class plus a
//! salted hash of the physical key (to pair press and release, held in memory only while
//! the key is down). Text from the type box and pastes is never looked at. When the
//! handoff ends, the samples' statistics (histograms and regression sums, see
//! [`Kinematics`]) are merged into the profile's file; nothing else is written.

use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aibrowse_cdp::Session;
use aibrowse_input::kinematics::{Extractor, KeyClass, KeyId, Kinematics, Sample};
use serde_json::json;
use tokio::sync::watch;

use crate::Status;
use crate::input::ClientEvent;

/// Where a profile's recordings go, set by the agent's server when the profile's policy
/// opts in.
#[derive(Clone)]
pub struct Recording {
    /// The accumulated statistics, e.g. `<profile>/kinematics.json`.
    pub file: PathBuf,
    /// Runs after each handoff's statistics are saved (e.g. to refit the persona).
    pub after_save: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub(crate) type RecordingSlot = Arc<Mutex<Option<Recording>>>;

/// One handoff's recorder.
pub(crate) struct Recorder {
    /// `None` once saved: input after the handoff ends isn't the user's handoff input.
    extractor: Mutex<Option<Extractor>>,
    salt: RandomState,
}

impl Recorder {
    fn new() -> Self {
        Self {
            extractor: Mutex::new(Some(Extractor::new())),
            salt: RandomState::new(),
        }
    }

    /// Starts recording, and saves into `recording` once `status` leaves Pending.
    pub fn start(recording: Recording, mut status: watch::Receiver<Status>) -> Arc<Self> {
        let recorder = Arc::new(Self::new());
        let saving = Arc::clone(&recorder);
        tokio::spawn(async move {
            let _ = status.wait_for(|s| *s != Status::Pending).await;
            let Some(extractor) = saving.lock().take() else {
                return;
            };
            let kinematics = extractor.finish();
            if kinematics.is_empty() {
                return;
            }
            let file = recording.file.clone();
            let saved = tokio::task::spawn_blocking(move || save(&file, kinematics)).await;
            match saved {
                Ok(Ok(())) => {
                    if let Some(after) = &recording.after_save {
                        after();
                    }
                }
                Ok(Err(err)) => tracing::warn!(%err, "could not save input kinematics"),
                Err(err) => tracing::warn!(%err, "saving input kinematics failed"),
            }
        });
        recorder
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Extractor>> {
        self.extractor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Takes note of one user event (already forwarded to the tab).
    pub async fn observe(&self, session: &Session, event: &ClientEvent) {
        let width = match event {
            ClientEvent::Mouse { kind, x, y, .. } if kind == "mousePressed" => {
                target_width(session, *x, *y).await
            }
            _ => None,
        };
        if let Some(sample) = self.reduce(event, width)
            && let Some(extractor) = self.lock().as_mut()
        {
            extractor.feed(sample);
        }
    }

    /// The event as a sample, or `None` for events kinematics don't use. Only mouse
    /// pointers count (touch and pen move differently), and only events carrying the
    /// page's timestamp.
    fn reduce(&self, event: &ClientEvent, target_width: Option<f64>) -> Option<Sample> {
        Some(match event {
            ClientEvent::Mouse {
                kind,
                x,
                y,
                button,
                ts: Some(t),
                pointer,
                ..
            } if pointer.as_deref() == Some("mouse") => match kind.as_str() {
                "mouseMoved" => Sample::Move {
                    t: *t,
                    x: *x,
                    y: *y,
                },
                "mousePressed" if button == "left" => Sample::Press {
                    t: *t,
                    x: *x,
                    y: *y,
                    target_width,
                },
                "mouseReleased" if button == "left" => Sample::Release { t: *t },
                _ => return None,
            },
            ClientEvent::Wheel {
                dx,
                dy,
                ts: Some(t),
                ..
            } => Sample::Wheel {
                t: *t,
                dx: *dx,
                dy: *dy,
            },
            ClientEvent::Key {
                kind,
                key,
                code,
                ts: Some(t),
                ..
            } => {
                let id = KeyId(self.salt.hash_one(code));
                match kind.as_str() {
                    "keyDown" => Sample::KeyDown {
                        t: *t,
                        key: id,
                        class: KeyClass::of(key),
                    },
                    "keyUp" => Sample::KeyUp { t: *t, key: id },
                    _ => return None,
                }
            }
            // Committed text (type box, paste, IME) has no rhythm to learn, and its
            // content must never be looked at.
            _ => return None,
        })
    }
}

/// The smaller side of the element under a viewport point: the target width W of
/// Fitts's law, as the agent's own clicks use it.
async fn target_width(session: &Session, x: f64, y: f64) -> Option<f64> {
    let metrics = session
        .call("Page.getLayoutMetrics", json!({}))
        .await
        .ok()?;
    let viewport = &metrics["cssVisualViewport"];
    let hit = session
        .call(
            "DOM.getNodeForLocation",
            json!({
                "x": (x + viewport["pageX"].as_f64().unwrap_or(0.0)).round() as i64,
                "y": (y + viewport["pageY"].as_f64().unwrap_or(0.0)).round() as i64,
            }),
        )
        .await
        .ok()?;
    let model = session
        .call(
            "DOM.getBoxModel",
            json!({ "backendNodeId": hit["backendNodeId"] }),
        )
        .await
        .ok()?;
    let (width, height) = (
        model["model"]["width"].as_f64()?,
        model["model"]["height"].as_f64()?,
    );
    Some(width.min(height)).filter(|w| *w > 0.0)
}

/// Merges `recorded` into the statistics file, atomically.
pub fn save(file: &Path, mut recorded: Kinematics) -> std::io::Result<()> {
    let mut total = load(file)?;
    recorded.handoffs = 1;
    total.merge(&recorded);
    let json = serde_json::to_vec_pretty(&total).map_err(std::io::Error::other)?;
    let partial = file.with_extension("json.partial");
    std::fs::write(&partial, json)?;
    std::fs::rename(&partial, file)
}

/// The statistics recorded so far (empty when nothing was). A corrupt file is an error
/// rather than silently restarted, so earlier recordings are never overwritten.
pub fn load(file: &Path) -> std::io::Result<Kinematics> {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Kinematics::default()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(kind: &str, key: &str, code: &str, t: f64) -> ClientEvent {
        ClientEvent::Key {
            kind: kind.into(),
            key: key.into(),
            code: code.into(),
            key_code: 0,
            modifiers: 0,
            ts: Some(t),
        }
    }

    /// Every object key in the stored JSON is a field name or a number, and every value
    /// a number: there is nowhere a key identity or typed text could hide.
    fn only_numbers(value: &serde_json::Value, path: &str) {
        const FIELDS: &[&str] = &[
            "handoffs",
            "pointer",
            "keys",
            "wheel",
            "fitts",
            "n",
            "sx",
            "sy",
            "sxx",
            "sxy",
            "bow",
            "peak_at",
            "aimed",
            "overshoots",
            "dwell_ms",
            "press_ms",
            "hold_ms",
            "flight_ms",
            "flight_after_space_ms",
            "trackpad_gestures",
            "notched_gestures",
            "unclassified_gestures",
            "notch_px",
            "notch_gap_ms",
            "trackpad_gap_ms",
            "trackpad_events",
        ];
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    assert!(
                        FIELDS.contains(&k.as_str()) || k.parse::<i64>().is_ok(),
                        "unexpected key {k:?} at {path}"
                    );
                    only_numbers(v, &format!("{path}.{k}"));
                }
            }
            serde_json::Value::Number(_) => {}
            other => panic!("non-numeric value {other} at {path}"),
        }
    }

    #[test]
    fn typing_a_password_stores_rhythm_and_nothing_else() {
        let recorder = Recorder::new();
        let mut t = 1000.0;
        let feed = |event: ClientEvent| {
            if let Some(sample) = recorder.reduce(&event, None) {
                recorder.lock().as_mut().unwrap().feed(sample);
            }
        };
        for _ in 0..10 {
            for ch in "hunter2 xyzzy".chars() {
                let k = ch.to_string();
                let code = format!("Key{}", ch.to_ascii_uppercase());
                feed(key("keyDown", &k, &code, t));
                feed(key("keyUp", &k, &code, t + 85.0));
                t += 170.0;
            }
            feed(ClientEvent::Text {
                text: "s3cret-pasted".into(),
            });
            t += 3000.0;
        }
        let kinematics = recorder.lock().take().unwrap().finish();
        assert_eq!(kinematics.keys.hold_ms.count(), 130);
        assert_eq!(kinematics.keys.flight_ms.count(), 100);
        let json = serde_json::to_value(&kinematics).unwrap();
        only_numbers(&json, "");
        let text = json.to_string();
        for secret in ["hunter", "xyzzy", "s3cret", "KeyH", "\"h\""] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }

    #[test]
    fn touch_input_and_events_without_page_timestamps_are_ignored() {
        let recorder = Recorder::new();
        let mouse = |pointer: &str, ts: Option<f64>| ClientEvent::Mouse {
            kind: "mouseMoved".into(),
            x: 1.0,
            y: 2.0,
            button: "none".into(),
            buttons: 0,
            click_count: 0,
            modifiers: 0,
            ts,
            pointer: Some(pointer.into()),
        };
        assert!(recorder.reduce(&mouse("touch", Some(5.0)), None).is_none());
        assert!(recorder.reduce(&mouse("mouse", None), None).is_none());
        assert!(recorder.reduce(&mouse("mouse", Some(5.0)), None).is_some());
    }

    #[test]
    fn saving_accumulates_across_handoffs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("kinematics.json");
        let mut one = Kinematics::default();
        one.keys.hold_ms.add(90.0);
        save(&file, one.clone()).unwrap();
        save(&file, one).unwrap();
        let total = load(&file).unwrap();
        assert_eq!(total.handoffs, 2);
        assert_eq!(total.keys.hold_ms.count(), 2);
        std::fs::write(&file, "not json").unwrap();
        assert!(save(&file, Kinematics::default()).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not json");
    }
}
