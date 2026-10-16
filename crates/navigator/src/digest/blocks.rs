//! The page's markdown cut into headings and passages.

use std::collections::{HashMap, HashSet};

use aibrowse_perception::FRAGMENT_MARK;

/// Prose longer than this, in characters, is split into its lines (its paragraphs).
const PARAGRAPHS_CHARS: usize = 600;
/// A line before a table this short, alone in its passage and not a sentence, names
/// what the table is of.
const LABEL_CHARS: usize = 60;

/// A piece of the page's text: a heading, or a passage (a paragraph, list, quote, code
/// block or table row) under the headings before it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Block {
    /// Heading level (1–6), or 0 for a passage.
    pub(super) level: usize,
    pub(super) text: String,
    /// For a table row: the table's header (its first row and separator), shown once
    /// above the first kept row of that table.
    pub(super) table: Option<usize>,
    /// Whether the element the address's `#fragment` points at starts here.
    pub(super) target: bool,
}

impl Block {
    fn passage(text: String) -> Self {
        Self::new(0, text)
    }

    fn new(level: usize, text: String) -> Self {
        Self {
            level,
            text,
            table: None,
            target: false,
        }
    }
}

/// Cuts markdown into headings and passages. A list or quote stays together; a code
/// block stays whole, blank lines and all, unless it is a long plain-text document
/// (`fenced`); a table becomes one passage per row, so the rows that matter can be kept
/// without the rest. Link targets and images are dropped: a digest is for reading, and
/// `observe` has the links. The fragment mark (see `FRAGMENT_MARK`) marks the block
/// after it as the target.
pub(super) fn blocks(markdown: &str) -> (Vec<Block>, Vec<String>) {
    let mut out = Vec::new();
    let running = running_lines(markdown);
    let mut tables: Vec<String> = Vec::new();
    let mut passage: Vec<String> = Vec::new();
    let mut fenced = false;
    // Inside a fenced block, the line the fragment mark stood before.
    let mut fence_target: Option<usize> = None;
    // The block the fragment mark stood before, outside fenced blocks.
    let mut target: Option<usize> = None;
    let mut table_lines: Vec<String> = Vec::new();
    for raw in markdown.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed == FRAGMENT_MARK {
            if fenced {
                fence_target = Some(passage.len());
            } else {
                flush_table(&mut table_lines, &mut out, &mut tables);
                flush(&mut passage, &mut out);
                target = Some(out.len());
            }
            continue;
        }
        if trimmed.starts_with("```") {
            if fenced {
                passage.push(line.to_owned());
                fenced_block(&passage, fence_target.take(), &running, &mut out);
                passage.clear();
            } else {
                flush(&mut passage, &mut out);
                passage.push(line.to_owned());
            }
            fenced = !fenced;
            continue;
        }
        if fenced {
            passage.push(line.to_owned());
            continue;
        }
        if trimmed.starts_with('|') {
            flush(&mut passage, &mut out);
            table_lines.push(unlink(trimmed));
            continue;
        }
        flush_table(&mut table_lines, &mut out, &mut tables);
        if trimmed.is_empty() {
            flush(&mut passage, &mut out);
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            flush(&mut passage, &mut out);
            out.push(Block::new(hashes, unlink(trimmed[hashes..].trim())));
            continue;
        }
        // A list right after paragraphs of prose, with no blank line between (a shop's
        // category text and then its products), starts a passage of its own: kept
        // together, the products sat past the cut of one long passage. A single line
        // before a list introduces it ("Teams include:") and stays with it; lines after
        // a list item stay with it too (product cards continue their item on lines of
        // their own).
        if is_list_item(trimmed)
            && passage.len() > 1
            && !passage.iter().any(|l| is_list_item(l.trim_start()))
        {
            flush(&mut passage, &mut out);
        }
        passage.push(unlink(line));
    }
    flush_table(&mut table_lines, &mut out, &mut tables);
    if fenced {
        // An unclosed fence: its lines are the rest of the text.
        fenced_block(&passage, fence_target, &running, &mut out);
    } else {
        flush(&mut passage, &mut out);
    }
    if let Some(block) = target.and_then(|t| out.get_mut(t)) {
        block.target = true;
    }
    (out, tables)
}

/// Ends a passage. Lines of prose that run long are paragraphs, each a passage of its
/// own: a page that separates its paragraphs with single line breaks was one passage,
/// kept whole and cut after its first few hundred characters.
fn flush(passage: &mut Vec<String>, out: &mut Vec<Block>) {
    let prose = passage.len() > 1
        && !passage.iter().any(|l| is_list_item(l.trim_start()))
        && passage.iter().map(|l| l.chars().count()).sum::<usize>() > PARAGRAPHS_CHARS;
    let texts = if prose {
        std::mem::take(passage)
    } else {
        vec![passage.join("\n")]
    };
    passage.clear();
    for text in texts {
        // Separators and image-only lines say nothing.
        if text.chars().any(char::is_alphanumeric) {
            out.push(Block::passage(text));
        }
    }
}

/// Ends a table: its header (a first row and its separator) is kept apart, shown above
/// whichever of its rows are kept, and each body row is a passage. A short line alone
/// right before the table, not a sentence (its caption, "Oct - 2026", "Fri"), goes with
/// the header: it says what the table is of, and judged on its own it was dropped,
/// which left a forecast's hours without their day and a calendar without its month.
fn flush_table(rows: &mut Vec<String>, out: &mut Vec<Block>, tables: &mut Vec<String>) {
    if rows.is_empty() {
        return;
    }
    let header_len = if rows.len() > 1 && rows[1].trim_start_matches('|').trim().starts_with("---")
    {
        2
    } else {
        0
    };
    let label = out
        .last()
        .is_some_and(|b| {
            b.level == 0
                && b.table.is_none()
                && !b.text.contains('\n')
                && b.text.chars().count() <= LABEL_CHARS
                && !b.text.ends_with(['.', '!', '?'])
                && !is_list_item(&b.text)
        })
        .then(|| out.pop())
        .flatten();
    let id = tables.len();
    let header = rows[..header_len].join("\n");
    tables.push(match &label {
        Some(label) if header.is_empty() => label.text.clone(),
        Some(label) => format!("{}\n{header}", label.text),
        None => header,
    });
    for (n, row) in rows[header_len..].iter().enumerate() {
        out.push(Block {
            level: 0,
            text: row.clone(),
            table: Some(id),
            // The fragment's target, when it was the label, is the table's first row.
            target: n == 0 && label.as_ref().is_some_and(|l| l.target),
        });
    }
    rows.clear();
}

/// A fenced block (its lines, fences included): a passage whole, blank lines and all,
/// unless it is a long plain-text document set in `<pre>` (an RFC, a man page, a
/// mailing-list post). Kept whole, such a document was one passage cut after its
/// opening, and neither a digest nor the reached check saw past its first page. Its
/// paragraphs, apart at blank lines, are passages of their own then, and its numbered
/// section titles ("4.1.2.  Authorization Response") headings. `target`: the line
/// the fragment mark stood before. A printed page's running head and foot (see
/// `running_lines`) are left out of such a document.
fn fenced_block(
    lines: &[String],
    target: Option<usize>,
    running: &HashSet<String>,
    out: &mut Vec<Block>,
) {
    let inner = lines.get(1..lines.len().saturating_sub(1)).unwrap_or(&[]);
    let long = inner.iter().map(|l| l.chars().count()).sum::<usize>() > PARAGRAPHS_CHARS;
    if !long || !prose_like(inner) {
        let mut block = Block::passage(lines.join("\n"));
        block.target = target.is_some();
        out.push(block);
        return;
    }
    let mut paragraph: Vec<&str> = Vec::new();
    // Whether the target is in the paragraph being gathered (or before it, when the
    // paragraph before it said nothing).
    let mut pending = false;
    // Ends a paragraph, as a heading or a passage; whether it was kept.
    let end = |paragraph: &mut Vec<&str>, is_target: bool, out: &mut Vec<Block>| {
        let page_line = paragraph.len() == 1 && running.contains(&line_shape(paragraph[0]));
        let says = paragraph
            .iter()
            .any(|l| l.chars().any(char::is_alphanumeric));
        let kept = says && !page_line;
        if kept {
            let mut block = match (paragraph.len(), section_level(paragraph[0])) {
                (1, Some(level)) => Block::new(level, paragraph[0].trim().to_owned()),
                _ => Block::passage(dedent(paragraph)),
            };
            block.target = is_target;
            out.push(block);
        }
        paragraph.clear();
        kept
    };
    // `lines[0]` is the opening fence, so the inner line `n` is the passage's `n + 1`.
    for (n, line) in inner.iter().enumerate() {
        if target == Some(n + 1) {
            end(&mut paragraph, false, out);
            pending = true;
        }
        if line.trim().is_empty() {
            if end(&mut paragraph, pending, out) {
                pending = false;
            }
            continue;
        }
        paragraph.push(line);
    }
    end(&mut paragraph, pending, out);
}

/// The running heads and feet of a document printed in pages, one `<pre>` a page (an
/// RFC's "RFC 6749   OAuth 2.0   October 2012" and "Hardt   Standards Track   [Page
/// 26]"), by their shape (`line_shape`): the first or last line of at least three
/// fenced blocks. Kept, they were passages of their own, which a digest spent its
/// budget on, between the halves of a sentence split by a page break.
fn running_lines(markdown: &str) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut fence: Vec<&str> = Vec::new();
    let mut fenced = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            if fenced {
                let filled: Vec<&str> = fence
                    .iter()
                    .copied()
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                if filled.len() > 2 {
                    *counts.entry(line_shape(filled[0])).or_default() += 1;
                    *counts
                        .entry(line_shape(filled[filled.len() - 1]))
                        .or_default() += 1;
                }
                fence.clear();
            }
            fenced = !fenced;
        } else if fenced && line.trim() != FRAGMENT_MARK {
            fence.push(line);
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= 3)
        .map(|(shape, _)| shape)
        .collect()
}

/// A line with its numbers as `#` and its spaces collapsed: a page's running foot
/// alike on every page, whatever its page number.
fn line_shape(line: &str) -> String {
    let mut out = String::new();
    for word in line.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut digits = false;
        for c in word.chars() {
            if c.is_ascii_digit() {
                if !digits {
                    out.push('#');
                }
                digits = true;
            } else {
                digits = false;
                out.push(c);
            }
        }
    }
    out
}

/// Whether lines read as prose rather than code or data: mostly words, several to a
/// line, and hardly any of the symbols code is made of. RFC text has a parenthesis now
/// and then; code has them on most lines, and config files a word or two per line.
fn prose_like(lines: &[String]) -> bool {
    let text = lines.join("\n");
    let shown = text.chars().filter(|c| !c.is_whitespace()).count();
    let symbols = text.chars().filter(|c| "{}[]();=<>".contains(*c)).count();
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let words = tokens.iter().filter(|t| is_word(t)).count();
    let filled = lines.iter().filter(|l| !l.trim().is_empty()).count();
    shown > 0
        && symbols * 40 < shown
        && words * 10 >= tokens.len() * 6
        && tokens.len() >= filled * 6
}

/// A word of prose: letters (and a hyphen or apostrophe), with its punctuation.
fn is_word(token: &str) -> bool {
    let word = token.trim_matches(|c: char| ",.;:!?\"'()".contains(c));
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
}

/// The heading level of a plain-text document's numbered section title, "4.1.2.
/// Authorization Response": one deeper than its number has parts. Not a sentence (it
/// would end in a period) and not a page's running header ("RFC 6749  OAuth 2.0").
fn section_level(line: &str) -> Option<usize> {
    let line = line.trim();
    if line.chars().count() > 80 || line.ends_with('.') {
        return None;
    }
    let (number, title) = line.split_once(char::is_whitespace)?;
    let parts: Vec<&str> = number.trim_end_matches('.').split('.').collect();
    let numbered = parts
        .iter()
        .all(|p| (1..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()));
    let titled = title.trim().chars().next().is_some_and(char::is_uppercase);
    (numbered && titled).then_some((parts.len() + 1).min(6))
}

/// Lines without the indent they all share.
fn dedent(lines: &[&str]) -> String {
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| l.get(indent..).unwrap_or(l.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A markdown list item: "- …", "* …" or "3. …".
fn is_list_item(line: &str) -> bool {
    line.starts_with("- ")
        || line.starts_with("* ")
        || line
            .split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// `[text](url)` becomes `text`, and `[image: …]` goes, unless the image is all there is
/// on the line: then it is the content (a forecast's "Night: heavy rain") and its name
/// stays. Other brackets stay.
pub(super) fn unlink(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut images: Vec<&str> = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let Some(close) = rest.find(']') else {
            break;
        };
        if rest.starts_with("[image:") {
            images.push(rest["[image:".len()..close].trim());
            rest = &rest[close + 1..];
            continue;
        }
        if rest[close + 1..].starts_with('(')
            && let Some(end) = rest[close..].find(')').map(|i| close + i)
        {
            out.push_str(&rest[1..close]);
            rest = &rest[end + 1..];
            continue;
        }
        // Not a link: keep the bracket and go on after it.
        out.push('[');
        rest = &rest[1..];
    }
    out.push_str(rest);
    // Only a list number or bullet besides the images (table rows keep their cells).
    let said = out
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', '-', '*', '>', ' ']);
    if !images.is_empty()
        && !out.trim_start().starts_with('|')
        && !said.chars().any(char::is_alphanumeric)
    {
        let lead = out.trim_end();
        let gap = if lead.is_empty() { "" } else { " " };
        out = format!("{lead}{gap}{}", images.join(", "));
    }
    out.trim_end().to_owned()
}
