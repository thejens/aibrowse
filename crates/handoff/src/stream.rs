//! Streaming a tab into a live view socket: Chrome's screencast frames (JPEG), sent as
//! fast as the viewer's connection takes them and no faster.
//!
//! - **Backpressure.** A frame is acknowledged to Chrome only once it has gone out on the
//!   socket, and Chrome sends the next only after that: on a slow link the picture
//!   drops frames rather than falling behind.
//! - **Adaptive quality.** How long frames take to send picks a level (JPEG quality and
//!   size): slower steps down, steady quick sends step back up.
//! - **The viewer's size.** The page reports its picture's size in device pixels
//!   (`{"t":"view"}`); frames are no larger than that (a phone gets phone-sized frames).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use aibrowse_cdp::Session;
use axum::extract::ws::{Message, WebSocket};
use base64::Engine;
use futures_util::SinkExt;
use futures_util::stream::SplitSink;
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

/// A live view socket's sending half, shared between the frame pump and the messages
/// that change what the page shows, with what the viewer said about its screen.
pub(crate) struct Link {
    out: Mutex<SplitSink<WebSocket, Message>>,
    screen: watch::Sender<Option<Screen>>,
}

pub(crate) type Sink = Arc<Link>;

/// The viewer's picture size in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Screen {
    pub width: u32,
    pub height: u32,
}

impl Link {
    pub(crate) fn new(out: SplitSink<WebSocket, Message>) -> Sink {
        Arc::new(Self {
            out: Mutex::new(out),
            screen: watch::Sender::new(None),
        })
    }

    /// Frames go no larger than this from now on.
    pub(crate) fn set_screen(&self, width: u32, height: u32) {
        let screen = Screen {
            width: width.clamp(MIN_SIDE, MAX_WIDTH),
            height: height.clamp(MIN_SIDE, MAX_HEIGHT),
        };
        self.screen.send_if_modified(|now| {
            let changed = *now != Some(screen);
            *now = Some(screen);
            changed
        });
    }
}

/// Sends a JSON control message; false if the socket is gone.
pub(crate) async fn send_json(sink: &Sink, value: Value) -> bool {
    sink.out
        .lock()
        .await
        .send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}

async fn send_frame(sink: &Sink, jpeg: Vec<u8>) -> bool {
    sink.out
        .lock()
        .await
        .send(Message::Binary(jpeg.into()))
        .await
        .is_ok()
}

fn decode(data: &Value) -> Option<Vec<u8>> {
    data.as_str()
        .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
}

/// The largest frame, whatever the viewer: as before levels existed.
const MAX_WIDTH: u32 = 1600;
const MAX_HEIGHT: u32 = 1200;
/// The smallest side a viewer's report may ask for.
const MIN_SIDE: u32 = 320;

/// JPEG quality and a share of the largest size, best first.
const LEVELS: [(u32, f64); 4] = [(80, 1.0), (65, 1.0), (50, 0.75), (40, 0.5)];
/// Where a stream starts: good, without the largest frames.
const START_LEVEL: usize = 1;
/// A frame taking longer than this to send (smoothed) steps the level down...
const SLOW: Duration = Duration::from_millis(250);
/// ...and this many frames in a row sent faster than `QUICK` step it back up.
const QUICK: Duration = Duration::from_millis(60);
const QUICK_FRAMES: u32 = 30;
/// Frames sent at a level before it can be judged too slow: one slow frame (a large
/// first one, a hiccup) says little.
const JUDGED_AFTER: u32 = 3;
/// Weight of the newest send time in the smoothed one.
const SMOOTHING: f64 = 0.3;

/// Picks the level from how long frames take to send.
#[derive(Debug)]
struct Pace {
    level: usize,
    smoothed: Option<Duration>,
    /// Frames sent at this level: a level is judged slow only after a few.
    measured: u32,
    quick: u32,
}

impl Pace {
    fn new() -> Self {
        Self {
            level: START_LEVEL,
            smoothed: None,
            measured: 0,
            quick: 0,
        }
    }

    /// Takes one frame's send time; returns the new level when it changes.
    fn sent(&mut self, took: Duration) -> Option<usize> {
        let smoothed = match self.smoothed {
            Some(before) => before.mul_f64(1.0 - SMOOTHING) + took.mul_f64(SMOOTHING),
            None => took,
        };
        self.smoothed = Some(smoothed);
        self.measured += 1;
        self.quick = if took < QUICK { self.quick + 1 } else { 0 };
        if smoothed > SLOW && self.measured >= JUDGED_AFTER && self.level + 1 < LEVELS.len() {
            self.level += 1;
            // Measured afresh at the new level.
            self.smoothed = None;
            self.measured = 0;
            self.quick = 0;
            return Some(self.level);
        }
        if self.quick >= QUICK_FRAMES && self.level > 0 {
            self.level -= 1;
            self.quick = 0;
            return Some(self.level);
        }
        None
    }
}

/// The screencast's parameters for a level and the viewer's screen.
fn screencast(level: usize, screen: Option<Screen>) -> Value {
    let (quality, share) = LEVELS[level];
    let (width, height) = screen.map_or((MAX_WIDTH, MAX_HEIGHT), |s| (s.width, s.height));
    json!({
        "format": "jpeg",
        "quality": quality,
        "maxWidth": (f64::from(width.min(MAX_WIDTH)) * share).round() as u32,
        "maxHeight": (f64::from(height.min(MAX_HEIGHT)) * share).round() as u32,
    })
}

/// How a stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// The socket failed: the viewer left.
    ViewerLeft,
    /// `stop` completed, the tab went away, or the screencast couldn't start.
    Stopped,
    /// A newer live view took the tab's picture (see `claim`).
    Superseded,
}

/// A tab has one screencast, so one live view streams it at a time: the newest. Per
/// tab (session id), the number of the stream that has it. A live view still showing
/// the agent at work after a handoff, its tab closed by the user without it noticing
/// (a still page sends nothing to fail on), held the screencast, and the next handoff's
/// view got no picture ("Screencast is already active").
static OWNERS: LazyLock<std::sync::Mutex<HashMap<String, watch::Sender<u64>>>> =
    LazyLock::new(Default::default);

/// Takes the tab's picture for a new stream: its number, and what tells it when a newer
/// one has taken it.
fn claim(session: &Session) -> (u64, watch::Receiver<u64>) {
    let key = session.id().unwrap_or_default().to_owned();
    let mut owners = OWNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let owner = owners.entry(key).or_insert_with(|| watch::Sender::new(0));
    let mine = *owner.borrow() + 1;
    owner.send_replace(mine);
    (mine, owner.subscribe())
}

/// Streams `session`'s tab until `stop` completes, the viewer leaves, or a newer live
/// view takes the tab's picture.
pub(crate) async fn stream_tab(
    session: &Session,
    sink: &Sink,
    stop: impl Future<Output = ()>,
) -> Ended {
    let (mine, mut owner) = claim(session);
    let mut frames = session.events();
    let mut screen = sink.screen.subscribe();
    let mut pace = Pace::new();
    let viewer = *screen.borrow_and_update();
    let params = screencast(pace.level, viewer);
    // A screencast an older view left running is taken over: that view has stopped
    // streaming or is about to (it sees the newer claim).
    let mut started = session.call("Page.startScreencast", params.clone()).await;
    if started.is_err() {
        let _ = session.call("Page.stopScreencast", json!({})).await;
        started = session.call("Page.startScreencast", params).await;
    }
    if let Err(err) = started {
        tracing::warn!(%err, "could not start the live view screencast");
        return Ended::Stopped;
    }
    // Keep the page rendering while someone watches it.
    let _ = session.call("Page.bringToFront", json!({})).await;
    // Chrome sends screencast frames only when the page repaints; a static page would
    // leave the view blank. Start with a screenshot so there is always a picture.
    if let Ok(shot) = session
        .call(
            "Page.captureScreenshot",
            json!({ "format": "jpeg", "quality": LEVELS[START_LEVEL].0 }),
        )
        .await
        && let Some(jpeg) = decode(&shot["data"])
    {
        let metrics = session
            .call("Page.getLayoutMetrics", json!({}))
            .await
            .unwrap_or_default();
        let viewport = &metrics["cssVisualViewport"];
        if !send_json(
            sink,
            json!({ "t": "size", "w": viewport["clientWidth"], "h": viewport["clientHeight"] }),
        )
        .await
            || !send_frame(sink, jpeg).await
        {
            stop_if_mine(session, &owner, mine).await;
            return Ended::ViewerLeft;
        }
    }

    let restart = |params: Value| async move {
        let _ = session.call("Page.stopScreencast", json!({})).await;
        let _ = session.call("Page.startScreencast", params).await;
    };
    let pump = async {
        let mut last_size = (0.0, 0.0);
        loop {
            let event = tokio::select! {
                // A newer live view has the picture: leave its screencast alone.
                taken = owner.changed() => {
                    let holder = *owner.borrow_and_update();
                    if taken.is_err() || holder != mine {
                        return Ended::Superseded;
                    }
                    continue;
                }
                event = frames.recv() => match event {
                    Some(event) => event,
                    None => return Ended::Stopped, // the tab or browser is gone
                },
                changed = screen.changed() => {
                    if changed.is_ok() {
                        let viewer = *screen.borrow_and_update();
                        restart(screencast(pace.level, viewer)).await;
                    }
                    continue;
                }
            };
            if event.method != "Page.screencastFrame" {
                continue;
            }
            let params = &event.params;
            let meta = &params["metadata"];
            let size = (
                meta["deviceWidth"].as_f64().unwrap_or(0.0),
                meta["deviceHeight"].as_f64().unwrap_or(0.0),
            );
            if size != last_size {
                last_size = size;
                if !send_json(sink, json!({ "t": "size", "w": size.0, "h": size.1 })).await {
                    return Ended::ViewerLeft;
                }
            }
            let sending = Instant::now();
            if let Some(jpeg) = decode(&params["data"])
                && !send_frame(sink, jpeg).await
            {
                return Ended::ViewerLeft;
            }
            // Acknowledged once sent: Chrome sends the next frame only then, so a slow
            // connection skips frames instead of queueing them.
            let _ = session
                .call(
                    "Page.screencastFrameAck",
                    json!({ "sessionId": params["sessionId"] }),
                )
                .await;
            if let Some(level) = pace.sent(sending.elapsed()) {
                tracing::debug!(level, "live view quality changed");
                let viewer = *screen.borrow();
                restart(screencast(level, viewer)).await;
            }
        }
    };
    let ended = tokio::select! {
        ended = pump => ended,
        () = stop => Ended::Stopped,
    };
    if ended != Ended::Superseded {
        stop_if_mine(session, &owner, mine).await;
    }
    ended
}

/// Stops the tab's screencast unless a newer live view has taken it.
async fn stop_if_mine(session: &Session, owner: &watch::Receiver<u64>, mine: u64) {
    let still_mine = *owner.borrow() == mine;
    if still_mine {
        let _ = session.call("Page.stopScreencast", json!({})).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_sends_step_down_and_quick_ones_back_up() {
        let mut pace = Pace::new();
        let slow = Duration::from_millis(400);
        // A level is judged after a few frames, each time afresh.
        let downs: Vec<_> = (0..12).filter_map(|_| pace.sent(slow)).collect();
        assert_eq!(
            downs,
            [START_LEVEL + 1, START_LEVEL + 2],
            "the lowest level stays"
        );
        let quick = Duration::from_millis(20);
        let mut ups = Vec::new();
        for _ in 0..(QUICK_FRAMES * 4) {
            if let Some(level) = pace.sent(quick) {
                ups.push(level);
            }
        }
        assert_eq!(ups, [2, 1, 0]);
    }

    #[test]
    fn frames_fit_the_viewer() {
        let phone = Some(Screen {
            width: 780,
            height: 1688,
        });
        let params = screencast(START_LEVEL, phone);
        assert_eq!(params["maxWidth"], 780);
        assert_eq!(params["maxHeight"], MAX_HEIGHT);
        let lowest = screencast(LEVELS.len() - 1, None);
        assert_eq!(lowest["maxWidth"], MAX_WIDTH / 2);
        assert_eq!(lowest["quality"], 40);
    }
}
