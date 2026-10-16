//! What part of the viewport a target's content is seen through, and where in it an
//! element sits comfortably for a click (away from edges where fixed bars live).

use aibrowse_input::Rect;

/// Keep targets this far from viewport edges, where fixed headers and footers live.
pub(crate) const EDGE_MARGIN: f64 = 48.0;

/// What content of one target is seen through, in main-viewport coordinates.
pub(crate) struct View {
    pub viewport: Rect,
    /// The boxes of the frames around the target, intersected; `None` on the page itself.
    pub frames: Option<Rect>,
}

impl View {
    /// Where the target's content can be seen at all. A frame scrolled entirely out of
    /// the viewport leaves the viewport, so that scrolling brings the frame back first.
    pub(crate) fn clip(&self) -> Rect {
        self.frames
            .and_then(|frames| frames.intersect(&self.viewport))
            .unwrap_or(self.viewport)
    }

    /// Whether the element sits where it can be clicked without scrolling: its center
    /// away from the window's edges, where fixed headers and footers live, and, inside
    /// an iframe, in full view through the frame. A frame's own edges need no margin:
    /// the page draws nothing fixed over them, and the frame often can't scroll an
    /// element at its top or bottom any further in.
    pub(crate) fn comfortable(&self, rect: &Rect) -> bool {
        self.comfortable_across(rect) && {
            let zone = inset(&self.viewport, EDGE_MARGIN);
            let center = rect.center().y;
            center >= zone.y
                && center <= zone.y + zone.height
                && self
                    .frames
                    .is_none_or(|f| span_in_view(rect.y, rect.height, f.y, f.height))
        }
    }

    /// The horizontal half of `comfortable`: the element in full view across. The edge
    /// margin is a vertical concern (fixed headers and footers); a logo or sidebar link
    /// near the left edge is perfectly clickable, and scrolling sideways to center it is
    /// something people don't do (on Linux a sideways wheel even navigates back).
    pub(crate) fn comfortable_across(&self, rect: &Rect) -> bool {
        span_in_view(rect.x, rect.width, self.viewport.x, self.viewport.width)
            && self
                .frames
                .is_none_or(|f| span_in_view(rect.x, rect.width, f.x, f.width))
    }
}

/// Whether a span of an element is in full view through a frame's span, or, for an
/// element larger than the frame, at least its middle is. A pixel of slack absorbs
/// sub-pixel layout.
fn span_in_view(start: f64, len: f64, frame_start: f64, frame_len: f64) -> bool {
    let frame_end = frame_start + frame_len;
    if len <= frame_len {
        start >= frame_start - 1.0 && start + len <= frame_end + 1.0
    } else {
        let middle = start + len / 2.0;
        middle >= frame_start && middle <= frame_end
    }
}

/// `rect` shrunk by `margin` on every side; small rectangles get a proportionally
/// smaller margin, so the result never collapses.
pub(crate) fn inset(rect: &Rect, margin: f64) -> Rect {
    let margin_x = margin.min(rect.width / 4.0);
    let margin_y = margin.min(rect.height / 4.0);
    Rect::new(
        rect.x + margin_x,
        rect.y + margin_y,
        rect.width - 2.0 * margin_x,
        rect.height - 2.0 * margin_y,
    )
}

/// Whether two reads of an element's position agree (sub-pixel jitter aside).
pub(crate) fn same_place(a: &Rect, b: &Rect) -> bool {
    (a.y - b.y).abs() < 1.0 && (a.x - b.x).abs() < 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case that made clicks in iframes flaky: the viewport's reading position lay
    /// at the frame's bottom edge, leaving the element mostly clipped by the frame.
    #[test]
    fn an_element_clipped_by_its_frame_is_not_comfortable() {
        let view = View {
            viewport: Rect::new(0.0, 0.0, 1440.0, 757.0),
            frames: Some(Rect::new(438.0, 81.875, 420.0, 260.0)),
        };
        let at_frame_edge = Rect::new(446.0, 329.875, 88.6, 21.5);
        assert!(!view.comfortable(&at_frame_edge));
        let inside = Rect::new(446.0, 200.0, 88.6, 21.5);
        assert!(view.comfortable(&inside));
        // Near the frame's top: no margin needed there, the frame can't scroll further.
        let at_frame_top = Rect::new(446.0, 89.875, 87.1, 21.5);
        assert!(view.comfortable(&at_frame_top));
    }

    #[test]
    fn the_page_keeps_its_edge_margin() {
        let view = View {
            viewport: Rect::new(0.0, 0.0, 1440.0, 757.0),
            frames: None,
        };
        assert!(!view.comfortable(&Rect::new(100.0, 5.0, 80.0, 20.0)));
        assert!(view.comfortable(&Rect::new(100.0, 300.0, 80.0, 20.0)));
        assert_eq!(view.clip(), view.viewport);
    }

    /// A logo or sidebar link hugging the left edge is clickable as it is: scrolling
    /// sideways to center it was a tell (and navigated back on Linux).
    #[test]
    fn elements_near_the_side_edges_need_no_sideways_scroll() {
        let view = View {
            viewport: Rect::new(0.0, 0.0, 1440.0, 757.0),
            frames: None,
        };
        let logo = Rect::new(8.0, 300.0, 40.0, 24.0);
        assert!(view.comfortable_across(&logo));
        assert!(view.comfortable(&logo));
        let cut_off = Rect::new(-30.0, 300.0, 80.0, 24.0);
        assert!(!view.comfortable_across(&cut_off));
    }

    #[test]
    fn elements_larger_than_their_frame_need_only_their_middle_in_view() {
        assert!(span_in_view(0.0, 500.0, 100.0, 300.0));
        assert!(!span_in_view(-400.0, 500.0, 100.0, 300.0));
    }

    #[test]
    fn small_rectangles_keep_a_proportional_inset() {
        let zone = inset(&Rect::new(0.0, 0.0, 100.0, 60.0), EDGE_MARGIN);
        assert_eq!(zone, Rect::new(25.0, 15.0, 50.0, 30.0));
    }
}
