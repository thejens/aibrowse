//! Readable content: the page's main text as compact markdown, for reading articles,
//! documentation, messages and search results. Complements the Page Model, which is
//! about what can be *done* on a page rather than what it *says*.

use std::time::{Duration, Instant};

use aibrowse_browser::{Result, Tab};

/// Characters per `read` page; roughly 5k tokens.
pub const DEFAULT_PAGE_CHARS: usize = 20_000;

/// Runs in the isolated world: invisible to the page, immune to its globals. One
/// function, in three parts that share their names: which region is read, its tables,
/// and the walk that writes it as markdown.
const EXTRACT: &str = concat!(
    "(() => {\n",
    include_str!("read/region.js"),
    include_str!("read/tables.js"),
    include_str!("read/walk.js"),
    "})()"
);

/// The line the reader puts where the element the address's `#fragment` points at starts
/// (RFC 6749's `#section-4.1.2`): digests and the reached check put what follows first.
/// `read` leaves it out. The same string is `MARK` in read/region.js.
pub const FRAGMENT_MARK: &str = "<!-- aibrowse: the address #fragment points here -->";

/// How long `settled_main_text` waits at most for a page's text to finish rendering,
/// and how often it looks.
const SETTLE_MAX: Duration = Duration::from_secs(3);
const SETTLE_POLL: Duration = Duration::from_millis(300);
/// A main text shorter than this, in characters, may be a page still rendering: a
/// banner and a title, the content to come from a script (NVD's vulnerability pages).
const SETTLED_CHARS: usize = 1500;
/// One shorter than this is hardly a page's content at all: it is read again until it
/// grows, even when it holds still for a while (a script fetching the content).
const TINY_CHARS: usize = 500;

/// Returns one page of the readable content, starting at `offset` characters.
pub async fn read(tab: &mut Tab, offset: usize, max_chars: usize) -> Result<String> {
    let markdown = without_mark(&main_text(tab).await?);
    Ok(paginate(&markdown, &tab.url(), offset, max_chars))
}

/// `text` without the fragment mark's line.
pub fn without_mark(text: &str) -> String {
    if !text.contains(FRAGMENT_MARK) {
        return text.to_owned();
    }
    text.lines()
        .filter(|l| l.trim() != FRAGMENT_MARK)
        .collect::<Vec<_>>()
        .join("\n")
        .replace("\n\n\n", "\n\n")
}

/// The page's main text once it has rendered: right after a navigation a page that
/// draws its content with scripts shows little more than its banner (NVD: the ".gov"
/// notice and a title), and a digest or a check of it judged the page empty. A short
/// text is read again every `SETTLE_POLL`, up to `SETTLE_MAX`, until it holds still for
/// two readings, or, while tiny, until it grows. A page with a real text is read once.
pub async fn settled_main_text(tab: &mut Tab) -> Result<String> {
    let mut text = main_text(tab).await?;
    let deadline = Instant::now() + SETTLE_MAX;
    // Readings alike in a row: one alike could be a pause between a page's fetches.
    let mut alike = 0;
    loop {
        let chars = text.chars().count();
        let settled = chars >= SETTLED_CHARS || (chars >= TINY_CHARS && alike >= 2);
        if settled || Instant::now() >= deadline {
            return Ok(text);
        }
        tokio::time::sleep(SETTLE_POLL).await;
        let next = main_text(tab).await?;
        alike = if next == text { alike + 1 } else { 0 };
        text = next;
    }
}

/// The page's readable content as markdown, whole and unpaginated: for a PDF, the
/// document's own text (pdf.rs). Where the address's `#fragment` points, a line of its
/// own reads `FRAGMENT_MARK`.
pub async fn main_text(tab: &mut Tab) -> Result<String> {
    if let Some(text) = crate::pdf::pdf_text(tab).await? {
        return Ok(text);
    }
    let markdown = tab.evaluate_isolated_raw(EXTRACT).await?;
    Ok(markdown.as_str().unwrap_or_default().to_owned())
}

fn paginate(markdown: &str, url: &str, offset: usize, max_chars: usize) -> String {
    if markdown.is_empty() {
        return format!("url: {url}\n(no readable text on this page; try observe or a screenshot)");
    }
    paginate_text(markdown, url, offset, max_chars, "read")
}

/// One page of `text` from `offset` characters, headed by its source URL and ending with
/// how to continue (calling `tool` with the next offset). Pages end at paragraph
/// boundaries where possible.
pub fn paginate_text(text: &str, url: &str, offset: usize, max_chars: usize, tool: &str) -> String {
    let total = text.chars().count();
    let start = offset.min(total);
    let mut chunk: String = text.chars().skip(start).take(max_chars).collect();
    let mut end = start + chunk.chars().count();
    // Prefer to stop at a paragraph or line boundary rather than mid-sentence.
    if end < total
        && let Some(cut) = chunk.rfind("\n\n").or_else(|| chunk.rfind('\n'))
        && cut > chunk.len() / 2
    {
        chunk.truncate(cut);
        end = start + chunk.chars().count();
    }
    let mut out = format!("url: {url}\n");
    if start > 0 || end < total {
        out.push_str(&format!("(characters {start}–{end} of {total})\n"));
    }
    out.push('\n');
    out.push_str(chunk.trim_end());
    if end < total {
        out.push_str(&format!(
            "\n\n… {} more characters. Call {tool} with offset={end} to continue.",
            total - end
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{FRAGMENT_MARK, paginate, without_mark};

    #[test]
    fn the_fragment_mark_is_the_one_read_js_writes() {
        assert!(super::EXTRACT.contains(&format!("'{FRAGMENT_MARK}'")));
        let text = format!("Intro.\n\n{FRAGMENT_MARK}\n\n## Section\n\nBody.");
        assert_eq!(without_mark(&text), "Intro.\n\n## Section\n\nBody.");
    }

    #[test]
    fn short_content_is_returned_whole() {
        let out = paginate("# Title\n\nHello.", "https://x.test/", 0, 100);
        assert!(out.ends_with("# Title\n\nHello."));
        assert!(!out.contains("more characters"));
    }

    #[test]
    fn long_content_pages_at_paragraph_boundaries() {
        let text = (0..50)
            .map(|i| format!("Paragraph {i} text."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let first = paginate(&text, "u", 0, 200);
        assert!(first.contains("Call read with offset="), "{first}");
        let offset: usize = first
            .rsplit("offset=")
            .next()
            .unwrap()
            .trim_end_matches(" to continue.")
            .parse()
            .unwrap();
        let body = first.split("\n\n").nth(1).unwrap();
        assert!(body.starts_with("Paragraph 0"));
        let second = paginate(&text, "u", offset, 200);
        assert!(
            second.contains(&format!("(characters {offset}–")),
            "{second}"
        );
        assert!(!second.contains("Paragraph 0 "));
    }
}
