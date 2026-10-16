//! Splitting a flight payload into its rows.
//!
//! Each row is `<hex id>:<tag?><data>`. Most rows end at a newline; text and binary rows
//! instead give their length in bytes (`3:T1a,<26 bytes>`) and may contain newlines.
//! The tag rules mirror React's `ReactFlightClient` (`processStringChunk`): a byte in
//! `LENGTHED` starts a length-prefixed row, any other `A`-`Z` (and `#`, `r`, `x`) is a
//! tag on a newline-terminated row, and anything else is the first byte of a JSON model.

use std::collections::HashMap;

use serde_json::Value;

/// Rows kept from one payload; anything past this is dropped (and reported truncated).
const MAX_ROWS: usize = 50_000;
/// Characters kept of a row that couldn't be understood, shown to the agent as is.
const MAX_RAW_ROW: usize = 2_000;
/// Row ids are hex numbers; more digits than this aren't a row id.
const MAX_ID_DIGITS: usize = 12;

/// Tags followed by a hex byte length and a comma: `T` is text, the rest are typed arrays
/// (`A` ArrayBuffer, `O`/`o` Int8/Uint8, `U` Uint8Clamped, `S`/`s` Int16/Uint16,
/// `L`/`l` Int32/Uint32, `G`/`g` Float32/64, `M`/`m` BigInt64/BigUint64, `V` DataView,
/// `b` a byte-stream chunk).
const LENGTHED: &[u8] = b"TAOobUSsLlGgMmV";

/// One row's content, by what the client would do with it.
#[derive(Debug, Clone)]
pub(super) enum Row {
    /// A JSON model (no tag): the data and element trees, with `$` references.
    Model(Value),
    /// `T`: a long string, referenced from a model.
    Text(String),
    /// `I`: a client module (component) reference: `[id, chunks, name]` or an object.
    Module(Value),
    /// `E`: an error thrown while rendering on the server (`{digest, message?}`).
    Error(Value),
    /// A typed-array row; only its size is kept.
    Binary { tag: char, len: usize },
    /// `R`/`r`/`X`/`x`: a stream whose chunks follow as model rows with the same id.
    Stream(Vec<Value>),
    /// An unknown tag, or a model that isn't valid JSON: kept as raw text.
    Raw { tag: Option<char>, data: String },
}

/// A payload's rows by id.
#[derive(Debug, Default)]
pub(super) struct Rows {
    pub map: HashMap<u64, Row>,
    /// Ids in order of first appearance.
    pub order: Vec<u64>,
    /// Hint (`H`) and development rows (`D`, `W`, `N`, `J`, `#`) skipped: preload hints
    /// and debug info, never data.
    pub skipped: usize,
    /// The payload ended mid-row or had more rows than are kept.
    pub truncated: bool,
    /// Lines that weren't rows at all (no `<hex id>:` prefix).
    pub stray: usize,
}

impl Rows {
    /// Every row of `body`. Never fails: malformed rows become `Row::Raw`, and a payload
    /// that ends mid-row keeps what arrived.
    pub fn parse(body: &str) -> Self {
        let bytes = body.as_bytes();
        let mut rows = Rows::default();
        let mut i = 0;
        while i < bytes.len() {
            // A text row whose declared length doesn't match its content can end inside
            // a character; resume at the next one.
            if !body.is_char_boundary(i) || bytes[i] == b'\n' || bytes[i] == b'\r' {
                i += 1;
                continue;
            }
            if rows.order.len() >= MAX_ROWS {
                rows.truncated = true;
                break;
            }
            let Some((id, after_id)) = row_id(bytes, i) else {
                // Not a row: skip the line, so one bad line doesn't lose the rest.
                let end = line_end(bytes, i);
                rows.stray += 1;
                rows.insert_raw(None, None, &body[i..end]);
                i = end;
                continue;
            };
            i = after_id;
            let tag = bytes.get(i).copied();
            match tag {
                Some(t) if LENGTHED.contains(&t) => {
                    let Some((len, start)) = row_length(bytes, i + 1) else {
                        // A lengthed tag with no length is a model starting with that
                        // letter in some future protocol; keep it as raw text.
                        let end = line_end(bytes, i);
                        rows.insert_raw(Some(id), Some(t as char), &body[i + 1..end]);
                        i = end;
                        continue;
                    };
                    let end = start.saturating_add(len).min(bytes.len());
                    if end < start.saturating_add(len) {
                        rows.truncated = true;
                    }
                    let row = if t == b'T' {
                        Row::Text(String::from_utf8_lossy(&bytes[start..end]).into_owned())
                    } else {
                        Row::Binary {
                            tag: t as char,
                            len: end - start,
                        }
                    };
                    rows.insert(id, row);
                    i = end;
                }
                Some(t) if t.is_ascii_uppercase() || matches!(t, b'#' | b'r' | b'x') => {
                    let end = line_end(bytes, i);
                    rows.truncated |= end == bytes.len() && !body.ends_with('\n');
                    // `t` is ASCII, so `i + 1` is a char boundary.
                    rows.tagged(id, t as char, &body[i + 1..end]);
                    i = end;
                }
                _ => {
                    let end = line_end(bytes, i);
                    rows.truncated |= end == bytes.len() && !body.ends_with('\n');
                    rows.model(id, &body[i..end]);
                    i = end;
                }
            }
        }
        rows
    }

    fn insert(&mut self, id: u64, row: Row) {
        // The first row for an id wins: later ones are stream chunks (handled by the
        // caller) or debug info attached to the same id.
        if !self.map.contains_key(&id) {
            self.order.push(id);
            self.map.insert(id, row);
        }
    }

    fn insert_raw(&mut self, id: Option<u64>, tag: Option<char>, data: &str) {
        let data = crate::payload::truncate(data.trim_end(), MAX_RAW_ROW).to_owned();
        // Lines without an id get ids past any real one, so they still show up.
        let id = id.unwrap_or(u64::MAX - self.order.len() as u64);
        self.insert(id, Row::Raw { tag, data });
    }

    fn model(&mut self, id: u64, data: &str) {
        let parsed = serde_json::from_str::<Value>(data.trim_end());
        if let Some(Row::Stream(items)) = self.map.get_mut(&id) {
            items.push(parsed.unwrap_or_else(|_| Value::String(data.to_owned())));
            return;
        }
        match parsed {
            Ok(value) => self.insert(id, Row::Model(value)),
            Err(_) => self.insert_raw(Some(id), None, data),
        }
    }

    fn tagged(&mut self, id: u64, tag: char, data: &str) {
        let json = || serde_json::from_str::<Value>(data.trim_end());
        match tag {
            'I' => match json() {
                Ok(value) => self.insert(id, Row::Module(value)),
                Err(_) => self.insert_raw(Some(id), Some(tag), data),
            },
            'E' => {
                let value = json().unwrap_or_else(|_| Value::String(data.to_owned()));
                self.insert(id, Row::Error(value));
            }
            'R' | 'r' | 'X' | 'x' => self.insert(id, Row::Stream(Vec::new())),
            // Closing a stream carries an optional final value, rarely data.
            'C' => {}
            'H' | 'D' | 'W' | 'N' | 'J' | '#' => self.skipped += 1,
            _ => self.insert_raw(Some(id), Some(tag), data),
        }
    }
}

/// The hex row id starting at `i` and the index after its colon.
fn row_id(bytes: &[u8], i: usize) -> Option<(u64, usize)> {
    let digits = bytes[i..]
        .iter()
        .take(MAX_ID_DIGITS + 1)
        .take_while(|b| b.is_ascii_hexdigit())
        .count();
    if digits == 0 || digits > MAX_ID_DIGITS || bytes.get(i + digits) != Some(&b':') {
        return None;
    }
    let text = std::str::from_utf8(&bytes[i..i + digits]).ok()?;
    Some((u64::from_str_radix(text, 16).ok()?, i + digits + 1))
}

/// The hex byte length starting at `i` (up to its comma) and the index of the data.
fn row_length(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let digits = bytes[i..]
        .iter()
        .take(17)
        .take_while(|b| b.is_ascii_hexdigit())
        .count();
    if digits == 0 || digits > 16 || bytes.get(i + digits) != Some(&b',') {
        return None;
    }
    let text = std::str::from_utf8(&bytes[i..i + digits]).ok()?;
    Some((usize::from_str_radix(text, 16).ok()?, i + digits + 1))
}

/// The index of the newline ending the line at `i`, or the end of the payload.
fn line_end(bytes: &[u8], i: usize) -> usize {
    bytes[i..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| i + p)
}

/// `"<module id>#<export>"` from an import row: `[id, chunks, name, async?]`
/// (react-server-dom-webpack) or `{id, chunks, name}` (older bundler configs).
pub(super) fn module_name(module: &Value) -> String {
    let (id, name) = match module {
        Value::Array(items) => (items.first(), items.get(2)),
        Value::Object(map) => (map.get("id"), map.get("name")),
        other => (Some(other), None),
    };
    let text = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let (id, name) = (text(id), text(name));
    if name.is_empty() {
        id
    } else {
        format!("{id}#{name}")
    }
}

/// A component's name from its module: the export name when it's a real one, otherwise
/// the module's file name (`./src/components/InvitationCard.tsx#default` →
/// `InvitationCard`).
pub(super) fn component_name(module: &str) -> String {
    let (path, export) = module.rsplit_once('#').unwrap_or((module, ""));
    if !export.is_empty() && export != "default" && export != "*" {
        return export.to_owned();
    }
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    match stem {
        "" | "index" | "page" => path.to_owned(),
        stem if stem.bytes().all(|b| b.is_ascii_digit()) => format!("client:{stem}"),
        stem => stem.to_owned(),
    }
}

/// What an error row says: its message (development builds) or digest (production).
pub(super) fn error_message(error: &Value) -> Value {
    error
        .get("message")
        .or_else(|| error.get("digest"))
        .cloned()
        .unwrap_or_else(|| error.clone())
}
