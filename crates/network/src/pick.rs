//! `api_call`'s `pick`: the part of a JSON response the agent asked for, so a list of
//! 30 search hits can come back as 30 titles rather than 30 full records.
//!
//! A pick is a dot path, with two additions:
//! - `*` steps into every item of a list (or every value of an object);
//! - `{a,b.c}` keeps only those fields, each itself a path, under its path as the key.
//!
//! A field name on a list steps into every item too, so `hits.title` is `hits.*.title`.
//! A number on a list is an index: `items.0`.

use serde_json::{Map, Value};

/// The part of `value` at `path`, or `None` when nothing is there.
pub fn pick(value: &Value, path: &str) -> Option<Value> {
    let steps = parse(path)?;
    at(value, &steps)
}

#[derive(Debug, PartialEq)]
enum Step {
    Key(String),
    Each,
    Fields(Vec<Vec<Step>>),
}

/// Splits `path` at the dots outside braces. `None` when the braces don't balance.
fn parse(path: &str) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    for part in split_outside_braces(path, '.')? {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let step = if part == "*" {
            Step::Each
        } else if let Some(inner) = part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
            let fields = split_outside_braces(inner, ',')?
                .into_iter()
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(parse)
                .collect::<Option<Vec<_>>>()?;
            Step::Fields(fields)
        } else if part.contains(['{', '}']) {
            return None;
        } else {
            Step::Key(part.to_owned())
        };
        steps.push(step);
    }
    Some(steps)
}

fn split_outside_braces(text: &str, separator: char) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.checked_sub(1)?,
            c if c == separator && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    (depth == 0).then(|| {
        parts.push(&text[start..]);
        parts
    })
}

fn at(value: &Value, steps: &[Step]) -> Option<Value> {
    let Some((step, rest)) = steps.split_first() else {
        return Some(value.clone());
    };
    match (step, value) {
        (Step::Key(key), Value::Object(map)) => at(map.get(key)?, rest),
        (Step::Key(key), Value::Array(items)) => match key.parse::<usize>() {
            Ok(index) => at(items.get(index)?, rest),
            Err(_) => each(items.iter(), steps),
        },
        (Step::Each, Value::Array(items)) => each(items.iter(), rest),
        (Step::Each, Value::Object(map)) => each(map.values(), rest),
        (Step::Fields(fields), Value::Object(_)) => {
            let mut kept = Map::new();
            for field in fields {
                if let Some(found) = at(value, field) {
                    kept.insert(name(field), found);
                }
            }
            if kept.is_empty() {
                return None;
            }
            at(&Value::Object(kept), rest)
        }
        (Step::Fields(_), Value::Array(items)) => each(items.iter(), steps),
        _ => None,
    }
}

/// `steps` applied to every item; the items without a match are left out.
fn each<'a>(items: impl Iterator<Item = &'a Value>, steps: &[Step]) -> Option<Value> {
    let found: Vec<Value> = items.filter_map(|item| at(item, steps)).collect();
    (!found.is_empty()).then_some(Value::Array(found))
}

/// The key a kept field goes under: its path as written.
fn name(steps: &[Step]) -> String {
    steps
        .iter()
        .map(|step| match step {
            Step::Key(key) => key.clone(),
            Step::Each => "*".to_owned(),
            Step::Fields(fields) => format!(
                "{{{}}}",
                fields.iter().map(|f| name(f)).collect::<Vec<_>>().join(",")
            ),
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn results() -> Value {
        json!({
            "nbHits": 2,
            "hits": [
                { "title": "Rust MCP", "url": "https://a.dev", "points": 12,
                  "author": { "name": "ada", "karma": 5 }, "_rankingInfo": { "words": 3 } },
                { "title": "Show HN", "points": 4, "author": { "name": "bo" } }
            ]
        })
    }

    #[test]
    fn dot_paths_and_indexes() {
        assert_eq!(pick(&results(), "nbHits"), Some(json!(2)));
        assert_eq!(pick(&results(), "hits.1.title"), Some(json!("Show HN")));
        assert_eq!(pick(&results(), "hits.7"), None);
        assert_eq!(pick(&results(), "nope"), None);
        assert_eq!(pick(&results(), ""), Some(results()));
    }

    #[test]
    fn a_field_of_every_item() {
        let titles = json!(["Rust MCP", "Show HN"]);
        assert_eq!(pick(&results(), "hits.*.title"), Some(titles.clone()));
        assert_eq!(pick(&results(), "hits.title"), Some(titles));
        // Items without the field are left out.
        assert_eq!(pick(&results(), "hits.url"), Some(json!(["https://a.dev"])));
    }

    #[test]
    fn some_fields_of_every_item() {
        assert_eq!(
            pick(&results(), "hits.{title,points,author.name}"),
            Some(json!([
                { "title": "Rust MCP", "points": 12, "author.name": "ada" },
                { "title": "Show HN", "points": 4, "author.name": "bo" }
            ]))
        );
        assert_eq!(
            pick(&results(), "{nbHits,hits.title}"),
            Some(json!({ "nbHits": 2, "hits.title": ["Rust MCP", "Show HN"] }))
        );
    }

    #[test]
    fn every_value_of_an_object() {
        let value = json!({ "a": { "id": 1 }, "b": { "id": 2 } });
        assert_eq!(pick(&value, "*.id"), Some(json!([1, 2])));
    }

    #[test]
    fn unbalanced_braces_pick_nothing() {
        assert_eq!(pick(&results(), "hits.{title"), None);
        assert_eq!(pick(&results(), "hits.title}"), None);
    }
}
