//! What a ref names on the page now: refused when it is the document's frame rather
//! than an element (a ref from the previous page), and, for a checkbox or radio button
//! that isn't its own visible hit target, placed at its label.
//!
//! Custom toggles hide the real input and draw its `<label>` instead. Some leave the
//! input a transparent box over the drawing, some shrink it to nothing (Zalando's filter
//! dialogs: 0x0 inputs, each with a visible `<label for>`), some clip it away where it
//! lies under another control's label (OpenStreetMap's mode toggles: three clipped
//! radios stacked at one spot, all under the "Car" label). A person clicks the label,
//! which toggles the input, so that is where the input is aimed at.

use aibrowse_input::Rect;
use serde_json::json;

use crate::error::Result;
use crate::frames::ElementRef;
use crate::isolated::IsolatedWorld;
use crate::tab::{Tab, stale};

/// `this` is an input: its first visible label, if it is a checkbox or radio button
/// that can't be clicked where it is itself. Labels naming its id count as well as
/// `labels`: where a page repeats ids, the browser gives the label to the first control
/// with the id (see `CONTAINS_HIT`).
///
/// It can't be clicked where it is when it has no box worth the name (0x0, 1px), when
/// it is drawn invisible (transparent, clipped, hidden), or when what is on top at its
/// middle is neither it nor one of its own labels: Bootstrap's `btn-check` radios are
/// clipped and stacked under a sibling's label, where a click toggles the sibling.
/// A visible input that is its own hit target is aimed at itself: its label may be a
/// sentence away.
const STAND_IN_LABEL: &str = r#"function () {
  if (this.localName !== 'input' || !/^(checkbox|radio)$/i.test(this.type)) return null;
  const shown = (l) => {
    const r = l.getBoundingClientRect();
    return r.width >= 1 && r.height >= 1 && getComputedStyle(l).visibility !== 'hidden';
  };
  const labels = [...(this.labels || [])];
  if (this.id) {
    for (const l of this.getRootNode().querySelectorAll('label[for]')) {
      if (l.htmlFor === this.id && !labels.includes(l)) labels.push(l);
    }
  }
  const label = labels.find(shown);
  if (!label) return null;
  const box = this.getBoundingClientRect();
  if (box.width < 2 || box.height < 2) return label;
  const style = getComputedStyle(this);
  const clipped = (style.clip && style.clip !== 'auto') || (style.clipPath && style.clipPath !== 'none');
  if (clipped || parseFloat(style.opacity) === 0 || style.visibility === 'hidden') return label;
  // What is on top at its middle, where it can be judged: inside the viewport.
  const x = box.left + box.width / 2;
  const y = box.top + box.height / 2;
  const view = this.ownerDocument.defaultView;
  if (x < 0 || y < 0 || x >= view.innerWidth || y >= view.innerHeight) return null;
  const root = this.getRootNode();
  const hit = (root.elementFromPoint ? root : this.ownerDocument).elementFromPoint(x, y);
  if (!hit || hit === this || this.contains(hit)) return null;
  const on = hit.closest('label');
  return on && labels.includes(on) ? null : label;
}"#;

impl Tab {
    /// Refuses a ref that now names the document's frame rather than an element in it:
    /// the root, `<head>` or `<body>`, or no element at all. Refs are node ids, which a
    /// new document (another renderer after a navigation) numbers afresh from 1, so a ref
    /// from the previous page can name the new page's `<html>`; acting on it would click
    /// the middle of the page. The outline never lists those, except a `<body>` that is
    /// itself an editor (an iframe's rich-text field), which stays usable.
    pub(crate) async fn refuse_document_root(&self, element: ElementRef) -> Result<()> {
        let described = self
            .session_for(element.target)?
            .call(
                "DOM.describeNode",
                json!({ "backendNodeId": element.backend }),
            )
            .await
            .map_err(|_| stale(element))?;
        let node = &described["node"];
        let attrs: Vec<&str> = node["attributes"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let editable = attrs
            .chunks(2)
            .any(|kv| kv[0] == "contenteditable" && kv.get(1).is_none_or(|v| *v != "false"));
        let refused = node["nodeType"].as_i64() != Some(1)
            || match node["localName"].as_str().unwrap_or_default() {
                "html" | "head" => true,
                "body" => !editable,
                _ => false,
            };
        if refused {
            return Err(stale(element));
        }
        Ok(())
    }

    /// The box of the element's visible label, for a checkbox or radio button that can't
    /// be clicked where it is itself (see `STAND_IN_LABEL`). `None` for anything else.
    pub(crate) async fn label_rect(&self, element: ElementRef) -> Option<Rect> {
        let session = self.session_for(element.target).ok()?;
        // Only an input can be one: the rest is told from its node alone, without a
        // script in an isolated world on every aim.
        let described = session
            .call(
                "DOM.describeNode",
                json!({ "backendNodeId": element.backend }),
            )
            .await
            .ok()?;
        let node = &described["node"];
        let attrs: Vec<&str> = node["attributes"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let toggle = attrs.chunks(2).any(|kv| {
            kv[0] == "type"
                && kv.get(1).is_some_and(|t| {
                    t.eq_ignore_ascii_case("checkbox") || t.eq_ignore_ascii_case("radio")
                })
        });
        if node["localName"].as_str() != Some("input") || !toggle {
            return None;
        }
        let frames = self.frames().await.ok()?;
        for frame in frames.iter().filter(|f| f.target == element.target) {
            let Ok(world) = IsolatedWorld::create(&session, &frame.frame_id, 0).await else {
                continue;
            };
            let Ok(object_id) = world.resolve(&session, element.backend).await else {
                continue;
            };
            if object_id.is_empty() {
                continue;
            }
            let label = session
                .call(
                    "Runtime.callFunctionOn",
                    json!({ "functionDeclaration": STAND_IN_LABEL, "objectId": object_id }),
                )
                .await
                .ok()?;
            let label_id = label["result"]["objectId"].as_str()?;
            let described = session
                .call("DOM.describeNode", json!({ "objectId": label_id }))
                .await
                .ok()?;
            let backend = described["node"]["backendNodeId"].as_i64()?;
            return self
                .box_rect(ElementRef::new(element.target, backend))
                .await
                .ok()
                .flatten();
        }
        None
    }
}
