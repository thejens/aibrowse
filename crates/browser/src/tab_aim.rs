//! Aiming at an element before pointer input: scrolling it into view with the wheel,
//! then picking a point on it that a click would actually reach (hit testing through
//! iframes, and naming whatever covers it when nothing does).
//!
//! An element inside an iframe can only be seen, and clicked, through its frame's box,
//! clipped again by every frame around it. Hit testing inside an out-of-process frame
//! sees only that frame's document: it can't tell whether the parent draws something
//! over the frame, and right at the frame's edge (it takes whole document pixels) it
//! can accept a point that Chrome then routes to the parent. So aiming accounts for the
//! frames explicitly: the element is scrolled into full view through its frame, click
//! points are picked inside the frame's box, and each parent target must agree that the
//! point lands on the `<iframe>` leading to the element.

use std::time::Duration;

use aibrowse_cdp::Session;
use aibrowse_input::{Point, Rect, pick_click_point};
use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::aim_cover::{Aim, Blocked, Cover, Hit, Reach, describe_node};
use crate::aim_view::{EDGE_MARGIN, View, inset, same_place};
use crate::error::{BrowserError, Result};
use crate::frames::ElementRef;
use crate::isolated::{
    CONTAINS_HIT, CONTAINS_POINT, COVER_IS_CONTROL, FIELD_DECORATION, INERT_TWIN, IsolatedWorld,
    OVERLAY_CONTROLS,
};
use crate::tab::{Tab, stale};

const MAX_SCROLL_ROUNDS: usize = 8;
const CLICK_ATTEMPTS: usize = 3;
/// How often to re-read an element's position while a wheel scroll lands.
const SCROLL_POLL: Duration = Duration::from_millis(30);
/// How long a wheel scroll gets to show up in the element's position before it counts
/// as having scrolled nothing.
const SCROLL_START_BUDGET: Duration = Duration::from_millis(300);
/// How long a scroll that did start gets to come to rest (smooth scrolling animates).
const SCROLL_SETTLE_BUDGET: Duration = Duration::from_millis(1000);

impl Tab {
    /// Brings the element into view and picks a point on it that is actually clickable,
    /// i.e. the topmost element there is the target (or inside it), not an overlay.
    pub(crate) async fn aim(&mut self, element: ElementRef) -> Result<Aim> {
        self.try_aim(element)
            .await?
            .map_err(|blocked| blocked.into_error(element))
    }

    /// Whether a press at `point` lands on the element now: the page may have moved
    /// since it was aimed at.
    pub(crate) async fn still_reaches(
        &mut self,
        point: Point,
        element: ElementRef,
    ) -> Result<bool> {
        Ok(matches!(
            self.hit_test(point, element).await?,
            Hit::Reaches { .. }
        ))
    }

    /// Reaches a field to type into: by a click when one gets there; otherwise, when what
    /// covers it is just another control drawn over it (a language picker over a
    /// collapsed search box), by focusing it as the Tab key would, which needs no click
    /// to land on it. A dialog, banner or overlay over it is still an error: it is the
    /// page asking for something first.
    ///
    /// A decoration of the field's own (see `Aim::through`) passes a click on to the
    /// field as its page decides, which is not always this field: bahn.de's sends any
    /// click on its date to the day. The field itself is focused instead.
    pub(crate) async fn reach_field(&mut self, element: ElementRef) -> Result<Reach> {
        let blocked = match self.try_aim(element).await? {
            Ok(aim) => {
                let Some(through) = aim.through.clone() else {
                    return Ok(Reach::Click(aim));
                };
                let focused = aim
                    .session
                    .call("DOM.focus", json!({ "backendNodeId": element.backend }))
                    .await
                    .is_ok();
                if !focused {
                    return Ok(Reach::Click(aim));
                }
                return Ok(Reach::Focused {
                    aim: Aim {
                        point: self.cursor,
                        width: 0.0,
                        through: None,
                        ..aim
                    },
                    cover: through,
                });
            }
            Err(blocked) => blocked,
        };
        let Some(cover) = &blocked.cover else {
            return Err(blocked.into_error(element));
        };
        let world = IsolatedWorld::create(&cover.session, &cover.frame_id, 0).await?;
        let by_control = world
            .call_on(&cover.session, cover.node, COVER_IS_CONTROL, &[])
            .await
            .is_ok_and(|v| v.as_bool() == Some(true));
        let focused = by_control
            && cover
                .session
                .call("DOM.focus", json!({ "backendNodeId": element.backend }))
                .await
                .is_ok();
        if !focused {
            return Err(blocked.into_error(element));
        }
        Ok(Reach::Focused {
            aim: Aim {
                point: self.cursor,
                width: 0.0,
                through: None,
                session: cover.session.clone(),
                frame_id: cover.frame_id.clone(),
            },
            cover: blocked.description,
        })
    }

    /// Aims at the element, or says what covers it.
    async fn try_aim(&mut self, element: ElementRef) -> Result<std::result::Result<Aim, Blocked>> {
        self.refuse_document_root(element).await?;
        let rect = self.scroll_into_view(element).await?;
        // Pick points only where the element can be seen: inside the viewport and inside
        // every frame it is nested in.
        let view = self.view(element.target).await?;
        let visible = rect
            .intersect(&view.clip())
            .ok_or_else(|| {
                BrowserError::Element(format!(
                    "element {element} is outside the visible area (scrolled away, or clipped by its frame)"
                ))
            })?;

        let mut blocker = None;
        for _ in 0..CLICK_ATTEMPTS {
            let point = pick_click_point(&visible, &mut self.rng);
            match self.hit_test(point, element).await? {
                Hit::Reaches {
                    session,
                    frame_id,
                    through,
                } => {
                    return Ok(Ok(Aim {
                        point,
                        width: visible.min_side(),
                        through,
                        session,
                        frame_id,
                    }));
                }
                Hit::Blocked(blocked) => blocker = Some(blocked),
            }
        }
        Ok(Err(blocker.expect("at least one attempt")))
    }

    /// What content of `target` is seen through: the viewport, and the boxes of the
    /// frames the target is nested in.
    ///
    /// Same-process iframes inside a target aren't accounted for (an element's own frame
    /// isn't known from its node id), but there the target's hit test already sees the
    /// clip, because one hit test covers every frame in the process.
    async fn view(&self, target: u32) -> Result<View> {
        let viewport = self.viewport().await?;
        let mut frames: Option<Rect> = None;
        for link in self.frame_chain(target).await? {
            let clipped = match frames {
                None => Some(link.rect),
                Some(inner) => inner.intersect(&link.rect),
            };
            frames = Some(clipped.ok_or_else(|| {
                BrowserError::Element(format!(
                    "frame f{target} is clipped away entirely by the frames around it"
                ))
            })?);
        }
        Ok(View {
            viewport: Rect::new(0.0, 0.0, viewport.width, viewport.height),
            frames,
        })
    }

    /// Whether a click at `point` (main-viewport coordinates) reaches the element, or what
    /// is in the way.
    async fn hit_test(&mut self, point: Point, element: ElementRef) -> Result<Hit> {
        // Hit-test inside the element's own target: an out-of-process iframe is opaque to
        // its parent, which would only report the <iframe> element itself.
        let (session, hit) = self.node_at(element.target, point).await?;
        let Some((hit_id, frame_id)) = hit else {
            return Ok(Hit::Blocked(Blocked::by(
                "nothing (no element at that point)".into(),
            )));
        };
        let mut through = None;
        let reaches = hit_id == element.backend || {
            // Containment is checked in an isolated world of the hit node's frame. If the
            // element lives in another frame it can't contain the hit node, and resolving
            // it there fails: that means something else is on top.
            let world = IsolatedWorld::create(&session, &frame_id, 0).await?;
            let pseudo = session
                .call("DOM.describeNode", json!({ "backendNodeId": hit_id }))
                .await
                .is_ok_and(|d| d["node"]["pseudoType"].is_string());
            let contained = if pseudo {
                let (dx, dy) = self.target_offset(element.target).await?;
                let at = [json!(point.x - dx), json!(point.y - dy)];
                world
                    .call_on_values(&session, element.backend, CONTAINS_POINT, &at)
                    .await
            } else {
                world
                    .call_on(&session, element.backend, CONTAINS_HIT, &[hit_id])
                    .await
            };
            let contained = contained.is_ok_and(|v| v.as_bool() == Some(true));
            let part_of = |check: &'static str| {
                let (world, session) = (&world, &session);
                async move {
                    world
                        .call_on(session, element.backend, check, &[hit_id])
                        .await
                        .is_ok_and(|v| v.as_bool() == Some(true))
                }
            };
            let decoration = !contained
                && !pseudo
                && (part_of(FIELD_DECORATION).await || part_of(INERT_TWIN).await);
            if decoration {
                through = Some(describe_node(&session, hit_id).await?);
            }
            contained || decoration
        };
        if !reaches {
            let mut description = describe_node(&session, hit_id).await?;
            // What dismisses it: the overlay's own controls, by their names.
            if let Ok(world) = IsolatedWorld::create(&session, &frame_id, 0).await
                && let Ok(controls) = world.call_on(&session, hit_id, OVERLAY_CONTROLS, &[]).await
                && let Some(controls) = controls.as_str().filter(|c| !c.is_empty())
            {
                description.push_str(&format!(", part of an overlay with {controls}"));
            }
            return Ok(Hit::Blocked(Blocked {
                description,
                cover: Some(Cover {
                    session,
                    frame_id,
                    node: hit_id,
                }),
            }));
        }
        // Inside its own target the point reaches the element; the parents must also let
        // it through. In each target above, the point must land on the <iframe> leading
        // down to the element, not on something drawn over it (a banner over an embed).
        // Same-process frames are skipped: the hit test of their target already
        // descended through them.
        for link in self.frame_chain(element.target).await? {
            if link.target == link.parent_target {
                continue;
            }
            let (parent, hit) = self.node_at(link.parent_target, point).await?;
            match hit {
                Some((id, _)) if id == link.owner => {}
                Some((id, _)) => {
                    return Ok(Hit::Blocked(Blocked::by(describe_node(&parent, id).await?)));
                }
                None => {
                    return Ok(Hit::Blocked(Blocked::by(
                        "nothing (no element at that point)".into(),
                    )));
                }
            }
        }
        Ok(Hit::Reaches {
            session,
            frame_id,
            through,
        })
    }

    /// The topmost node at `point` (main-viewport coordinates) in a target's documents,
    /// as (backend node id, frame id), along with the target's session.
    async fn node_at(&self, target: u32, point: Point) -> Result<(Session, Option<(i64, String)>)> {
        let session = self.session_for(target)?;
        let (dx, dy) = self.target_offset(target).await?;
        // Unlike content quads (viewport coordinates), getNodeForLocation takes document
        // coordinates, so the target's scroll offset must be added. Pointer-events:none
        // elements are skipped by default, matching where a real click would land.
        let metrics = session.call("Page.getLayoutMetrics", json!({})).await?;
        let scroll_x = metrics["cssVisualViewport"]["pageX"]
            .as_f64()
            .unwrap_or(0.0);
        let scroll_y = metrics["cssVisualViewport"]["pageY"]
            .as_f64()
            .unwrap_or(0.0);
        let hit = session
            .call(
                "DOM.getNodeForLocation",
                json!({
                    "x": (point.x - dx + scroll_x).round() as i64,
                    "y": (point.y - dy + scroll_y).round() as i64,
                }),
            )
            .await;
        let hit = match hit {
            Ok(hit) => hit,
            // No node there (outside the target's viewport, for instance).
            Err(aibrowse_cdp::CdpError::Protocol { .. }) => return Ok((session, None)),
            Err(err) => return Err(err.into()),
        };
        let node = match (hit["backendNodeId"].as_i64(), hit["frameId"].as_str()) {
            (Some(id), Some(frame_id)) => Some((id, frame_id.to_owned())),
            _ => None,
        };
        Ok((session, node))
    }

    /// Scrolls with wheel events until the element sits comfortably inside the part of
    /// the viewport it can be seen through. Works for elements inside iframes too: the
    /// wheel scrolls whatever is under the cursor, and the cursor moves over the
    /// element's scroller when needed.
    async fn scroll_into_view(&mut self, element: ElementRef) -> Result<Rect> {
        self.ensure_cursor_in_viewport().await?;
        let mut moved_cursor_toward_target = false;
        for _ in 0..MAX_SCROLL_ROUNDS {
            let rect = self.element_rect(element).await?;
            // Recomputed every round: scrolling the page moves the frames on it.
            let view = self.view(element.target).await?;
            if view.comfortable(&rect) {
                return Ok(rect);
            }
            // Aim to bring the element to a reading position a bit above the middle of
            // what it is seen through. For an element in an iframe that is the frame's
            // box: the viewport's middle may lie outside the frame altogether.
            let clip = view.clip();
            let dy = rect.center().y - (clip.y + clip.height * 0.45);
            let dx = if view.comfortable_across(&rect) {
                0.0
            } else {
                rect.center().x - (clip.x + clip.width / 2.0)
            };
            self.wheel(dx, dy).await?;
            self.human_pause(120.0).await;

            let after = self.rect_after_scroll(element, rect).await?;
            if same_place(&after, &rect) {
                // The wheel scrolled nothing: the element lives in a scroll container that
                // isn't under the cursor. Move the cursor to the visible point nearest the
                // element (likely over its container) and try again, once. The point stays
                // inside the clip, so for an element in an iframe the cursor ends up over
                // that frame rather than beside it.
                if moved_cursor_toward_target {
                    break;
                }
                moved_cursor_toward_target = true;
                let zone = inset(&clip, EDGE_MARGIN);
                let toward = Point::new(
                    after.center().x.clamp(zone.x, zone.x + zone.width),
                    after.center().y.clamp(zone.y, zone.y + zone.height),
                );
                self.move_cursor(toward, 100.0).await?;
            }
        }
        // Not past a part of the page that clips without scrolling: no person can scroll
        // there, and a programmatic scroll shifts an app's fixed layout (Excel's, hiding
        // its Name Box and formula bar behind the ribbon for good).
        if self.behind_unscrollable(element).await {
            return Err(BrowserError::Element(format!(
                "element {element} is out of view in a part of the page that doesn't scroll (hidden by the app's own layout)"
            )));
        }
        // Last resort: a programmatic scroll. It emits scroll events without wheel input,
        // which is a mild tell, but failing the agent's action would be worse.
        tracing::warn!(%element, "wheel scrolling could not reveal element; using DOM.scrollIntoViewIfNeeded");
        self.session_for(element.target)?
            .call(
                "DOM.scrollIntoViewIfNeeded",
                json!({ "backendNodeId": element.backend }),
            )
            .await
            .map_err(|_| stale(element))?;
        self.element_rect(element).await
    }

    /// Whether the element lies outside an ancestor that clips its content without
    /// letting anyone scroll it (`overflow: hidden` or `clip`).
    async fn behind_unscrollable(&mut self, element: ElementRef) -> bool {
        let Ok((session, object_id)) = self.resolve_in_frame(element).await else {
            return false;
        };
        session
            .call(
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": BEHIND_UNSCROLLABLE, "objectId": object_id, "returnByValue": true }),
            )
            .await
            .is_ok_and(|r| r["result"]["value"].as_bool() == Some(true))
    }

    /// The element's position once a wheel scroll has landed.
    ///
    /// The wheel's acknowledgement doesn't mean the scroll is visible in the DOM yet: the
    /// compositor scrolls first and the frame's document learns its new offset a frame
    /// or more later (an out-of-process frame's notably later), and smooth scrolling
    /// animates the offset over several frames. Reading right away can return the old
    /// position, which looks as if the wheel scrolled nothing, or a position mid-way,
    /// which aims at a spot the element is about to leave. So: wait for the position to
    /// change from `before`, then for it to hold still.
    async fn rect_after_scroll(&self, element: ElementRef, before: Rect) -> Result<Rect> {
        let started = Instant::now();
        let mut last = self.element_rect(element).await?;
        loop {
            let moving = !same_place(&last, &before);
            let budget = if moving {
                SCROLL_SETTLE_BUDGET
            } else {
                SCROLL_START_BUDGET
            };
            if started.elapsed() >= budget {
                return Ok(last);
            }
            sleep(SCROLL_POLL).await;
            let next = self.element_rect(element).await?;
            if moving && same_place(&next, &last) {
                return Ok(next);
            }
            last = next;
        }
    }

    pub(crate) async fn ensure_cursor_in_viewport(&mut self) -> Result<()> {
        let viewport = self.viewport().await?;
        let inside = Rect::new(0.0, 0.0, viewport.width, viewport.height).contains(self.cursor);
        if !inside {
            let center = Point::new(viewport.width / 2.0, viewport.height / 2.0);
            self.move_cursor(center, 200.0).await?;
        }
        Ok(())
    }
}

/// Whether `this` lies outside an ancestor that clips without scrolling.
const BEHIND_UNSCROLLABLE: &str = r#"function () {
  const e = this.getBoundingClientRect();
  for (let a = this.parentElement; a; a = a.parentElement) {
    const style = getComputedStyle(a);
    if (!/hidden|clip/.test(style.overflowX + ' ' + style.overflowY)) continue;
    const r = a.getBoundingClientRect();
    if (e.bottom <= r.top || e.top >= r.bottom || e.right <= r.left || e.left >= r.right) return true;
  }
  return false;
}"#;
