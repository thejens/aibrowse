//! Decoding React Server Components ("flight") responses.
//!
//! Sites built on React Server Components (Next.js App Router, server-driven UIs)
//! answer navigations, server actions and pagination with `text/x-component` bodies:
//! rows of JSON models, text and module references stitched together by `$` references.
//! As raw text that is useless to an agent; decoded, it is the data the page renders
//! (`{"value": {...}, "text": ["Ada Lovelace", "Accept"]}`).
//!
//! The decoder follows React 19's client (`react-client/src/ReactFlightClient.js`) but is
//! lenient: unknown row tags and invalid JSON are kept as raw text, a truncated stream
//! keeps what arrived, and output is bounded however the rows reference each other.

mod resolve;
mod rows;
mod text;

#[cfg(test)]
mod tests;

use serde_json::{Map, Value};

use resolve::Resolver;
use rows::Rows;

/// The media type of flight responses.
pub fn is_flight(content_type: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .contains("text/x-component")
}

/// Whether a response is a flight payload: by its media type, or, for responses served
/// as opaque bytes (`application/octet-stream`, as LinkedIn does) or plain text, by its
/// first row.
pub fn is_flight_response(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    is_flight(&ct)
        || ((ct.is_empty() || ct.contains("octet-stream") || ct.starts_with("text/plain"))
            && looks_like_flight(body))
}

/// A flight stream starts with a row: a hex id, a colon, then a tag letter or a JSON
/// value (`0:{...}`, `1:I[...]`, `2:"$Sreact.suspense"`, `3:T1a2,`).
fn looks_like_flight(body: &str) -> bool {
    let Some((id, rest)) = body.split_once(':') else {
        return false;
    };
    let first = rest.chars().next().unwrap_or(' ');
    !id.is_empty()
        && id.len() <= 8
        && id.bytes().all(|b| b.is_ascii_hexdigit())
        && (matches!(first, '[' | '{' | '"' | '$' | '-')
            || first.is_ascii_digit()
            || first.is_ascii_uppercase()
            || rest.starts_with("null")
            || rest.starts_with("true")
            || rest.starts_with("false"))
}

/// A decoded flight payload.
#[derive(Debug, Clone)]
pub struct Flight {
    /// The root model (row 0) with references resolved and element trees reduced to
    /// their data: `{"$el": "a", "href": ..., "children": ...}` where an element has
    /// data props, just its children where it doesn't. Without a row 0, an object of
    /// every row by hex id.
    pub value: Value,
    /// The text the element trees show, in document order.
    pub text: Vec<String>,
    /// Rows in the payload.
    pub rows: usize,
    /// The payload ended mid-row, or parts were omitted to bound the output.
    pub truncated: bool,
}

impl Flight {
    /// The agent-facing document: `{"text": [...], "value": ...}` (text only when the
    /// payload rendered any), which `pick` paths address.
    pub fn document(&self) -> Value {
        let mut doc = Map::new();
        if !self.text.is_empty() {
            doc.insert("text".into(), self.text.clone().into());
        }
        doc.insert("value".into(), self.value.clone());
        Value::Object(doc)
    }
}

/// Decodes a flight payload. `None` when `body` has no flight rows at all.
pub fn decode(body: &str) -> Option<Flight> {
    let rows = Rows::parse(body);
    if rows.order.len() == rows.stray {
        return None;
    }
    let mut resolver = Resolver::new(&rows);
    let value = if rows.map.contains_key(&0) {
        resolver.row(0)
    } else {
        let mut all = Map::new();
        for &id in &rows.order {
            if resolver.exhausted() {
                break;
            }
            all.insert(format!("{id:x}"), resolver.row(id));
        }
        Value::Object(all)
    };
    let truncated = rows.truncated || resolver.exhausted();
    Some(Flight {
        value,
        text: resolver.text.into_pieces(),
        rows: rows.order.len(),
        truncated,
    })
}
