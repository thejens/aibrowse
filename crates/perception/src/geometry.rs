//! Where outline nodes are relative to what the user can see.

use aibrowse_browser::Viewport;

use crate::model::{Node, NodeKind, Position};

/// Where a box is relative to the visible area.
pub(crate) struct Placement {
    pub position: Position,
    /// Distance from view in viewport heights (0 when visible).
    pub distance: f64,
    /// The visible part of the box in viewport coordinates, when any of it is visible.
    pub rect: Option<[f64; 4]>,
}

/// Places bounds relative to the visible area. Bounds are relative to the node's own
/// frame document; a frame maps them into the main viewport through its `<iframe>` box
/// (`frame_rect`, with the frame document's `frame_scroll`), and a node inside a frame
/// is only visible within that box.
pub(crate) fn place(
    bounds: [f64; 4],
    frame_rect: Option<[f64; 4]>,
    frame_scroll: (f64, f64),
    v: &Viewport,
) -> Placement {
    let [x, y, w, h] = bounds;
    let viewport = [0.0, 0.0, v.width, v.height];
    let (left, top, clip) = match frame_rect {
        None => (x - v.scroll_x, y - v.scroll_y, viewport),
        Some([fx, fy, fw, fh]) => {
            let (sx, sy) = frame_scroll;
            let clip_x0 = fx.max(0.0);
            let clip_y0 = fy.max(0.0);
            let clip_x1 = (fx + fw).min(v.width);
            let clip_y1 = (fy + fh).min(v.height);
            (
                x - sx + fx,
                y - sy + fy,
                [clip_x0, clip_y0, clip_x1 - clip_x0, clip_y1 - clip_y0],
            )
        }
    };
    let [cx, cy, cw, ch] = clip;
    let screens = |pixels: f64| pixels / v.height.max(1.0);
    let (position, distance) = if top + h <= cy {
        (Position::Above, screens(cy - (top + h)))
    } else if top >= cy + ch {
        (Position::Below, screens(top - (cy + ch)))
    } else if left + w <= cx {
        (Position::Left, 0.0)
    } else if left >= cx + cw {
        (Position::Right, 0.0)
    } else {
        (Position::Visible, 0.0)
    };
    let rect = (position == Position::Visible).then(|| {
        let x0 = left.max(cx);
        let y0 = top.max(cy);
        [
            x0,
            y0,
            (left + w).min(cx + cw) - x0,
            (top + h).min(cy + ch) - y0,
        ]
    });
    Placement {
        position,
        distance,
        rect,
    }
}

/// A landmark's own box says little about where its contents are (fixed dialogs are often
/// zero-height wrappers, sections span far beyond the viewport): it is visible when any
/// of its contents is.
pub(crate) fn position_containers(nodes: &mut [Node]) {
    for i in 0..nodes.len() {
        if nodes[i].kind != NodeKind::Structure {
            continue;
        }
        let depth = nodes[i].depth;
        let children = nodes[i + 1..].iter().take_while(|n| n.depth > depth);
        let mut first = None;
        let mut any_visible = false;
        for child in children {
            let Some(position) = child.position else {
                continue;
            };
            first.get_or_insert(position);
            any_visible |= position == Position::Visible;
        }
        if let Some(first) = first {
            nodes[i].position = Some(if any_visible {
                Position::Visible
            } else {
                first
            });
        }
    }
}
