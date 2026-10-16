//! `aibrowse trace [dir]`: a self-contained HTML page for a trace, written next to it as
//! `index.html`. The events and outlines are embedded in the page (browsers don't let
//! `file://` pages fetch neighbouring files); screenshots are referenced by their
//! relative paths, which `file://` pages may load. No build step, no network.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use super::root;
use super::writer::write_private;
use crate::respond::open_in_browser;

const TEMPLATE: &str = include_str!("viewer.html");
const PLACEHOLDER: &str = "__TRACE_DATA__";
/// Outline characters embedded per step; full outlines stay in `outlines/`.
const MAX_OUTLINE_CHARS: usize = 200_000;

/// Runs the subcommand: `aibrowse trace [dir] [--no-open]`. Without a directory, the
/// most recent trace of any profile.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let open = !args.iter().any(|a| a == "--no-open");
    let dirs: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let dir = match dirs.as_slice() {
        [dir] => PathBuf::from(dir),
        [] => latest().context("no traces found; is AIBROWSE_TRACES set to off?")?,
        _ => bail!("usage: aibrowse trace [dir] [--no-open]"),
    };
    let index = build(&dir)?;
    println!("{}", index.display());
    if open {
        open_in_browser(&index.to_string_lossy());
    }
    Ok(())
}

/// Writes `index.html` into the trace directory `dir` and returns its path.
pub fn build(dir: &Path) -> anyhow::Result<PathBuf> {
    let events_path = dir.join("events.jsonl");
    let raw = fs::read_to_string(&events_path)
        .with_context(|| format!("reading {}", events_path.display()))?;
    let mut events: Vec<Value> = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        // A line cut short (the process was killed mid-write) is skipped, not fatal.
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    events.sort_by_key(|e| e["seq"].as_u64().unwrap_or(0));
    for event in &mut events {
        if let Some(path) = event["outline"].as_str().map(str::to_owned)
            && let Some(text) = read_inside(dir, &path)
        {
            let text: String = text.chars().take(MAX_OUTLINE_CHARS).collect();
            event["outline_text"] = Value::String(text);
        }
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let data = json!({ "name": name, "events": events });
    let page = TEMPLATE.replace(PLACEHOLDER, &script_safe(&data.to_string()));
    let index = dir.join("index.html");
    write_private(&index, page.as_bytes())?;
    Ok(index)
}

/// Reads a file the trace references, refusing paths that leave the trace directory
/// (a trace is data; it doesn't get to pick files to embed).
fn read_inside(dir: &Path, relative: &str) -> Option<String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    fs::read_to_string(dir.join(path)).ok()
}

/// JSON made safe to sit inside a `<script>` element: no sequence in it can end the
/// element or open a comment, whatever the pages put in their text.
fn script_safe(json: &str) -> String {
    json.replace('<', "\\u003c")
}

/// The most recently written trace directory under the traces root.
fn latest() -> Option<PathBuf> {
    let root = root()?;
    fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|profile| fs::read_dir(profile.path()).ok())
        .flatten()
        .flatten()
        .filter(|session| session.path().join("events.jsonl").is_file())
        .max_by_key(|session| session.metadata().and_then(|m| m.modified()).ok())
        .map(|session| session.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_embeds_events_safely() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("outlines")).unwrap();
        fs::write(dir.path().join("outlines/0002.txt"), "[e1] link \"Home\"").unwrap();
        let lines = [
            r#"{"seq":2,"tool":"act","ok":true,"result":"</script><script>alert(1)</script>","outline":"outlines/0002.txt","screenshot":"screens/0002.jpg"}"#,
            r#"{"seq":1,"tool":"navigate","ok":true,"result":"ok","outline":"../../etc/passwd"}"#,
            r#"{"seq":3,"tool":"obs"#,
        ];
        fs::write(dir.path().join("events.jsonl"), lines.join("\n")).unwrap();
        let page = fs::read_to_string(build(dir.path()).unwrap()).unwrap();
        let data = page.split(r#"id="trace-data">"#).nth(1).unwrap();
        let data = &data[..data.find("</script>").unwrap()];
        assert!(!data.contains('<'), "{data}");
        let parsed: Value = serde_json::from_str(data).unwrap();
        let events = parsed["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["seq"], 1);
        assert!(events[0].get("outline_text").is_none());
        assert_eq!(events[1]["outline_text"], "[e1] link \"Home\"");
        assert!(page.contains("screens/0002.jpg"));
    }
}
