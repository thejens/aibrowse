use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The subset of `DOMSnapshot.captureSnapshot` we use. Strings are interned: most fields
/// hold indices into `strings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DomSnapshot {
    pub documents: Vec<Document>,
    pub strings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Document {
    #[serde(rename = "documentURL")]
    pub document_url: i64,
    pub frame_id: i64,
    pub title: i64,
    pub nodes: Nodes,
    pub layout: Layout,
    pub scroll_offset_x: f64,
    pub scroll_offset_y: f64,
    /// Document size in the same units as `Layout::bounds`.
    pub content_width: f64,
    pub content_height: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Nodes {
    pub backend_node_id: Vec<i64>,
    pub node_name: Vec<i64>,
    pub attributes: Vec<Vec<i64>>,
    pub is_clickable: RareBoolean,
    /// What `<input>` and `<textarea>` elements hold now, which their `value` attribute
    /// doesn't follow once the field is used.
    pub input_value: RareString,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RareString {
    pub index: Vec<usize>,
    pub value: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RareBoolean {
    pub index: Vec<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Layout {
    pub node_index: Vec<usize>,
    /// Per layout object: computed style values for `crate::STYLES`, as string indices.
    pub styles: Vec<Vec<i64>>,
    /// Per layout object: [x, y, width, height] in document coordinates.
    pub bounds: Vec<Vec<f64>>,
}

/// Everything the fusion step needs to know about one DOM element.
/// What the snapshot says about one frame's document.
#[derive(Debug, Clone, Default)]
pub struct FrameFacts {
    pub elements: HashMap<i64, ElementFacts>,
    /// The document's scroll offset in CSS pixels.
    pub scroll: (f64, f64),
}

#[derive(Debug, Clone, Default)]
pub struct ElementFacts {
    pub tag: String,
    pub attributes: Vec<(String, String)>,
    /// Bounds in document coordinates; `None` when the element generates no box.
    pub bounds: Option<[f64; 4]>,
    pub display: String,
    pub visibility: String,
    pub opacity: String,
    pub cursor: String,
    /// Chrome saw a click listener on it, or it is a navigating anchor.
    pub clickable: bool,
    /// What a text field holds now (see `Nodes::input_value`).
    pub input_value: Option<String>,
}

/// How far past the document's top or left edge an element must end to count as
/// parked out of sight (see `ElementFacts::is_hidden`).
const PARKED: f64 = 2000.0;

impl ElementFacts {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_hidden(&self) -> bool {
        match self.bounds {
            None => true,
            Some([x, y, w, h]) => {
                w <= 0.0 && h <= 0.0
                    // Parked far above or left of the document, where no scrolling
                    // reaches: the "visually hidden" offset (left: -9999px) that apps
                    // use for inputs they draw elsewhere (Google Sheets' cell editor at
                    // y -9998). The margin spares a carousel scrolled a little past the
                    // page's edge.
                    || x + w <= -PARKED
                    || y + h <= -PARKED
                    || self.visibility == "hidden"
                    || self.visibility == "collapse"
                    || (self.opacity == "0" && !self.is_overlay_toggle())
            }
        }
    }

    /// A transparent checkbox, radio button or file input laid over the styled label
    /// that draws it: what is clicked is the input, what is seen is the label (often
    /// `aria-hidden`). Wikipedia's "294 languages" button is a checkbox like that.
    /// Transparent text fields stay hidden: those are mostly traps for bots.
    fn is_overlay_toggle(&self) -> bool {
        self.tag.eq_ignore_ascii_case("input")
            && self.attribute("type").is_some_and(|t| {
                matches!(
                    t.to_ascii_lowercase().as_str(),
                    "checkbox" | "radio" | "file"
                )
            })
    }
}

/// Checkboxes and radio buttons (and file inputs) shrunk to nothing (0x0) and drawn by
/// their `<label for>` instead, as Zalando's filter dialogs do: without a box they would count as hidden,
/// and the dialog would show only its search field and buttons. Each is where its
/// visible label is, which is also what a click on it lands on (the label toggles it).
/// An input without any box (`display: none`) stays hidden.
fn place_boxless_toggles(facts: &mut [ElementFacts]) {
    let boxless: Vec<(usize, String)> = facts
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.is_overlay_toggle() && f.bounds.is_some_and(|[_, _, w, h]| w <= 0.0 && h <= 0.0)
        })
        .filter_map(|(i, f)| Some((i, f.attribute("id").filter(|id| !id.is_empty())?.to_owned())))
        .collect();
    for (i, id) in boxless {
        let label = facts
            .iter()
            .find(|l| l.tag == "label" && l.attribute("for") == Some(id.as_str()) && !l.is_hidden())
            .and_then(|l| l.bounds);
        if let Some(bounds) = label {
            facts[i].bounds = Some(bounds);
        }
    }
}

impl DomSnapshot {
    /// Device-to-CSS pixel factor. On HiDPI displays Chrome may report snapshot geometry
    /// in device pixels; comparing the main document's content width against the CSS
    /// width (from `Page.getLayoutMetrics`) gives the factor to undo that, without needing
    /// the device pixel ratio. All documents of a page share it.
    pub fn scale(&self, css_content_width: f64) -> f64 {
        match self.documents.first() {
            Some(doc) if doc.content_width > 0.0 && css_content_width > 0.0 => {
                doc.content_width / css_content_width
            }
            _ => 1.0,
        }
    }

    /// Per frame id: its elements by backend node id (bounds in CSS pixels, relative to
    /// that frame's document), and the frame document's scroll offset.
    pub fn facts_by_frame(&self, scale: f64) -> HashMap<String, FrameFacts> {
        self.documents
            .iter()
            .map(|doc| {
                let frame_id = self.string(doc.frame_id).to_owned();
                let facts = FrameFacts {
                    elements: self.document_facts(doc, scale),
                    scroll: (doc.scroll_offset_x / scale, doc.scroll_offset_y / scale),
                };
                (frame_id, facts)
            })
            .collect()
    }

    fn string(&self, idx: i64) -> &str {
        usize::try_from(idx)
            .ok()
            .and_then(|i| self.strings.get(i))
            .map_or("", String::as_str)
    }

    fn document_facts(&self, doc: &Document, scale: f64) -> HashMap<i64, ElementFacts> {
        let s = |idx: i64| self.string(idx);
        let nodes = &doc.nodes;
        let mut facts: Vec<ElementFacts> = (0..nodes.backend_node_id.len())
            .map(|i| ElementFacts {
                tag: s(*nodes.node_name.get(i).unwrap_or(&-1)).to_ascii_lowercase(),
                attributes: nodes
                    .attributes
                    .get(i)
                    .map(|pairs| {
                        pairs
                            .chunks(2)
                            .filter(|kv| kv.len() == 2)
                            .map(|kv| (s(kv[0]).to_owned(), s(kv[1]).to_owned()))
                            .collect()
                    })
                    .unwrap_or_default(),
                ..ElementFacts::default()
            })
            .collect();
        for &i in &nodes.is_clickable.index {
            if let Some(f) = facts.get_mut(i) {
                f.clickable = true;
            }
        }
        for (&i, &value) in nodes.input_value.index.iter().zip(&nodes.input_value.value) {
            if let Some(f) = facts.get_mut(i) {
                f.input_value = Some(s(value).to_owned());
            }
        }
        for (layout_idx, &node_idx) in doc.layout.node_index.iter().enumerate() {
            let Some(f) = facts.get_mut(node_idx) else {
                continue;
            };
            // A node can own several layout objects (e.g. ::before); keep the first, which
            // is the element's principal box.
            if f.bounds.is_some() {
                continue;
            }
            if let Some(b) = doc.layout.bounds.get(layout_idx).filter(|b| b.len() == 4) {
                f.bounds = Some([b[0] / scale, b[1] / scale, b[2] / scale, b[3] / scale]);
            }
            if let Some(styles) = doc.layout.styles.get(layout_idx) {
                let style = |i: usize| styles.get(i).map_or("", |&v| s(v)).to_owned();
                f.display = style(0);
                f.visibility = style(1);
                f.opacity = style(2);
                f.cursor = style(3);
            }
        }
        place_boxless_toggles(&mut facts);
        nodes.backend_node_id.iter().copied().zip(facts).collect()
    }

    pub fn title(&self) -> String {
        self.documents
            .first()
            .and_then(|d| usize::try_from(d.title).ok())
            .and_then(|i| self.strings.get(i))
            .cloned()
            .unwrap_or_default()
    }
}
