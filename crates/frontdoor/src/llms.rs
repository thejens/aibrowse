//! llms.txt (<https://llmstxt.org>): a markdown index a site writes for language models.
//! Its only required part is an H1 title; then usually a `>` summary, and `##` sections
//! listing `[name](url): notes` links.

use serde::{Deserialize, Serialize};

use crate::text::clip;

/// What an llms.txt-style document says about itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmsTxt {
    pub url: String,
    pub title: String,
    pub summary: Option<String>,
    /// Markdown links in the document (a lower bound when `truncated`).
    pub links: usize,
    /// `##` section headings, in order.
    pub sections: Vec<String>,
    /// Its size in bytes, when the server said or we read it all.
    pub bytes: Option<u64>,
    /// Only the beginning was read (the size cap).
    pub truncated: bool,
}

/// Parses an llms.txt-style document, or `None` if it isn't one. Servers that answer
/// every path with their HTML app shell (single-page apps) are common, so the title line
/// is required.
pub fn parse(url: &str, text: &str, truncated: bool) -> Option<LlmsTxt> {
    let text = text.trim_start_matches('\u{FEFF}');
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let title = lines.next()?.strip_prefix("# ")?.trim();
    if title.is_empty() {
        return None;
    }
    let mut summary = Vec::new();
    for line in lines.by_ref() {
        match line.strip_prefix('>') {
            Some(quoted) => summary.push(quoted.trim()),
            None => break,
        }
    }
    let sections = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("## "))
        .map(|s| clip(s, 60))
        .filter(|s| !s.is_empty())
        .collect();
    Some(LlmsTxt {
        url: url.to_owned(),
        title: clip(title, 120),
        summary: (!summary.is_empty()).then(|| clip(&summary.join(" "), 300)),
        links: count_links(text),
        sections,
        bytes: (!truncated).then_some(text.len() as u64),
        truncated,
    })
}

/// Markdown links: `[text](target)`, counted per line.
fn count_links(text: &str) -> usize {
    text.lines()
        .map(|line| {
            let mut count = 0;
            let mut rest = line;
            while let Some(at) = rest.find("](") {
                let after = &rest[at + 2..];
                if rest[..at].contains('[') && after.contains(')') {
                    count += 1;
                }
                rest = after;
            }
            count
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{FEFF}# FastHTML\n\n> FastHTML is a python library which brings together Starlette, Uvicorn,\n> HTMX, and fastcore.\n\nImportant notes:\n\n## Docs\n\n- [FastHTML quick start](https://fastht.ml/docs/tutorials/quickstart.md): A brief overview\n- [HTMX reference](https://github.com/bigskysoftware/htmx/blob/master/www/content/reference.md)\n\n## Optional\n\n- [Starlette docs](https://gist.githubusercontent.com/x/starlette.md) and [more](/more)\n";

    #[test]
    fn parses_title_summary_sections_and_links() {
        let doc = parse("https://fastht.ml/llms.txt", SAMPLE, false).unwrap();
        assert_eq!(doc.title, "FastHTML");
        assert_eq!(
            doc.summary.as_deref(),
            Some(
                "FastHTML is a python library which brings together Starlette, Uvicorn, HTMX, and fastcore."
            )
        );
        assert_eq!(doc.sections, ["Docs", "Optional"]);
        assert_eq!(doc.links, 4);
        assert!(!doc.truncated);
    }

    #[test]
    fn rejects_html_app_shells_and_untitled_text() {
        assert!(parse("u", "<!doctype html><html>", false).is_none());
        assert!(parse("u", "User-agent: *\nDisallow:", false).is_none());
        assert!(parse("u", "#\n", false).is_none());
    }

    #[test]
    fn summary_is_optional() {
        let doc = parse("u", "# Title\n\n## Section\n- [a](b)", true).unwrap();
        assert_eq!(doc.summary, None);
        assert_eq!(doc.links, 1);
        assert_eq!(doc.bytes, None);
    }
}
