//! Turning rows into one JSON value: `$` references resolved, element trees compacted.
//!
//! References follow React's `parseModelString`: `$<id>` (also `$L` lazy, `$@` promise,
//! `$w`, `$H`) points at another row, optionally with a path into it (`$1:props:children`);
//! `$$` escapes a literal `$`; `$S` is a symbol; `$h`/`$F` a server action; `$Q`/`$W`/
//! `$i`/`$K` a Map, Set, iterator or FormData outlined to a row; the rest are special
//! values (`$u` undefined, `$I` Infinity, `$D` dates, `$n` BigInts...).
//!
//! Every row is resolved once and reused; a reference back into a row being resolved
//! becomes a marker instead of recursing, and a node budget bounds the output however
//! often rows are reused, so hostile or truncated payloads cost bounded time and memory.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::rows::{Row, Rows, component_name, error_message, module_name};
use super::text::{TextSink, is_dropped_tag, is_noise_prop};

/// Output nodes produced in total, including rows reused by reference.
const MAX_NODES: usize = 200_000;
/// Nesting followed, counting references; deeper parts become a marker.
const MAX_DEPTH: usize = 400;
/// Steps of a path reference (`$1:props:children:0`).
const MAX_PATH: usize = 64;

pub(super) struct Resolver<'a> {
    rows: &'a Rows,
    memo: HashMap<u64, Value>,
    active: HashSet<u64>,
    budget: usize,
    depth: usize,
    pub text: TextSink,
}

impl<'a> Resolver<'a> {
    pub fn new(rows: &'a Rows) -> Self {
        Self {
            rows,
            memo: HashMap::new(),
            active: HashSet::new(),
            budget: MAX_NODES,
            depth: 0,
            text: TextSink::default(),
        }
    }

    /// Whether the node budget ran out somewhere.
    pub fn exhausted(&self) -> bool {
        self.budget == 0
    }

    /// Row `id`, resolved (once; later references reuse it).
    pub fn row(&mut self, id: u64) -> Value {
        if self.active.contains(&id) {
            return Value::String(format!("[cycle: row {id:x}]"));
        }
        if let Some(value) = self.memo.get(&id) {
            let size = nodes(value);
            if size > self.budget {
                self.budget = 0;
                return Value::String(format!("[row {id:x} again, omitted]"));
            }
            self.budget -= size;
            return value.clone();
        }
        let Some(row) = self.rows.map.get(&id) else {
            return Value::String(format!("[row {id:x} not in the response]"));
        };
        self.active.insert(id);
        let value = match row {
            Row::Model(model) => self.value(model),
            Row::Text(text) => Value::String(text.clone()),
            Row::Module(module) => json!({ "$module": module_name(module) }),
            Row::Error(error) => json!({ "$error": error_message(error) }),
            Row::Binary { tag, len } => Value::String(format!("[binary {tag} row, {len} bytes]")),
            Row::Stream(items) => Value::Array(items.iter().map(|v| self.value(v)).collect()),
            Row::Raw { tag, data } => match tag {
                Some(tag) => json!({ "$row": tag.to_string(), "raw": data }),
                None => json!({ "$raw": data }),
            },
        };
        self.active.remove(&id);
        // The kept copy counts against the budget too: otherwise every row on a chain of
        // references above a large subtree would keep its own copy of it.
        let size = if self.budget == 0 { 1 } else { nodes(&value) };
        let kept = if size <= self.budget {
            self.budget -= size;
            value.clone()
        } else {
            self.budget = 0;
            Value::String(format!("[row {id:x} again, omitted]"))
        };
        self.memo.insert(id, kept);
        value
    }

    /// A model value with references resolved and elements compacted.
    pub fn value(&mut self, value: &Value) -> Value {
        if self.budget == 0 {
            return Value::String("[…]".into());
        }
        self.budget -= 1;
        if self.depth >= MAX_DEPTH {
            return Value::String("[too deep]".into());
        }
        self.depth += 1;
        let out = match value {
            Value::String(s) => self.string(s),
            Value::Array(items) if is_element(items) => self.element(items),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.value(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.value(v)))
                    .collect(),
            ),
            other => other.clone(),
        };
        self.depth -= 1;
        out
    }

    fn string(&mut self, s: &str) -> Value {
        let Some(rest) = s.strip_prefix('$') else {
            return Value::String(s.to_owned());
        };
        let Some(kind) = rest.chars().next() else {
            return Value::String(s.to_owned());
        };
        let after = &rest[kind.len_utf8()..];
        match kind {
            '$' => Value::String(rest.to_owned()),
            'L' | '@' | 'w' | 'H' => self.reference(after),
            'S' => Value::String(format!("[{after}]")),
            'h' | 'F' => self.action(after),
            'Q' | 'K' => pairs_to_object(self.reference(after)),
            'W' | 'i' => self.reference(after),
            'u' => Value::Null,
            'I' => Value::String("Infinity".into()),
            'N' => Value::String("NaN".into()),
            '-' if after == "0" => json!(0),
            '-' => Value::String("-Infinity".into()),
            'D' | 'n' => Value::String(after.to_owned()),
            'B' => Value::String("[blob]".into()),
            'Z' => Value::String("[error]".into()),
            'T' => Value::String("[temporary reference]".into()),
            c if c.is_ascii_hexdigit() => self.reference(rest),
            // Development-only encodings (`$E`, `$P`, `$Y`...) and anything newer.
            _ => Value::String(s.to_owned()),
        }
    }

    /// `<hex id>` or `<hex id>:<key>:<key>...`.
    fn reference(&mut self, reference: &str) -> Value {
        let mut parts = reference.split(':');
        let Some(id) = parts.next().and_then(|id| u64::from_str_radix(id, 16).ok()) else {
            return Value::String(format!("${reference}"));
        };
        let path: Vec<&str> = parts.take(MAX_PATH).collect();
        if path.is_empty() {
            return self.row(id);
        }
        match self.walk(id, &path) {
            Some(raw) => self.value(&raw),
            None => Value::String(format!("[${reference} not in the response]")),
        }
    }

    /// The raw model at `path` inside row `id`, following references along the way, as
    /// React does for deduplicated objects (`$1:props:children`).
    fn walk(&self, id: u64, path: &[&str]) -> Option<Value> {
        let mut current = self.raw_row(id)?;
        for key in path {
            current = self.deref(current)?;
            current = match current {
                Value::Array(items) if is_element(items) => match *key {
                    "type" => items.get(1)?,
                    "key" => items.get(2)?,
                    "props" => items.get(3)?,
                    _ => return None,
                },
                Value::Array(items) => items.get(key.parse::<usize>().ok()?)?,
                Value::Object(map) => map.get(*key)?,
                _ => return None,
            };
        }
        Some(current.clone())
    }

    /// Whether a reference names text rather than an element: long text outlined to a
    /// `T` row, or a deduplicated string (`$7:props:children`). Their text is a text
    /// node of the element referencing it.
    fn is_text_reference(&self, reference: &str) -> bool {
        let Some(rest) = reference.strip_prefix('$') else {
            return false;
        };
        let rest = rest.strip_prefix(['L', '@']).unwrap_or(rest);
        let mut parts = rest.split(':');
        let Some(id) = parts.next().and_then(|id| u64::from_str_radix(id, 16).ok()) else {
            return false;
        };
        let path: Vec<&str> = parts.take(MAX_PATH).collect();
        let is_text = |raw: &Value| match raw {
            Value::String(s) => !s.starts_with('$') || s.starts_with("$$"),
            Value::Number(_) => true,
            _ => false,
        };
        if !path.is_empty() {
            return self.walk(id, &path).is_some_and(|raw| is_text(&raw));
        }
        match self.rows.map.get(&id) {
            Some(Row::Text(_)) => true,
            Some(Row::Model(model)) => is_text(model),
            _ => false,
        }
    }

    /// Follows plain row references (a bounded number of hops) to the model they name.
    fn deref<'v>(&'v self, mut value: &'v Value) -> Option<&'v Value> {
        for _ in 0..MAX_PATH {
            let Some(id) = value.as_str().and_then(row_reference) else {
                return Some(value);
            };
            value = self.raw_row(id)?;
        }
        None
    }

    fn raw_row(&self, id: u64) -> Option<&'a Value> {
        match self.rows.map.get(&id)? {
            Row::Model(value) => Some(value),
            _ => None,
        }
    }

    /// A server action reference: the row holds `{id, bound}`; the id is what the site
    /// sends back (Next.js: the `Next-Action` header) to call it.
    fn action(&mut self, reference: &str) -> Value {
        let resolved = self.reference(reference);
        let id = resolved.get("id").cloned().unwrap_or(resolved.clone());
        match resolved.get("bound").filter(|b| !b.is_null()) {
            Some(bound) => json!({ "$action": id, "bound": bound }),
            None => json!({ "$action": id }),
        }
    }

    /// `["$", type, key, props, ...]` (dev builds add owner, stack and validation info).
    fn element(&mut self, items: &[Value]) -> Value {
        let props = items
            .get(3)
            .and_then(|p| self.deref(p))
            .cloned()
            .unwrap_or(Value::Null);
        let name = self.element_name(&items[1]);
        // A tag name written inline is an HTML element; components are references.
        let host = items[1].as_str().is_some_and(|k| !k.starts_with('$'));
        if host && name.as_deref().is_some_and(is_dropped_tag) {
            return Value::Null;
        }
        let children = props
            .get("children")
            .map(|c| self.children(c))
            .unwrap_or_default();
        let Some(name) = name else {
            // Fragments, Suspense and other built-ins render just their children.
            return collapse(children);
        };
        let mut out = Map::new();
        for (key, raw) in props.as_object().into_iter().flatten() {
            // Handler props are wiring, except server actions: their id is how the site
            // calls the server.
            let action = raw
                .as_str()
                .is_some_and(|r| r.starts_with("$h") || r.starts_with("$F"));
            if key == "children" || (is_noise_prop(key) && !action) {
                continue;
            }
            let value = self.value(raw);
            // On host elements only scalar attributes are data (`href`, `alt`, `value`);
            // a client component's props are its data, whatever their type.
            let keep = if host {
                matches!(value, Value::String(_) | Value::Number(_))
            } else {
                !value.is_null()
            };
            if keep {
                out.insert(key.clone(), value);
            }
        }
        if host && out.is_empty() {
            return collapse(children);
        }
        out.insert("$el".into(), Value::String(name));
        if !children.is_empty() {
            out.insert("children".into(), collapse(children));
        }
        Value::Object(out)
    }

    /// The element's display name, or `None` for built-ins that only wrap children
    /// (fragments, Suspense: symbols, written inline or outlined to their own row).
    fn element_name(&mut self, kind: &Value) -> Option<String> {
        let symbol = self
            .deref(kind)
            .and_then(Value::as_str)
            .is_some_and(|k| k.starts_with("$S"));
        let kind = kind.as_str()?;
        if symbol {
            return None;
        }
        if !kind.starts_with('$') {
            return Some(kind.to_owned());
        }
        match self.string(kind) {
            Value::Object(map) => match map.get("$module") {
                Some(Value::String(module)) => Some(component_name(module)),
                _ => Some("component".into()),
            },
            Value::String(name) if !name.starts_with('[') => Some(name),
            _ => Some("component".into()),
        }
    }

    /// Children flattened (arrays and wrappers that collapsed into arrays), with what
    /// React renders as nothing dropped and adjacent text nodes joined, as they appear
    /// on screen. Text written directly as children is also collected as visible text.
    fn children(&mut self, raw: &Value) -> Vec<Value> {
        let mut out = Children::default();
        self.child(raw, &mut out);
        self.text.end_run();
        out.items
    }

    fn child(&mut self, raw: &Value, out: &mut Children) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        match raw {
            Value::Array(items) if !is_element(items) => {
                self.depth += 1;
                for item in items {
                    self.child(item, out);
                }
                self.depth -= 1;
            }
            Value::String(s) if !s.starts_with('$') || s.starts_with("$$") => {
                let text = if s.starts_with("$$") { &s[1..] } else { s };
                self.text.push(text);
                out.text(text);
            }
            Value::Number(n) => {
                let text = n.to_string();
                self.text.push(&text);
                out.text(&text);
            }
            raw => {
                let text_node = raw.as_str().is_some_and(|r| self.is_text_reference(r));
                if text_node && let Value::String(text) = self.value(raw) {
                    self.text.push(&text);
                    out.text(&text);
                    return;
                }
                // An element's own text is collected as it resolves: keep it apart from
                // the text around it.
                self.text.end_run();
                let value = self.value(raw);
                out.node(value);
            }
        }
    }
}

/// An element's children as they are collected.
#[derive(Default)]
struct Children {
    items: Vec<Value>,
    /// Whether the last item is a text node that adjacent text continues.
    open: bool,
}

impl Children {
    fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.items.last_mut() {
            Some(Value::String(previous)) if self.open => previous.push_str(text),
            _ => self.items.push(Value::String(text.to_owned())),
        }
        self.open = true;
    }

    /// A resolved child: arrays (collapsed wrappers) are spliced in, what React renders
    /// as nothing is dropped, and text never joins across element boundaries.
    fn node(&mut self, value: Value) {
        self.open = false;
        match value {
            Value::Array(items) => items.into_iter().for_each(|item| self.node(item)),
            Value::Null | Value::Bool(_) => {}
            Value::String(text) if text.is_empty() => {}
            other => self.items.push(other),
        }
    }
}

fn is_element(items: &[Value]) -> bool {
    items.len() >= 4 && items[0] == "$" && items[1].is_string()
}

/// A reference to a whole row (`$1`, `$L1`, `$@1`), as followed inside path references.
fn row_reference(s: &str) -> Option<u64> {
    let rest = s.strip_prefix('$')?;
    let rest = rest.strip_prefix(['L', '@']).unwrap_or(rest);
    if rest.contains(':') {
        return None;
    }
    u64::from_str_radix(rest, 16).ok()
}

fn collapse(mut children: Vec<Value>) -> Value {
    match children.len() {
        0 => Value::Null,
        1 => children.pop().expect("one child"),
        _ => Value::Array(children),
    }
}

/// Map and FormData rows are lists of `[key, value]` pairs.
fn pairs_to_object(value: Value) -> Value {
    let Value::Array(pairs) = &value else {
        return value;
    };
    let mut map = Map::new();
    for pair in pairs {
        match pair.as_array().map(Vec::as_slice) {
            Some([Value::String(key), v]) => {
                map.insert(key.clone(), v.clone());
            }
            _ => return value,
        }
    }
    Value::Object(map)
}

/// Nodes in a value, for the output budget.
fn nodes(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(nodes).sum::<usize>(),
        Value::Object(map) => 1 + map.values().map(nodes).sum::<usize>(),
        _ => 1,
    }
}
