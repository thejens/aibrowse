//! Showing the chosen passages under their headings, within a budget.

use super::alike;
use super::blocks::Block;
use super::select::MAX_PASSAGE_SHARE;

/// The kept passages in page order, each under the headings it sits under (a heading
/// is shown once, before the first kept passage below it). With a budget
/// (`max_tokens`), a passage over its share of it is cut at a line, or mid-line, with
/// "…"; without, passages are whole. After the last kept passage of a kind with more
/// left out (see `alike`), how many more there are.
pub(super) fn render(
    blocks: &[Block],
    tables: &[String],
    kept: &[usize],
    max_tokens: Option<usize>,
) -> String {
    let cap_chars = max_tokens.map_or(usize::MAX, |max| max / MAX_PASSAGE_SHARE * 4);
    let more = alike::left_out(&alike::kinds(blocks), kept);
    let more_like = |i: usize| {
        more.get(&i)
            .map_or(String::new(), |n| format!("({n} more like it)\n"))
    };
    let mut out = String::new();
    let mut table_shown: Option<usize> = None;
    // The headings in force, by level, and whether each has been shown.
    let mut headings: Vec<(usize, &str, bool)> = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        if block.level > 0 {
            headings.retain(|(level, ..)| *level < block.level);
            headings.push((block.level, &block.text, false));
            continue;
        }
        if !kept.contains(&i) {
            continue;
        }
        for (level, text, shown) in &mut headings {
            if !*shown {
                out.push_str(&format!("{} {text}\n", "#".repeat(*level)));
                *shown = true;
            }
        }
        match block.table {
            Some(id) => {
                // Rows of one table run together under its header, shown once.
                if table_shown != Some(id) {
                    if !out.ends_with("\n\n") && !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    if !tables[id].is_empty() {
                        out.push_str(&tables[id]);
                        out.push('\n');
                    }
                    table_shown = Some(id);
                }
                out.push_str(&cut(&block.text, cap_chars));
                out.push('\n');
                out.push_str(&more_like(i));
                // The next kept block that isn't a row of this table starts a paragraph.
                let next_kept = kept.iter().find(|&&k| k > i);
                if next_kept.is_none_or(|&k| blocks[k].table != Some(id)) {
                    out.push('\n');
                    table_shown = None;
                }
            }
            None => {
                out.push_str(&cut(&block.text, cap_chars));
                out.push('\n');
                out.push_str(&more_like(i));
                out.push('\n');
            }
        }
    }
    out.trim_end().to_owned() + "\n"
}

fn cut(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars).collect();
    let at_line = head.rfind('\n').filter(|&i| i > max_chars / 2);
    let head = at_line.map_or(head.as_str(), |i| &head[..i]);
    format!("{}…", head.trim_end())
}
