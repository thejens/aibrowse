//! Executing humanized input plans (from `aibrowse-input`) against a tab: the actions
//! the agent calls, and the pointer primitives (cursor motion, wheel, buttons) under them.
//! Aiming lives in `tab_aim`, keyboard input in `tab_keys`.
//!
//! All input is trusted CDP input (`Input.dispatch*`). Nothing here clicks via
//! `element.click()`, focuses via `element.focus()`, or scrolls via `scrollIntoView()`:
//! those produce untrusted events or no input events at all, both of which detectors see.

use std::time::Duration;

use aibrowse_input::{Modifiers, Pace, Point, parse_chord, plan_click, plan_move, plan_scroll};
use serde_json::json;
use tokio::time::{Instant, sleep, sleep_until};

use crate::aim_cover::{Aim, Reach};
use crate::error::{BrowserError, Result};
use crate::frames::ElementRef;
use crate::isolated::{FOCUSED_VALUE, IsolatedWorld};
use crate::range_scripts::{
    RANGE_GEOMETRY, RANGE_VALUE, RangeGeometry, SLIDER_INDEX, SLIDER_MARK, SLIDERS, SliderMark,
};
use crate::settle::lock;
use crate::tab::{ActionResult, Tab};
use crate::tab_keys::select_all_chord;

/// A link opened in a background tab: what the click did on this page, and the URL the
/// new tab was opened at, to claim it with `Browser::take_opened`.
#[derive(Debug, Clone)]
pub struct Opened {
    pub result: ActionResult,
    pub url: String,
}

/// Times a click aims again when the page moved under the hand on its way.
const REAIMS: usize = 2;

/// Key presses at most when setting a slider: Page Up/Down cover a range in about ten.
const MAX_RANGE_KEYS: usize = 60;
/// Times a slider may be drawn anew while being set, and how long the page gets to draw
/// it.
const MAX_REDRAWS: usize = 12;
const SLIDER_REDRAW_WAIT: Duration = Duration::from_secs(3);

/// How close to the target value (a share of the slider's range) needs no dragging.
const RANGE_CLOSE: f64 = 0.02;
/// How far from the target (a share of the range) a slider is dragged before the keys
/// take over.
const RANGE_FAR: f64 = 0.1;
/// How often, and how many times, a slider's track is measured until its size holds.
const GEOMETRY_POLL: Duration = Duration::from_millis(100);
const GEOMETRY_POLLS: usize = 15;
/// The narrowest track a slider is dragged along: narrower, it is still being laid out.
const MIN_TRACK: f64 = 40.0;

/// How long a page that opened a link in the background is watched for changes.
const OPEN_SETTLE: Duration = Duration::from_millis(1000);

/// How long an input event that opened a window may stay unacknowledged before the
/// action moves on (see `dispatch_input`).
const WINDOW_GRACE: Duration = Duration::from_secs(2);

impl Tab {
    pub async fn click(&mut self, element: ElementRef) -> Result<ActionResult> {
        // Named before clicking: the element may be gone afterwards. The name ends up in
        // the result and in the API catalog, as what triggered the requests it caused.
        let action = match self.element_label(element).await {
            Some(label) => format!("click {label}"),
            None => "click".to_owned(),
        };
        let start = self.begin();
        let hand = self.take_hand().await?;
        self.click_element(element).await?;
        drop(hand);
        self.finish(&action, start, Self::settle_timeout(), Vec::new())
            .await
    }

    /// Opens a link in a background tab, the way a person does: holding Cmd (macOS) or
    /// Ctrl while clicking it. This page stays as it is; the new tab is the browser's
    /// to hand out (`Browser::take_opened` with the returned URL).
    pub async fn open_in_background(&mut self, element: ElementRef) -> Result<Opened> {
        let Some(url) = self.link_href(element).await else {
            return Err(BrowserError::Element(format!(
                "element {element} is not a web link; only links open in a new tab"
            )));
        };
        let action = match self.element_label(element).await {
            Some(label) => format!("open in a new tab {label}"),
            None => "open in a new tab".to_owned(),
        };
        let start = self.begin();
        let hand = self.take_hand().await?;
        let target = self.aim(element).await?;
        self.move_cursor(target.point, target.width).await?;
        let modifier = background_tab_modifier();
        self.hold_modifier(modifier, true).await?;
        // The finger is on the key before the click and stays there through it.
        self.human_pause(120.0).await;
        let pressed = self.press_here().await;
        self.human_pause(80.0).await;
        self.hold_modifier(modifier, false).await?;
        drop(hand);
        pressed?;
        // This page stays as it was; a short look confirms it, where a full settle
        // would wait out whatever the page keeps doing for each link opened.
        let result = self.finish(&action, start, OPEN_SETTLE, Vec::new()).await?;
        Ok(Opened { result, url })
    }

    pub async fn hover(&mut self, element: ElementRef) -> Result<ActionResult> {
        let start = self.begin();
        let hand = self.take_hand().await?;
        let target = self.aim(element).await?;
        self.move_cursor(target.point, target.width).await?;
        drop(hand);
        self.finish("hover", start, Self::settle_timeout(), Vec::new())
            .await
    }

    /// Clicks a field, replaces its content with `text`, and verifies what it now holds.
    pub async fn fill(&mut self, element: ElementRef, text: &str) -> Result<ActionResult> {
        // A native dropdown takes no typing: the option the text names is chosen, as
        // `select` does (Rightmove's 56 prices, too many to offer one by one).
        if self.is_native_select(element).await {
            return self.select(element, text).await;
        }
        let start = self.begin();
        let mut notes = Vec::new();
        let hand = self.take_hand().await?;
        let aim = match self.reach_field(element).await? {
            Reach::Click(aim) => {
                self.move_cursor(aim.point, aim.width).await?;
                self.press_here().await?;
                aim
            }
            Reach::Focused { aim, cover } => {
                notes.push(format!(
                    "the field was covered by {cover}, so it was focused from the keyboard instead of clicked"
                ));
                aim
            }
        };
        self.human_pause(150.0).await;
        self.press_spec(&select_all_chord()).await?;
        if text.is_empty() {
            self.press_spec(&parse_chord("Backspace").expect("Backspace is a named key"))
                .await?;
        } else {
            self.type_strokes(text).await?;
        }
        drop(hand);
        self.last_field = Some((aim.session.clone(), aim.frame_id.clone()));
        // The field itself, wherever the focus went: a date field that opens its picker
        // on focus takes no typing, and the focus is then in the picker, which holds no
        // value to read back (Trenitalia's: "20/11/2026" typed, the search ran for today).
        let own = match IsolatedWorld::create(&aim.session, &aim.frame_id, 0).await {
            Ok(world) => world
                .call_on(&aim.session, element.backend, FIELD_VALUE, &[])
                .await
                .ok(),
            Err(_) => None,
        };
        let stayed_empty = own
            .as_ref()
            .and_then(|v| v.as_str())
            .is_some_and(|v| v.trim().is_empty() && !text.trim().is_empty());
        if stayed_empty {
            notes.push(FIELD_STAYED_EMPTY.to_owned());
        }
        // Read back in the element's own frame, where focus now is.
        if !stayed_empty
            && let Ok(world) = IsolatedWorld::create(&aim.session, &aim.frame_id, 0).await
            && let Ok(value) = world.evaluate(&aim.session, FOCUSED_VALUE).await
            && let Some(value) = value.as_str()
            && value != text
        {
            notes.push(if value.trim().is_empty() && !text.trim().is_empty() {
                FIELD_STAYED_EMPTY.to_owned()
            } else {
                format!(
                    "the focused field now holds {value:?}, not the requested text (input mask, autocomplete, or length limit?)"
                )
            });
        }
        self.finish("fill", start, Self::settle_timeout(), notes)
            .await
    }

    /// Whether the element is a native `<select>`.
    async fn is_native_select(&self, element: ElementRef) -> bool {
        let Ok(session) = self.session_for(element.target) else {
            return false;
        };
        session
            .call(
                "DOM.describeNode",
                json!({ "backendNodeId": element.backend }),
            )
            .await
            .is_ok_and(|d| d["node"]["nodeName"].as_str() == Some("SELECT"))
    }

    /// Types into whatever currently has focus.
    pub async fn type_text(&mut self, text: &str) -> Result<ActionResult> {
        let start = self.begin();
        let hand = self.take_hand().await?;
        self.type_strokes(text).await?;
        drop(hand);
        self.finish("type", start, Self::settle_timeout(), Vec::new())
            .await
    }

    /// Presses a key or chord, e.g. "Enter", "Escape", "Shift+Tab", "Meta+a".
    pub async fn press(&mut self, chord: &str) -> Result<ActionResult> {
        let spec = parse_chord(chord).ok_or_else(|| BrowserError::UnknownKey(chord.to_owned()))?;
        let start = self.begin();
        let hand = self.take_hand().await?;
        self.press_spec(&spec).await?;
        drop(hand);
        self.finish("press", start, Self::settle_timeout(), Vec::new())
            .await
    }

    /// Presses Enter in the field `fill` just typed into, to submit it (a search,
    /// a spreadsheet cell). Where Enter only starts a new line (a code editor, a message
    /// box), that line is taken back with Backspace: the text stays as given, and the
    /// result's first note says so.
    pub async fn submit_field(&mut self) -> Result<ActionResult> {
        let before = self.field_value().await;
        let mut done = self.press("Enter").await?;
        let after = self.field_value().await;
        if let (Some(before), Some(after)) = (before, after)
            && added_a_line(&before, &after)
        {
            let backspace = parse_chord("Backspace").expect("Backspace is a named key");
            let hand = self.take_hand().await?;
            self.press_spec(&backspace).await?;
            drop(hand);
            done.notes.insert(
                0,
                "Enter starts a new line in this field, so it was taken back".to_owned(),
            );
        }
        Ok(done)
    }

    /// What the focused element in the frame of the field `fill` last typed into holds.
    async fn field_value(&self) -> Option<String> {
        let (session, frame_id) = self.last_field.clone()?;
        let world = IsolatedWorld::create(&session, &frame_id, 0).await.ok()?;
        let value = world.evaluate(&session, FOCUSED_VALUE).await.ok()?;
        value.as_str().map(str::to_owned)
    }

    /// Scrolls whatever is under the cursor by (dx, dy) CSS pixels with wheel events.
    pub async fn scroll(&mut self, dx: f64, dy: f64) -> Result<ActionResult> {
        let start = self.begin();
        let hand = self.take_hand().await?;
        self.ensure_cursor_in_viewport().await?;
        self.wheel(dx, dy).await?;
        drop(hand);
        self.finish("scroll", start, Self::settle_timeout(), Vec::new())
            .await
    }

    /// Sets a range slider (`<input type=range>`, `role=slider`, or a handle a widget
    /// library draws without either, in the units its readout shows: see
    /// `range_scripts`) to about `value` as a person does: the keyboard first (Page Up/Down, then the arrows: sites set their
    /// own steps, and keys land on exact values), and where the keys move nothing (a
    /// slider that follows only the pointer), dragging its thumb along the track by the
    /// share of the range to go, once more for what is left. Keys first: on a slider
    /// that commits on release, a drag measured mid-animation overshot (10431 for 5000
    /// at intersport.se, where the keys landed on 5000).
    pub async fn set_range(&mut self, element: ElementRef, value: f64) -> Result<ActionResult> {
        let start = self.begin();
        let hand = self.take_hand().await?;
        let Some(before) = self.range_value(element).await else {
            return Err(BrowserError::Element(format!(
                "element {element} is not a slider with a value"
            )));
        };
        // Far from the target, the thumb is dragged most of the way first, in one
        // change: some panels apply the filter and close on the first change (XXL's and
        // Intersport's price popovers), where key steps got one step in (7655 for
        // 5000). The keys then take it the rest of the way, where the slider stays.
        let mut dragged_away = false;
        let mut start_keys = before;
        if let Some(geometry) = self.steady_range_geometry(element).await {
            let span = geometry.max - geometry.min;
            if span > 0.0 && (value - before).abs() / span > RANGE_FAR {
                self.drag_range(element, value).await?;
                match self.range_value(element).await {
                    Some(after) => start_keys = after,
                    None => dragged_away = true,
                }
            }
        }
        if dragged_away {
            tracing::debug!(before, value, "slider dragged; its panel closed");
            drop(hand);
            let notes = vec![format!(
                "the slider was dragged toward {value}, and its panel closed: the page shows where it stopped"
            )];
            return self
                .finish("set range", start, Self::settle_timeout(), notes)
                .await;
        }
        let mut now = self.keys_toward(element, value, start_keys).await?;
        if (now - before).abs() < f64::EPSILON {
            for _ in 0..2 {
                let Some(geometry) = self.range_geometry(element).await else {
                    break;
                };
                let span = geometry.max - geometry.min;
                if span <= 0.0 || (value - now).abs() / span <= RANGE_CLOSE {
                    break;
                }
                self.drag_range(element, value).await?;
                match self.range_value(element).await {
                    Some(after) => now = after,
                    // Drawn anew once the filter applied: what it reached shows in the page.
                    None => break,
                }
            }
        }
        tracing::debug!(before, now, value, "slider set");
        drop(hand);
        let notes = vec![format!("the slider is now at {now}")];
        self.finish("set range", start, Self::settle_timeout(), notes)
            .await
    }

    /// Moves a slider toward `value` with the keyboard from `now`: Page Up/Down for big
    /// steps, the arrow keys for small ones (sites set their own step sizes), reading
    /// the value after each press. Returns where it got.
    async fn keys_toward(
        &mut self,
        mut element: ElementRef,
        value: f64,
        mut now: f64,
    ) -> Result<f64> {
        let index = self.slider_index(element).await;
        // What makes it this slider, to tell it from another one at the same index once
        // it is gone (a panel that closed rather than drew it anew).
        let mark = self.slider_mark(element).await;
        let mut redrawn = 0;
        // Clicking the thumb focuses it. A slider drawn by the page over a visually
        // hidden range input (MUI's) has nothing of its own to click, and a click can
        // land on the drawn thumb rather than the input under it: then it is focused as
        // the Tab key would, and takes the keys all the same. One that can't take focus
        // follows the pointer only, and the keys do nothing there.
        // The thumb, not the slider's middle: pressing a native range input's track sets
        // the value where it lands, and a panel that applies the first change closes on it.
        let clicked = match self.range_geometry(element).await {
            Some(geometry) if geometry.handle[2] > 0.0 && geometry.handle[3] > 0.0 => {
                self.click_handle(element, &geometry).await.is_ok()
            }
            _ => self.click_element(element).await.is_ok(),
        };
        if !clicked || !self.has_focus(element).await {
            let _ = self
                .session_for(element.target)?
                .call("DOM.focus", json!({ "backendNodeId": element.backend }))
                .await;
        }
        // A click on the track (rather than the thumb) may have moved it.
        now = self.range_value(element).await.unwrap_or(now);
        let key = |up: bool, big: bool| {
            let name = match (up, big) {
                (true, true) => "PageUp",
                (false, true) => "PageDown",
                (true, false) => "ArrowRight",
                (false, false) => "ArrowLeft",
            };
            parse_chord(name).expect("a named key")
        };
        let mut big = true;
        for _ in 0..MAX_RANGE_KEYS {
            let up = value > now;
            if (value - now).abs() < f64::EPSILON {
                break;
            }
            self.press_spec(&key(up, big)).await?;
            let after = match self.range_value(element).await {
                Some(after) => after,
                // Drawn anew (the filter applied): the same slider is found again where
                // it was among the page's sliders, focused, and the keys go on.
                None => {
                    let found = match index {
                        Some(index) if redrawn < MAX_REDRAWS => {
                            self.wait_settled(SLIDER_REDRAW_WAIT).await;
                            self.slider_at(element.target, index).await
                        }
                        _ => None,
                    };
                    let Some(again) = found else { break };
                    let same = match (&mark, self.slider_mark(again).await) {
                        (Some(mark), Some(now)) => mark.same_as(&now),
                        _ => false,
                    };
                    if !same {
                        break;
                    }
                    redrawn += 1;
                    element = again;
                    let _ = self
                        .session_for(element.target)?
                        .call("DOM.focus", json!({ "backendNodeId": element.backend }))
                        .await;
                    match self.range_value(element).await {
                        Some(after) => after,
                        None => break,
                    }
                }
            };
            if (after - now).abs() < f64::EPSILON {
                // At an end, or the key does nothing here: small steps are all that's left.
                if big {
                    big = false;
                    continue;
                }
                break;
            }
            if (value > after) != up && (value - after).abs() > f64::EPSILON {
                // Past the target. A big step is undone and small ones take over; after a
                // small one, the closer of the two values stays.
                if big {
                    self.press_spec(&key(!up, true)).await?;
                    now = self.range_value(element).await.unwrap_or(now);
                    big = false;
                    continue;
                }
                if (value - after).abs() > (value - now).abs() {
                    self.press_spec(&key(!up, false)).await?;
                } else {
                    now = after;
                }
                break;
            }
            now = after;
        }
        Ok(now)
    }

    /// Clicks a slider's thumb (`RangeGeometry::handle`).
    async fn click_handle(&mut self, element: ElementRef, geometry: &RangeGeometry) -> Result<()> {
        let [x, y, w, h] = geometry.handle;
        let (dx, dy) = self.target_offset(element.target).await?;
        self.move_cursor(Point::new(x + w / 2.0 + dx, y + h / 2.0 + dy), w)
            .await?;
        self.press_here().await
    }

    /// Where a slider is among its document's sliders, to find it again once the page
    /// draws it anew (shops apply a price filter on every change and redraw the panel).
    async fn slider_index(&mut self, element: ElementRef) -> Option<i64> {
        let (session, object_id) = self.resolve_in_frame(element).await.ok()?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": SLIDER_INDEX, "objectId": object_id, "returnByValue": true }),
            )
            .await
            .ok()?;
        response["result"]["value"].as_i64().filter(|i| *i >= 0)
    }

    /// The slider at `index` among the sliders of `target`'s documents.
    async fn slider_mark(&mut self, element: ElementRef) -> Option<SliderMark> {
        let (session, object_id) = self.resolve_in_frame(element).await.ok()?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": SLIDER_MARK, "objectId": object_id, "returnByValue": true }),
            )
            .await
            .ok()?;
        serde_json::from_value(response["result"]["value"].clone()).ok()
    }

    async fn slider_at(&mut self, target: u32, index: i64) -> Option<ElementRef> {
        let session = self.session_for(target).ok()?;
        let frames = self.frames().await.ok()?;
        for frame in frames.iter().filter(|f| f.target == target) {
            let Ok(world) = IsolatedWorld::create(&session, &frame.frame_id, 0).await else {
                continue;
            };
            let expression = format!("({SLIDERS})[{index}]");
            if let Ok(Some(backend)) = world.element(&session, &expression).await {
                return Some(ElementRef::new(target, backend));
            }
        }
        None
    }

    /// Whether `element` is the focused element of its document.
    async fn has_focus(&mut self, element: ElementRef) -> bool {
        let Ok((session, object_id)) = self.resolve_in_frame(element).await else {
            return false;
        };
        session
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": "function () { return this.ownerDocument.activeElement === this; }",
                    "objectId": object_id,
                    "returnByValue": true,
                }),
            )
            .await
            .is_ok_and(|r| r["result"]["value"] == true)
    }

    /// The current value of a range slider, `None` when it isn't one.
    async fn range_value(&mut self, element: ElementRef) -> Option<f64> {
        let (session, object_id) = self.resolve_in_frame(element).await.ok()?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": RANGE_VALUE, "objectId": object_id, "returnByValue": true }),
            )
            .await
            .ok()?;
        response["result"]["value"].as_f64()
    }

    pub(crate) async fn click_element(&mut self, element: ElementRef) -> Result<Aim> {
        let mut target = self.aim(element).await?;
        self.move_cursor(target.point, target.width).await?;
        // The page may move while the hand travels (a side panel finishing its close,
        // content loading above): the point is checked again before pressing, and the
        // hand goes on to where the element is now. In Google Sheets a click meant for
        // the Name Box landed on the grid as the Tables panel closed.
        for _ in 0..REAIMS {
            if self.still_reaches(target.point, element).await? {
                break;
            }
            target = self.aim(element).await?;
            self.move_cursor(target.point, target.width).await?;
        }
        self.press_here().await?;
        Ok(target)
    }

    /// A left click where the cursor is, after the dwell a hand needs to settle on it.
    async fn press_here(&mut self) -> Result<()> {
        let timing = plan_click(&self.persona, self.pace, &mut self.rng);
        sleep(timing.dwell).await;
        let p = self.cursor;
        // Input always goes to the page target; Chrome routes it into iframes by
        // position, exactly as for a real mouse.
        self.mouse("mousePressed", p, "left", 1, 1).await?;
        sleep(timing.press).await;
        self.mouse("mouseReleased", p, "left", 0, 1).await
    }

    pub(crate) async fn move_cursor(&mut self, to: Point, target_width: f64) -> Result<()> {
        self.move_along(to, target_width, 0).await
    }

    /// Moves the cursor to `to` along a human path, with `buttons` held (1: the left
    /// button, dragging).
    async fn move_along(&mut self, to: Point, target_width: f64, buttons: u32) -> Result<()> {
        // A drag is a gesture the page follows over time, whatever the pace: at the fast
        // pace's few instant moves Intersport's price slider moved a few pixels (10391
        // for 5000), at a person's it went all the way.
        let pace = if buttons == 0 { self.pace } else { Pace::Human };
        let path = plan_move(
            self.cursor,
            to,
            target_width,
            &self.persona,
            pace,
            &mut self.rng,
        );
        let start = Instant::now();
        let button = if buttons == 0 { "none" } else { "left" };
        for step in path {
            sleep_until(start + step.at).await;
            self.mouse("mouseMoved", step.point, button, buttons, 0)
                .await?;
            self.cursor = step.point;
        }
        Ok(())
    }

    /// Drags a slider's thumb toward `value`: pressed where it is, moved along its track
    /// by the share of the range left to go, released.
    async fn drag_range(&mut self, element: ElementRef, value: f64) -> Result<()> {
        let Some(geometry) = self.range_geometry(element).await else {
            return Ok(());
        };
        let span = geometry.max - geometry.min;
        if span <= 0.0 || geometry.track <= 0.0 {
            return Ok(());
        }
        // The handle is what can be grabbed: the slider itself, or the thumb drawn
        // around a hidden range input.
        let [x, y, w, h] = geometry.handle;
        if w <= 0.0 || h <= 0.0 {
            return Ok(());
        }
        let (dx0, dy0) = self.target_offset(element.target).await?;
        let from = Point::new(x + w / 2.0 + dx0, y + h / 2.0 + dy0);
        self.move_cursor(from, w).await?;
        // The thumb's center travels the track; past its ends the drag only overshoots.
        let dx = (value - geometry.now) / span * geometry.track;
        let (left, right) = (
            geometry.track_left + dx0,
            geometry.track_left + dx0 + geometry.track,
        );
        let to = Point::new((from.x + dx).clamp(left, right), from.y);
        tracing::debug!(
            min = geometry.min,
            max = geometry.max,
            now = geometry.now,
            track = geometry.track,
            ?from,
            ?to,
            "dragging a slider"
        );
        self.mouse("mousePressed", from, "left", 1, 1).await?;
        self.human_pause(80.0).await;
        self.move_along(to, w, 1).await?;
        self.human_pause(60.0).await;
        self.mouse("mouseReleased", to, "left", 0, 1).await
    }

    /// The slider's geometry once its track has stopped changing size: a popover that
    /// is still opening measures its track short (XXL's: 22 px), and a drag measured
    /// then lands near the far end. `None` when it can't be measured.
    async fn steady_range_geometry(&mut self, element: ElementRef) -> Option<RangeGeometry> {
        let mut last: Option<RangeGeometry> = None;
        for _ in 0..GEOMETRY_POLLS {
            let read = self.range_geometry(element).await;
            if let (Some(now), Some(before)) = (&read, &last)
                && now.track >= MIN_TRACK
                && (now.track - before.track).abs() < 0.5
                && (now.track_left - before.track_left).abs() < 0.5
            {
                return read;
            }
            last = read;
            sleep(GEOMETRY_POLL).await;
        }
        None
    }

    /// A slider's range, value and the width of the track its thumb moves along.
    async fn range_geometry(&mut self, element: ElementRef) -> Option<RangeGeometry> {
        let (session, object_id) = self.resolve_in_frame(element).await.ok()?;
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": RANGE_GEOMETRY, "objectId": object_id, "returnByValue": true }),
            )
            .await
            .ok()?;
        serde_json::from_value(response["result"]["value"].clone()).ok()
    }

    pub(crate) async fn wheel(&mut self, dx: f64, dy: f64) -> Result<()> {
        let ticks = plan_scroll(dx, dy, &self.persona, self.pace, &mut self.rng);
        // Ticks are scheduled on an absolute timeline, like cursor moves. Sleeping
        // `after` past each dispatch would add Chrome's acknowledgement time (about a
        // frame) to every gap, turning a 60Hz trackpad stream into 30Hz.
        let mut due = Instant::now();
        for tick in ticks {
            due += tick.after;
            sleep_until(due).await;
            self.dispatch_input(
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mouseWheel",
                    "x": self.cursor.x,
                    "y": self.cursor.y,
                    "deltaX": tick.delta_x,
                    "deltaY": tick.delta_y,
                    "pointerType": "mouse",
                "modifiers": self.held,
                }),
            )
            .await?;
        }
        Ok(())
    }

    async fn mouse(
        &self,
        kind: &str,
        p: Point,
        button: &str,
        buttons: u32,
        click_count: u32,
    ) -> Result<()> {
        self.dispatch_input(
            "Input.dispatchMouseEvent",
            json!({
                "type": kind,
                "x": p.x,
                "y": p.y,
                "button": button,
                "buttons": buttons,
                "clickCount": click_count,
                "pointerType": "mouse",
                "modifiers": self.held,
                // A mouse has no pressure sensor, so per the Pointer Events spec its
                // pointer events report 0.5 while a button is down and 0 otherwise.
                // CDP's default force is 0, which would make every press report
                // pressure 0: a well-known automation tell.
                "force": if buttons == 0 { 0.0 } else { 0.5 },
            }),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn human_pause(&mut self, median_ms: f64) {
        if self.pace == Pace::Human {
            let ms = self.rng_range((median_ms * 0.6) as u64, (median_ms * 1.6) as u64);
            sleep(Duration::from_millis(ms)).await;
        }
    }

    pub(crate) fn rng_range(&mut self, low: u64, high: u64) -> u64 {
        use rand::RngExt;
        self.rng.random_range(low..=high.max(low))
    }
}

impl Tab {
    /// Sends one input event. If the page opens a dialog in response (a click handler
    /// calling confirm()), Chrome doesn't answer until the dialog closes; the dialog is
    /// then the outcome, so stop waiting and let the action report it. The late answer
    /// is discarded by the connection.
    pub(crate) async fn dispatch_input(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<()> {
        let call = self.session.call(method, params);
        let windows_before = lock(&self.activity).windows_opened;
        // Chrome acknowledges an input event only once the page has handled it. A dialog
        // blocks that until someone answers, and a click that opens a new window has
        // been seen to stall it too (rarely, headless); in both cases the input has
        // landed, so waiting longer would only hang the action.
        let input_landed_elsewhere = async {
            let mut window_seen: Option<Instant> = None;
            loop {
                if self.dialog().is_some() {
                    return;
                }
                if lock(&self.activity).windows_opened != windows_before {
                    let seen = *window_seen.get_or_insert_with(Instant::now);
                    if seen.elapsed() >= WINDOW_GRACE {
                        tracing::debug!(method, "input unacknowledged after opening a window");
                        return;
                    }
                }
                sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::select! {
            result = call => { result?; }
            () = input_landed_elsewhere => {}
        }
        Ok(())
    }
}

/// The modifier that makes a click open a link in a background tab.
fn background_tab_modifier() -> u32 {
    if cfg!(target_os = "macos") {
        Modifiers::META
    } else {
        Modifiers::CONTROL
    }
}

/// Whether Enter only added a line to the focused field: the focus stayed in it (nothing
/// submitted, no cell committed, which leaves another element focused or the text
/// gone) and its text grew by line breaks alone. A contenteditable reports the new line
/// as one or two breaks; a code editor may draw it as a glyph (CodeMirror's "↵" on
/// regex101).
fn added_a_line(before: &str, after: &str) -> bool {
    const BREAKS: [char; 2] = ['\n', '\u{21b5}'];
    let (before, after) = (before.replace("\r\n", "\n"), after.replace("\r\n", "\n"));
    let text = before.trim_end_matches(BREAKS);
    let Some(rest) = after.strip_prefix(text) else {
        return false;
    };
    after.len() > before.len()
        && rest.contains(BREAKS)
        && rest
            .chars()
            .all(|c| BREAKS.contains(&c) || c == ' ' || c == '\t')
}

/// What a field holds, called on it: its value, a contenteditable's text, or the text
/// field inside it (a combobox wrapper); `null` when it holds none of those.
const FIELD_VALUE: &str = r#"function() {
  const read = (el) => ('value' in el && typeof el.value === 'string' && el.tagName !== 'BUTTON')
    ? el.value : (el.isContentEditable ? el.innerText : null);
  const own = read(this);
  if (own !== null) return own;
  const inner = this.querySelector('input, textarea, [contenteditable=true]');
  return inner ? read(inner) : null;
}"#;

/// `fill`'s note when the field holds nothing after typing: it doesn't take typed text
/// (a date picker's display, a field filled only from its own list), so the value has to
/// be picked instead.
pub const FIELD_STAYED_EMPTY: &str = "the field is still empty after typing: it doesn't take typed text, so pick the value instead (its picker or list)";

#[cfg(test)]
mod line_tests {
    use super::added_a_line;

    #[test]
    fn enter_that_only_adds_a_line() {
        assert!(added_a_line("abc", "abc\n"));
        assert!(added_a_line("abc", "abc\n\n"));
        assert!(added_a_line("\\d{3}", "\\d{3}\u{21b5}\n"));
        // Submitted: the field emptied, or the focus moved elsewhere.
        assert!(!added_a_line("abc", ""));
        assert!(!added_a_line("abc", "other field"));
        assert!(!added_a_line("abc", "abc"));
    }
}
