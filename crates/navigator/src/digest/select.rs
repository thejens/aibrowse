//! Choosing a page's passages within a budget, and showing them under their headings.

use std::collections::HashMap;

use aibrowse_perception::without_mark;

use super::alike::{self, crowded};
use super::blocks::{Block, blocks};
use super::render::render;

/// A passage chosen for the digest takes at most this share of the budget, so that one
/// long block can't crowd out the rest.
pub(super) const MAX_PASSAGE_SHARE: usize = 3;

/// A rough token count: about four characters per token.
pub(super) fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// The page's text for checking it against a goal, in about `max_chars`: all of it when
/// it fits; otherwise its opening, then the passages that use the goal's `words` (a
/// passage under a heading that uses them counts too), each under its headings, in
/// page order. A crate's license or a plan's price sits far down a long page, past any
/// opening a check can afford to read. Chosen by words rather than by Jev: the check
/// itself is a Jev request, and a second one before it would double its latency. The
/// words only choose what Jev reads; Jev still judges it.
///
/// When the address points into the page (`#section-4.1.2`), what it points at takes
/// the opening's place, after only the page's first lines: a link to a section of a
/// long RFC is a link to that section, and its opening is a table of contents.
pub(crate) fn about(markdown: &str, words: &[String], max_chars: usize) -> String {
    if markdown.chars().count() <= max_chars {
        return without_mark(markdown);
    }
    let (blocks, tables) = blocks(markdown);
    let passages: Vec<usize> = (0..blocks.len())
        .filter(|&i| blocks[i].level == 0)
        .collect();
    let stems: Vec<String> = words.iter().map(|w| stem(w)).collect();
    let max_tokens = max_chars / 4;
    // The opening, up to half the budget, comes first, in page order; with a target,
    // the page's first lines (an eighth) and what the address points at (half).
    let target = blocks.iter().position(|b| b.target);
    let at_target = toward_target(&blocks, &passages, target, max_tokens / 2);
    let mut opening = 0;
    let first = if target.is_some() {
        max_tokens / 8
    } else {
        max_tokens / 2
    };
    let in_opening: Vec<bool> = passages
        .iter()
        .zip(&at_target)
        .map(|(&i, &at)| {
            opening += tokens(&blocks[i].text);
            at || opening <= first
        })
        .collect();
    let uses = word_uses(&contexts(&blocks, &tables), &stems);
    // A word the page uses everywhere says little about any one passage ("open" on a
    // careers page full of "open source"); one it uses once points right at it ("Job
    // Postings"). Each word counts 1 / the number of passages using it.
    let spread: Vec<usize> = (0..stems.len())
        .map(|w| {
            uses.iter()
                .zip(&in_opening)
                .filter(|(u, opening)| !**opening && u[w])
                .count()
        })
        .collect();
    let scores: Vec<f64> = uses
        .iter()
        .enumerate()
        .map(|(p, u)| {
            if in_opening[p] {
                OPENING - p as f64 / passages.len() as f64
            } else {
                rarity(u, &spread)
            }
        })
        .collect();
    let mut kept = select(&blocks, &passages, &scores, max_tokens);
    fill(&blocks, &passages, &mut kept, max_tokens);
    render(&blocks, &tables, &kept, Some(max_tokens))
}

/// A passage as the focus's words are looked for in it: its text (a table row with its
/// table's header and the passage introducing the table: a row rarely repeats the words
/// it is about, "6.12 | 2024-11-17 | Dec, 2028" is a longterm kernel's end of life), the
/// headings it sits under, and its section (the block index of the nearest heading).
/// All lowercased.
struct Context {
    text: String,
    headings: Vec<String>,
    section: usize,
}

fn contexts(blocks: &[Block], tables: &[String]) -> Vec<Context> {
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut section = 0;
    // The passage before the current table, which says what the table is.
    let mut introduction = String::new();
    let mut out = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        if block.level > 0 {
            headings.retain(|(level, _)| *level < block.level);
            headings.push((block.level, block.text.to_lowercase()));
            introduction.clear();
            section = i;
            continue;
        }
        let text = match block.table {
            Some(table) => format!("{}\n{}\n{}", block.text, tables[table], introduction),
            None => {
                introduction.clone_from(&block.text);
                block.text.clone()
            }
        }
        .to_lowercase();
        out.push(Context {
            text,
            headings: headings.iter().map(|(_, h)| h.clone()).collect(),
            section,
        });
    }
    out
}

/// Per passage, in page order: which of the words (`stems`) it uses. A passage under a
/// heading that uses a word uses it too, and so does a table row whose header or
/// introduction does.
fn word_uses(contexts: &[Context], stems: &[String]) -> Vec<Vec<bool>> {
    contexts
        .iter()
        .map(|c| {
            stems
                .iter()
                .map(|stem| {
                    c.text.contains(stem.as_str())
                        || c.headings.iter().any(|h| h.contains(stem.as_str()))
                })
                .collect()
        })
        .collect()
}

/// How closely a passage matches the focus, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Match {
    None,
    /// A word's stem, possibly inside a longer word ("fina" in "Semi-finals").
    Stem,
    /// A word itself, standing alone ("final" in "the final was played").
    Word,
    /// Under a heading that is the focus: most of its words are the focus's ("Final"),
    /// rather than containing one ("Quarter-finals").
    Heading,
}

/// How `context` matches the focus's `words` (lowercased) and their `stems`.
fn match_of(context: &Context, words: &[String], stems: &[String]) -> Match {
    let is_focus = |heading: &str| {
        let parts: Vec<&str> = heading
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        let focus = parts
            .iter()
            .filter(|p| words.iter().any(|w| w == *p))
            .count();
        focus > 0 && focus * 2 >= parts.len()
    };
    if context.headings.iter().any(|h| is_focus(h)) {
        return Match::Heading;
    }
    let texts = || std::iter::once(&context.text).chain(&context.headings);
    if words.iter().any(|w| texts().any(|t| whole_word(t, w))) {
        return Match::Word;
    }
    if stems
        .iter()
        .any(|s| texts().any(|t| t.contains(s.as_str())))
    {
        return Match::Stem;
    }
    Match::None
}

/// Whether `word` stands alone somewhere in `text`, not inside a longer word.
fn whole_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let alone = |c: Option<char>| !c.is_some_and(char::is_alphanumeric);
    text.match_indices(word).any(|(at, _)| {
        alone(text[..at].chars().next_back()) && alone(text[at + word.len()..].chars().next())
    })
}

/// A passage's score by the words it uses: each counts 1 / the number of passages using
/// it (`spread`).
fn rarity(uses: &[bool], spread: &[usize]) -> f64 {
    (0..uses.len())
        .filter(|&w| uses[w])
        .map(|w| 1.0 / spread[w] as f64)
        .sum()
}

/// Which passages (positions in `passages`) Jev judges when a page has more than it can
/// judge at once (`max`): the ones under a heading that is the focus (`words`), then
/// the ones using its words whole, then their stems, then each section's opening
/// passage, then the page's first passages. Within each of these, sections take turns,
/// so that one section of many table rows can't use up the judging: a Wikipedia
/// tournament's "Final", asked for by name, lost to 386 passages whose headings say
/// "Quarter-finals" and "Semi-finals", taken in page order.
pub(super) fn candidates(
    blocks: &[Block],
    tables: &[String],
    passages: &[usize],
    words: &[String],
    max: usize,
) -> Vec<usize> {
    if passages.len() <= max {
        return (0..passages.len()).collect();
    }
    let stems: Vec<String> = words.iter().map(|w| stem(w)).collect();
    let contexts = contexts(blocks, tables);
    let uses = word_uses(&contexts, &stems);
    let spread: Vec<usize> = (0..stems.len())
        .map(|w| uses.iter().filter(|u| u[w]).count())
        .collect();
    let openers = openers(blocks, passages);
    // Each passage's match, and within it a score: how rare the words it uses are,
    // whether it opens its section, whether it is among the page's first.
    let scored: Vec<(Match, f64)> = (0..passages.len())
        .map(|p| {
            let rare = rarity(&uses[p], &spread);
            let opens = if openers[p] >= 1.0 { 1.0 } else { 0.0 };
            let early = if p < max / 4 { 0.5 } else { 0.0 };
            (match_of(&contexts[p], words, &stems), rare + opens + early)
        })
        .collect();
    let mut order: Vec<usize> = (0..passages.len()).collect();
    // Stable: equal scores keep page order.
    order.sort_by(|&a, &b| {
        scored[b]
            .0
            .cmp(&scored[a].0)
            .then(scored[b].1.total_cmp(&scored[a].1))
    });
    // Each passage's turn: its place among the passages of its section that match as
    // closely, best first.
    let mut taken: HashMap<(Match, usize), usize> = HashMap::new();
    let mut turn = vec![0; passages.len()];
    for &p in &order {
        let n = taken.entry((scored[p].0, contexts[p].section)).or_default();
        turn[p] = *n;
        *n += 1;
    }
    // Stable again: by match, then by turn, the order above within a turn.
    order.sort_by(|&a, &b| scored[b].0.cmp(&scored[a].0).then(turn[a].cmp(&turn[b])));
    let mut chosen: Vec<usize> = order.into_iter().take(max).collect();
    chosen.sort_unstable();
    chosen
}

/// Which passages (in `passages` order) are what the address's fragment points at:
/// from the `target` block on, up to `max_tokens`.
pub(super) fn toward_target(
    blocks: &[Block],
    passages: &[usize],
    target: Option<usize>,
    max_tokens: usize,
) -> Vec<bool> {
    let Some(target) = target else {
        return vec![false; passages.len()];
    };
    let mut used = 0;
    passages
        .iter()
        .map(|&i| {
            if i < target {
                return false;
            }
            used += tokens(&blocks[i].text);
            // The first passage there counts even when it is long on its own.
            used <= max_tokens || used == tokens(&blocks[i].text)
        })
        .collect()
}

/// Gives the budget the chosen passages leave to the others, in page order. Where few
/// passages use the goal's words, the page is often in another language than the goal
/// (SJ's departures say "Avgår", the goal "departure"): without this the check read
/// only the opening, a calendar, and judged the list of trains not to be one.
fn fill(blocks: &[Block], passages: &[usize], kept: &mut Vec<usize>, max_tokens: usize) {
    let cap = max_tokens / MAX_PASSAGE_SHARE;
    let mut used = kept_tokens(blocks, kept, max_tokens);
    let kinds = alike::kinds(blocks);
    let mut of_kind = kinds_kept(&kinds, kept);
    for &i in passages {
        if kept.contains(&i) || crowded(&kinds, &of_kind, i) {
            continue;
        }
        let cost = tokens(&blocks[i].text).min(cap);
        if used + cost > max_tokens {
            break;
        }
        used += cost;
        kept.push(i);
        if let Some(kind) = kinds[i] {
            *of_kind.entry(kind).or_default() += 1;
        }
    }
    kept.sort_unstable();
}

/// How many passages of each kind (see `alike`) are among `kept`.
fn kinds_kept(kinds: &[Option<usize>], kept: &[usize]) -> HashMap<usize, usize> {
    let mut out = HashMap::new();
    for kind in kept.iter().filter_map(|&i| kinds[i]) {
        *out.entry(kind).or_default() += 1;
    }
    out
}

/// Above any score from goal words a passage can have.
const OPENING: f64 = 1000.0;

/// A word without its ending, so that "license" finds "licensed", "reporting" finds
/// "report" and "jobs" finds "Job Postings" (Shopify's open roles, missed when "jobs"
/// stayed whole): three letters stay whole, four lose one, longer ones up to two. A
/// stem that also matches unrelated words ("car" in "careers") only costs a passage
/// of the budget; Jev still judges what it reads.
pub(super) fn stem(word: &str) -> String {
    let n = word.chars().count();
    let keep = match n {
        0..=3 => n,
        4 => 3,
        _ => (n - 2).max(4),
    };
    word.chars().take(keep).collect()
}

/// Without Jev: the opening passage of each section matters most.
pub(super) fn openers(blocks: &[Block], passages: &[usize]) -> Vec<f64> {
    passages
        .iter()
        .map(|&i| {
            let opens_section = i == 0 || blocks[i - 1].level > 0;
            if opens_section { 1.0 } else { 0.3 }
        })
        .collect()
}

/// The passages to keep, best first until the budget is used, returned in page order.
/// Of passages alike but for their numbers, only the best few (`alike`).
pub(super) fn select(
    blocks: &[Block],
    passages: &[usize],
    scores: &[f64],
    max_tokens: usize,
) -> Vec<usize> {
    let mut ranked: Vec<(usize, f64)> = passages
        .iter()
        .copied()
        .zip(scores.iter().copied())
        .collect();
    // Stable: equal scores keep page order.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let cap = max_tokens / MAX_PASSAGE_SHARE;
    let kinds = alike::kinds(blocks);
    let mut of_kind: HashMap<usize, usize> = HashMap::new();
    let mut used = 0;
    let mut kept = Vec::new();
    for (i, score) in ranked {
        if score < 0.2 {
            break;
        }
        let cost = tokens(&blocks[i].text).min(cap);
        if used + cost > max_tokens || crowded(&kinds, &of_kind, i) {
            continue;
        }
        used += cost;
        kept.push(i);
        if let Some(kind) = kinds[i] {
            *of_kind.entry(kind).or_default() += 1;
        }
    }
    kept.sort_unstable();
    kept
}

pub(super) fn kept_tokens(blocks: &[Block], kept: &[usize], max_tokens: usize) -> usize {
    let cap = max_tokens / MAX_PASSAGE_SHARE;
    kept.iter().map(|&i| tokens(&blocks[i].text).min(cap)).sum()
}

/// Whether a kept passage is longer than its share of the budget, and so is cut.
pub(super) fn any_cut(blocks: &[Block], kept: &[usize], max_tokens: usize) -> bool {
    let cap = max_tokens / MAX_PASSAGE_SHARE;
    kept.iter().any(|&i| tokens(&blocks[i].text) > cap)
}

/// Headings with nothing kept under them (at any depth), in page order.
pub(super) fn omitted_sections(blocks: &[Block], kept: &[usize]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        if block.level == 0 {
            continue;
        }
        // The section runs to the next heading of the same or a higher level.
        let end = blocks[i + 1..]
            .iter()
            .position(|b| b.level > 0 && b.level <= block.level)
            .map_or(blocks.len(), |n| i + 1 + n);
        let has_text = (i + 1..end).any(|j| blocks[j].level == 0);
        if has_text && !(i + 1..end).any(|j| kept.contains(&j)) {
            out.push(block.text.clone());
        }
    }
    out
}
