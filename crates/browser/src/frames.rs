//! Iframes. Same-origin iframes share their page's renderer and CDP session; cross-origin
//! iframes run out of process (OOPIFs) and are separate targets with their own sessions.
//!
//! Each target gets a number: 0 for the page itself (including its same-process frames),
//! then 1, 2, ... for OOPIFs in the order they attach. Element refs carry that number
//! (`e45` on the page, `f2e45` in OOPIF 2) because DOM node ids are only unique within a
//! target.
//!
//! Coordinates: within one target, CDP geometry (content quads, box models) is relative
//! to that target's root viewport; same-process iframes are already folded in. An OOPIF's
//! geometry must be shifted by where its `<iframe>` element sits in the parent target.

use std::fmt;
use std::sync::{Arc, Mutex};

use aibrowse_cdp::Session;
use aibrowse_input::Rect;
use serde_json::{Value, json};

use crate::calibrate::UserAgentIdentity;
use crate::error::{BrowserError, Result};
use crate::settle::{SharedActivity, spawn_network_tracker};
use crate::tab::Tab;

/// Identifies an element: which target it lives in, and its backend node id there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementRef {
    pub target: u32,
    pub backend: i64,
}

impl ElementRef {
    pub fn new(target: u32, backend: i64) -> Self {
        Self { target, backend }
    }

    /// Parses "e45" or "f2e45" (brackets tolerated, as copied from an outline).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('[').trim_end_matches(']');
        let (target, rest) = match text.strip_prefix('f') {
            Some(rest) => {
                let split = rest.find('e')?;
                (rest[..split].parse().ok()?, &rest[split..])
            }
            None => (0, text),
        };
        Some(Self::new(target, rest.strip_prefix('e')?.parse().ok()?))
    }
}

impl fmt::Display for ElementRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.target == 0 {
            write!(f, "e{}", self.backend)
        } else {
            write!(f, "f{}e{}", self.target, self.backend)
        }
    }
}

/// An out-of-process iframe target attached to a tab.
#[derive(Clone)]
pub(crate) struct ChildTarget {
    pub number: u32,
    /// For OOPIFs the target id is also the root frame's id.
    pub frame_id: String,
    pub session: Session,
}

#[derive(Default)]
pub(crate) struct ChildTargets {
    children: Vec<ChildTarget>,
    next_number: u32,
}

pub(crate) type SharedChildren = Arc<Mutex<ChildTargets>>;

fn lock(children: &SharedChildren) -> std::sync::MutexGuard<'_, ChildTargets> {
    children
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Context needed to bring a newly attached iframe target to the same state as its page.
#[derive(Clone)]
pub(crate) struct FrameSetup {
    pub children: SharedChildren,
    pub activity: SharedActivity,
    pub identity: Option<UserAgentIdentity>,
}

/// Makes Chrome attach to every OOPIF (and worker) under `session`, paused before any of
/// its script runs, and handles each attachment.
pub(crate) fn watch_frames(
    session: &Session,
    setup: FrameSetup,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
    Box::pin(watch_frames_inner(session, setup))
}

async fn watch_frames_inner(session: &Session, setup: FrameSetup) -> Result<()> {
    let events = session.events();
    session
        .call(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        )
        .await?;
    let parent = session.clone();
    tokio::spawn(async move {
        let mut events = events;
        while let Some(event) = events.recv().await {
            match event.method.as_str() {
                "Target.attachedToTarget" => {
                    let session_id = event.params["sessionId"].as_str().unwrap_or_default();
                    let child = parent.connection().session(session_id);
                    let info = &event.params["targetInfo"];
                    tracing::debug!(kind = %info["type"], url = %info["url"], session_id, "target attached");
                    if info["type"] == "iframe" {
                        let frame_id = info["targetId"].as_str().unwrap_or_default().to_owned();
                        tokio::spawn(setup_child(child, frame_id, setup.clone()));
                    } else {
                        // Workers: nothing to configure (the user agent switch covers them),
                        // but they must be resumed and released.
                        let parent = parent.clone();
                        let session_id = session_id.to_owned();
                        tokio::spawn(async move {
                            let _ = child
                                .call("Runtime.runIfWaitingForDebugger", json!({}))
                                .await;
                            let _ = parent
                                .call(
                                    "Target.detachFromTarget",
                                    json!({ "sessionId": session_id }),
                                )
                                .await;
                        });
                    }
                }
                "Target.detachedFromTarget" => {
                    let session_id = event.params["sessionId"].as_str().unwrap_or_default();
                    lock(&setup.children)
                        .children
                        .retain(|c| c.session.id() != Some(session_id));
                }
                _ => {}
            }
        }
    });
    Ok(())
}

async fn setup_child(child: Session, frame_id: String, setup: FrameSetup) {
    let network = child.events();
    let identity = async {
        match &setup.identity {
            Some(identity) => identity.apply(&child).await,
            None => Ok(()),
        }
    };
    // The frame is paused before its first script. Its renderer won't answer
    // renderer-side commands until resumed, so awaiting each one before resuming would
    // deadlock. Instead every command is sent at once, resume last: CDP executes a
    // session's commands in order, so all configuration (notably the user agent
    // override) is in place before the frame runs anything.
    let (page, chooser, dom, net, ident, nested, resume) = tokio::join!(
        child.call("Page.enable", json!({})),
        child.call(
            "Page.setInterceptFileChooserDialog",
            json!({ "enabled": true })
        ),
        child.call("DOM.enable", json!({})),
        child.call("Network.enable", json!({})),
        identity,
        watch_frames(&child, setup.clone()),
        child.call("Runtime.runIfWaitingForDebugger", json!({})),
    );
    let failure = [
        page.err(),
        chooser.err(),
        dom.err(),
        net.err(),
        resume.err(),
    ]
    .into_iter()
    .flatten()
    .map(BrowserError::from)
    .chain([ident.err(), nested.err()].into_iter().flatten())
    .next();
    if let Some(err) = failure {
        tracing::warn!(%err, %frame_id, "could not fully set up iframe");
    } else {
        tracing::debug!(%frame_id, "iframe set up and resumed");
    }
    let mut children = lock(&setup.children);
    children.next_number += 1;
    let number = children.next_number;
    spawn_network_tracker(network, setup.activity.clone(), number);
    children.children.push(ChildTarget {
        number,
        frame_id,
        session: child,
    });
}

/// One step on the way from a frame up to the page: the `<iframe>` element that hosts the
/// frame, and where that element lives.
pub(crate) struct FrameLink {
    /// The target whose session hosts the frame itself.
    pub target: u32,
    /// The target whose session hosts the `<iframe>` element.
    pub parent_target: u32,
    /// The `<iframe>` element's backend node id in `parent_target`.
    pub owner: i64,
    /// The frame's content box in main-viewport coordinates.
    pub rect: Rect,
}

/// A frame in the tab, as needed to observe it.
#[derive(Clone)]
pub struct FrameHandle {
    /// The target (0 = page, n = OOPIF n) whose session hosts this frame.
    pub target: u32,
    pub frame_id: String,
    pub session: Session,
    /// The parent frame, and the backend id of the `<iframe>` element there.
    pub owner: Option<(String, i64)>,
    /// The frame's content box in main-viewport coordinates: [x, y, width, height].
    pub rect: Option<[f64; 4]>,
}

impl Tab {
    pub(crate) fn children(&self) -> Vec<ChildTarget> {
        lock(&self.frames).children.clone()
    }

    pub(crate) fn session_for(&self, target: u32) -> Result<Session> {
        if target == 0 {
            return Ok(self.session.clone());
        }
        self.children()
            .into_iter()
            .find(|c| c.number == target)
            .map(|c| c.session)
            .ok_or_else(|| {
                BrowserError::Element(format!(
                    "frame f{target} is gone (the iframe was removed or reloaded). Call observe again."
                ))
            })
    }

    /// Where a target's root viewport sits in the main viewport.
    pub(crate) async fn target_offset(&self, target: u32) -> Result<(f64, f64)> {
        if target == 0 {
            return Ok((0.0, 0.0));
        }
        let frames = self.frames().await?;
        let child = self.children().into_iter().find(|c| c.number == target);
        let rect = child
            .and_then(|c| frames.into_iter().find(|f| f.frame_id == c.frame_id))
            .and_then(|f| f.rect);
        rect.map(|[x, y, _, _]| (x, y)).ok_or_else(|| {
            BrowserError::Element(format!(
                "frame f{target} has no visible box; it may be hidden"
            ))
        })
    }

    /// The frames an OOPIF target is nested in, from its own root frame up to (not
    /// including) the page's main frame. Empty for the page itself.
    pub(crate) async fn frame_chain(&self, target: u32) -> Result<Vec<FrameLink>> {
        if target == 0 {
            return Ok(Vec::new());
        }
        let hidden = || {
            BrowserError::Element(format!(
                "frame f{target} has no visible box; it may be hidden"
            ))
        };
        let root = self
            .children()
            .into_iter()
            .find(|c| c.number == target)
            .map(|c| c.frame_id)
            .ok_or_else(|| {
                BrowserError::Element(format!(
                    "frame f{target} is gone (the iframe was removed or reloaded). Call observe again."
                ))
            })?;
        let frames = self.frames().await?;
        let mut chain = Vec::new();
        let mut current = root;
        while current != self.main_frame_id {
            let frame = frames
                .iter()
                .find(|f| f.frame_id == current)
                .ok_or_else(hidden)?;
            let (parent_id, owner) = frame.owner.clone().ok_or_else(hidden)?;
            let [x, y, width, height] = frame.rect.ok_or_else(hidden)?;
            let parent = frames
                .iter()
                .find(|f| f.frame_id == parent_id)
                .ok_or_else(hidden)?;
            chain.push(FrameLink {
                target: frame.target,
                parent_target: parent.target,
                owner,
                rect: Rect::new(x, y, width, height),
            });
            current = parent_id;
        }
        Ok(chain)
    }

    /// Every frame in the tab, parents before children, with geometry.
    pub async fn frames(&self) -> Result<Vec<FrameHandle>> {
        // Children first: an OOPIF may also appear in its parent's frame tree, and its
        // own session is the one that can see inside it.
        let mut sources: Vec<(u32, Session)> = self
            .children()
            .into_iter()
            .map(|c| (c.number, c.session))
            .collect();
        sources.push((0, self.session.clone()));

        // (target, session, frame id, parent frame id)
        let mut found: Vec<(u32, Session, String, Option<String>)> = Vec::new();
        for (target, session) in sources {
            let Ok(tree) = session.call("Page.getFrameTree", json!({})).await else {
                continue; // detached meanwhile
            };
            let mut stack = vec![tree["frameTree"].clone()];
            while let Some(node) = stack.pop() {
                let id = node["frame"]["id"].as_str().unwrap_or_default().to_owned();
                let parent = node["frame"]["parentId"].as_str().map(str::to_owned);
                if !found.iter().any(|(_, _, existing, _)| *existing == id) {
                    found.push((target, session.clone(), id, parent));
                }
                if let Some(kids) = node["childFrames"].as_array() {
                    stack.extend(kids.iter().cloned());
                }
            }
        }

        // Resolve owners and rectangles parents-first; each pass places frames whose
        // parent is already placed.
        let mut placed: Vec<FrameHandle> = Vec::new();
        let mut pending = found;
        while !pending.is_empty() {
            let before = pending.len();
            let mut rest = Vec::new();
            for (target, session, frame_id, parent) in pending {
                let Some(parent_id) = parent else {
                    placed.push(FrameHandle {
                        target,
                        frame_id,
                        session,
                        owner: None,
                        rect: None,
                    });
                    continue;
                };
                let Some(parent_frame) = placed.iter().find(|f| f.frame_id == parent_id).cloned()
                else {
                    rest.push((target, session, frame_id, Some(parent_id)));
                    continue;
                };
                let (owner, rect) = owner_geometry(&parent_frame, &frame_id, &placed).await;
                placed.push(FrameHandle {
                    target,
                    frame_id,
                    session,
                    owner: owner.map(|backend| (parent_id, backend)),
                    rect,
                });
            }
            if rest.len() == before {
                break; // parents that vanished mid-walk; drop their orphans
            }
            pending = rest;
        }
        Ok(placed)
    }
}

/// The `<iframe>` element hosting `frame_id` inside `parent`, and its content box in
/// main-viewport coordinates.
async fn owner_geometry(
    parent: &FrameHandle,
    frame_id: &str,
    placed: &[FrameHandle],
) -> (Option<i64>, Option<[f64; 4]>) {
    let Ok(owner) = parent
        .session
        .call("DOM.getFrameOwner", json!({ "frameId": frame_id }))
        .await
    else {
        return (None, None);
    };
    let Some(backend) = owner["backendNodeId"].as_i64() else {
        return (None, None);
    };
    let Ok(model) = parent
        .session
        .call("DOM.getBoxModel", json!({ "backendNodeId": backend }))
        .await
    else {
        return (Some(backend), None); // display:none or not rendered
    };
    let content: Vec<f64> = model["model"]["content"]
        .as_array()
        .map(|q| q.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    if content.len() != 8 {
        return (Some(backend), None);
    }
    // Box model coordinates are relative to the parent's target root viewport.
    let (dx, dy) = target_root_offset(parent.target, placed);
    let rect = [
        content[0] + dx,
        content[1] + dy,
        content[2] - content[0],
        content[5] - content[1],
    ];
    (Some(backend), Some(rect))
}

fn target_root_offset(target: u32, placed: &[FrameHandle]) -> (f64, f64) {
    if target == 0 {
        return (0.0, 0.0);
    }
    // The target's root frame is its frame whose parent lives in another target.
    placed
        .iter()
        .find(|f| {
            f.target == target
                && f.owner.as_ref().is_some_and(|(parent, _)| {
                    placed
                        .iter()
                        .any(|p| p.frame_id == *parent && p.target != target)
                })
        })
        .and_then(|f| f.rect)
        .map_or((0.0, 0.0), |[x, y, _, _]| (x, y))
}

#[cfg(test)]
mod tests {
    use super::ElementRef;

    #[test]
    fn refs_round_trip() {
        for text in ["e45", "f2e45", "f12e7"] {
            assert_eq!(ElementRef::parse(text).unwrap().to_string(), text);
        }
        assert_eq!(ElementRef::parse("[e9]"), Some(ElementRef::new(0, 9)));
        assert!(ElementRef::parse("x9").is_none());
        assert!(ElementRef::parse("fe9").is_none());
    }
}
