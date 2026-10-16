//! Compact JSON shapes inferred from sample payloads, so an agent can see what an
//! endpoint returns (`{elements: [{entityUrn: str, …}], paging: {count: num}}`) without
//! the payload itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Elements of an array merged into its element shape: enough to cover heterogeneous
/// lists (LinkedIn's `included`) without walking huge arrays.
const ARRAY_SAMPLE: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Shape {
    Null,
    Bool,
    Num,
    Str,
    /// Element shape; `None` for arrays only ever seen empty.
    Array(Option<Box<Shape>>),
    Object(BTreeMap<String, Shape>),
    /// Seen as null and as the inner shape.
    Nullable(Box<Shape>),
    /// Seen with incompatible types.
    Any,
}

impl Shape {
    pub fn of(value: &Value) -> Self {
        match value {
            Value::Null => Shape::Null,
            Value::Bool(_) => Shape::Bool,
            Value::Number(_) => Shape::Num,
            Value::String(_) => Shape::Str,
            Value::Array(items) => Shape::Array(
                items
                    .iter()
                    .take(ARRAY_SAMPLE)
                    .map(Shape::of)
                    .reduce(Shape::merge)
                    .map(Box::new),
            ),
            Value::Object(map) => {
                Shape::Object(map.iter().map(|(k, v)| (k.clone(), Shape::of(v))).collect())
            }
        }
    }

    /// The shape covering both samples. Object keys are unioned: a field missing from
    /// one sample is still a field of the endpoint.
    pub fn merge(self, other: Shape) -> Shape {
        match (self, other) {
            (a, b) if a == b => a,
            (Shape::Any, _) | (_, Shape::Any) => Shape::Any,
            (Shape::Null, b) | (b, Shape::Null) => b.nullable(),
            (Shape::Nullable(a), b) | (b, Shape::Nullable(a)) => (*a).merge(b).nullable(),
            (Shape::Array(a), Shape::Array(b)) => Shape::Array(match (a, b) {
                (Some(a), Some(b)) => Some(Box::new(a.merge(*b))),
                (a, b) => a.or(b),
            }),
            (Shape::Object(mut a), Shape::Object(b)) => {
                for (key, shape) in b {
                    let merged = match a.remove(&key) {
                        Some(existing) => existing.merge(shape),
                        None => shape,
                    };
                    a.insert(key, merged);
                }
                Shape::Object(a)
            }
            _ => Shape::Any,
        }
    }

    fn nullable(self) -> Shape {
        match self {
            Shape::Null | Shape::Nullable(_) | Shape::Any => self,
            other => Shape::Nullable(Box::new(other)),
        }
    }

    /// Renders the shape down to `depth` levels of nesting, listing at most `keys` fields
    /// per object.
    pub fn render(&self, depth: usize, keys: usize) -> String {
        let mut out = String::new();
        self.write(&mut out, depth, keys);
        out
    }

    fn write(&self, out: &mut String, depth: usize, keys: usize) {
        match self {
            Shape::Null => out.push_str("null"),
            Shape::Bool => out.push_str("bool"),
            Shape::Num => out.push_str("num"),
            Shape::Str => out.push_str("str"),
            Shape::Any => out.push_str("any"),
            Shape::Nullable(inner) => {
                inner.write(out, depth, keys);
                out.push('?');
            }
            Shape::Array(None) => out.push_str("[]"),
            Shape::Array(Some(inner)) => {
                out.push('[');
                inner.write(out, depth, keys);
                out.push(']');
            }
            Shape::Object(map) if map.is_empty() => out.push_str("{}"),
            Shape::Object(_) if depth == 0 => out.push_str("{…}"),
            Shape::Object(map) => {
                out.push('{');
                for (i, (key, shape)) in map.iter().take(keys).enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(key);
                    out.push_str(": ");
                    shape.write(out, depth - 1, keys);
                }
                if map.len() > keys {
                    out.push_str(&format!(", …+{}", map.len() - keys));
                }
                out.push('}');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_nested_shapes() {
        let shape = Shape::of(&json!({
            "elements": [{"id": 1, "name": "a"}, {"id": 2, "name": null, "extra": true}],
            "paging": {"count": 10, "links": []},
        }));
        assert_eq!(
            shape.render(4, 10),
            "{elements: [{extra: bool, id: num, name: str?}], paging: {count: num, links: []}}"
        );
        assert_eq!(shape.render(1, 10), "{elements: [{…}], paging: {…}}");
        assert_eq!(shape.render(4, 1), "{elements: [{extra: bool, …+2}], …+1}");
    }

    #[test]
    fn merges_samples() {
        let a = Shape::of(&json!({"v": 1}));
        let b = Shape::of(&json!({"v": "x", "w": []}));
        let c = Shape::of(&json!({"w": [1]}));
        assert_eq!(a.merge(b).merge(c).render(3, 10), "{v: any, w: [num]}");
    }
}
