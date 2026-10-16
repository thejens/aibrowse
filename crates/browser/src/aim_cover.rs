//! What aiming at an element finds: a point a click reaches it at, or what covers it.

use aibrowse_input::Point;
use serde_json::json;

use crate::error::{BrowserError, Result};
use crate::frames::ElementRef;

pub(crate) struct Aim {
    pub point: Point,
    pub width: f64,
    /// What the click lands on instead of the element, when that is part of the element's
    /// own widget that passes clicks on to it, described: a decoration of its field
    /// (`FIELD_DECORATION`) or an inert twin drawn over it (`INERT_TWIN`).
    pub through: Option<String>,
    /// Where the element lives, for follow-up reads in its frame.
    pub session: aibrowse_cdp::Session,
    pub frame_id: String,
}

/// How a field to type into was reached.
pub(crate) enum Reach {
    /// A click can land on it, at the aimed point.
    Click(Aim),
    /// It was focused without a click, past the control drawn over it (`cover`).
    Focused { aim: Aim, cover: String },
}

pub(crate) enum Hit {
    Reaches {
        session: aibrowse_cdp::Session,
        frame_id: String,
        through: Option<String>,
    },
    Blocked(Blocked),
}

/// What a click at the element would land on instead.
pub(crate) struct Blocked {
    pub description: String,
    /// The covering node, when it is in the element's own document (one in a parent
    /// frame, over the iframe, is out of reach of the element's frame).
    pub cover: Option<Cover>,
}

pub(crate) struct Cover {
    pub session: aibrowse_cdp::Session,
    pub frame_id: String,
    pub node: i64,
}

impl Blocked {
    pub(crate) fn by(description: String) -> Self {
        Self {
            description,
            cover: None,
        }
    }

    pub(crate) fn into_error(self, element: ElementRef) -> BrowserError {
        BrowserError::Element(format!(
            "element {element} is covered by {} (a dialog, banner or overlay?). Dismiss it first.",
            self.description
        ))
    }
}

/// A short description of a node for error messages, e.g. `<div id="cookie-banner">`.
pub(crate) async fn describe_node(session: &aibrowse_cdp::Session, backend: i64) -> Result<String> {
    let described = session
        .call("DOM.describeNode", json!({ "backendNodeId": backend }))
        .await?;
    let node = &described["node"];
    let name = node["localName"].as_str().unwrap_or("element");
    let attrs: Vec<&str> = node["attributes"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let attr = |key: &str| attrs.chunks(2).find(|kv| kv[0] == key).map(|kv| kv[1]);
    let mut description = format!("<{name}");
    if let Some(id) = attr("id") {
        description.push_str(&format!(" id=\"{id}\""));
    }
    if let Some(class) = attr("class") {
        description.push_str(&format!(
            " class=\"{}\"",
            class.chars().take(60).collect::<String>()
        ));
    }
    if name == "iframe"
        && let Some(src) = attr("src")
    {
        description.push_str(&format!(
            " src=\"{}\"",
            src.chars().take(80).collect::<String>()
        ));
    }
    description.push('>');
    Ok(description)
}
