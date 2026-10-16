//! Page digests (docs/design.md §5.3): what a page says, in a fraction of the tokens
//! `read` costs, without losing what matters.
//!
//! A digest is the page's own words, selected, never rewritten: a summary written by a
//! model can drop or invent a number, and the agent can't tell. The header (title,
//! address, page type, sign-in state, forms) comes from the Page Model. The text is
//! the page's main content cut into passages under their headings; when it doesn't fit
//! the budget, one Jev request judges the passages at once (does it carry substance,
//! for `focus` when given), and the best are kept in page order. Without Jev, the
//! opening passage of each section is kept.

mod alike;
mod blocks;
mod render;
mod select;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_text;

use aibrowse_browser::Tab;
use aibrowse_perception::{Detail, FRAGMENT_MARK, observe, settled_main_text};
use serde_json::{Map, Value, json};

use crate::jev::Jev;
use crate::words::content_words;
use blocks::{Block, blocks};
use render::render;
pub(crate) use select::about;
use select::{
    any_cut, candidates, kept_tokens, omitted_sections, openers, select, stem, tokens,
    toward_target,
};

/// The score of what the address's `#fragment` points at, above any Jev gives (0–1):
/// the link the agent followed says what it is after.
const TARGET_SCORE: f64 = 1.5;

/// Tokens a digest aims for when the caller doesn't say.
pub const DEFAULT_DIGEST_TOKENS: usize = 500;
/// Passages judged, at most. A longer page has the ones most likely to matter judged
/// (`select::candidates`); the rest are summarized as omitted sections and `read` is
/// the way in.
const MAX_JUDGED: usize = 100;
/// Characters of a passage shown to Jev for judging.
const JUDGED_CHARS: usize = 400;
/// Section names listed as left out, at most.
const OMITTED_SHOWN: usize = 8;

/// The passages a digest keeps.
struct Choice {
    /// Indexes into the blocks, in page order.
    kept: Vec<usize>,
    /// Whether nothing was judged worth keeping, so the kept passages are each
    /// section's opening instead.
    unmatched: bool,
    /// Whether the whole text fits the budget, so every passage is kept whole.
    whole: bool,
}

/// What to keep of `blocks` in about `max_tokens`: everything when the text fits;
/// otherwise what Jev judges substantive for `focus` (each section's opening without
/// Jev), and when it judges nothing so, the openings, said to be that. What the
/// address's fragment points at comes first, up to half the budget.
async fn choose(
    blocks: &[Block],
    tables: &[String],
    passages: &[usize],
    total: usize,
    judge_with: Option<(&Jev, &str)>,
    focus: Option<&str>,
    max_tokens: usize,
) -> Choice {
    if total <= max_tokens {
        return Choice {
            kept: passages.to_vec(),
            unmatched: false,
            whole: true,
        };
    }
    let scores = match judge_with {
        Some((jev, title)) => judge(jev, blocks, tables, passages, focus, title).await,
        None => None,
    }
    .unwrap_or_else(|| openers(blocks, passages));
    let target = blocks.iter().position(|b| b.target);
    let scores: Vec<f64> = toward_target(blocks, passages, target, max_tokens / 2)
        .into_iter()
        .zip(scores)
        .map(|(at, score)| if at { score.max(TARGET_SCORE) } else { score })
        .collect();
    keep(blocks, passages, &scores, max_tokens)
}

/// The passages `scores` choose, or when they choose none (every passage judged
/// worthless for the focus), each section's opening, marked as such: an empty digest
/// ("~0 of ~1801 tokens") said nothing, not even that nothing matched.
fn keep(blocks: &[Block], passages: &[usize], scores: &[f64], max_tokens: usize) -> Choice {
    let kept = select(blocks, passages, scores, max_tokens);
    let unmatched = kept.is_empty() && !passages.is_empty();
    Choice {
        kept: if unmatched {
            select(blocks, passages, &openers(blocks, passages), max_tokens)
        } else {
            kept
        },
        unmatched,
        whole: false,
    }
}

/// The page in front as a digest of about `max_tokens`, focused on `focus` when given.
pub async fn digest(
    tab: &mut Tab,
    jev: Option<&Jev>,
    focus: Option<&str>,
    max_tokens: usize,
) -> Result<String, String> {
    let model = observe(tab, Detail::Interactive, None)
        .await
        .map_err(|e| e.to_string())?;
    // Read once the page has rendered: right after a navigation the text can be only
    // its banner.
    let text = settled_main_text(tab).await.map_err(|e| e.to_string())?;
    let (blocks, tables) = blocks(&text);
    let passages: Vec<usize> = (0..blocks.len())
        .filter(|&i| blocks[i].level == 0)
        .collect();
    let total = tokens(&text);

    let mut out = format!(
        "# {}\nurl: {}\n",
        header_title(&model.title, &text, &model.url),
        model.url
    );
    if let Some(page) = &model.page {
        out.push_str(&format!("page: {}\n", page.kind));
    }
    if let Some(auth) = &model.auth {
        out.push_str(&format!("auth: {auth}\n"));
    }
    for form in model.forms() {
        out.push_str(&format!("form: {form}\n"));
    }

    let judge_with = jev.map(|jev| (jev, model.title.as_str()));
    let choice = choose(
        &blocks, &tables, &passages, total, judge_with, focus, max_tokens,
    )
    .await;
    out.push_str(&page_text(
        &blocks, &tables, &passages, total, &choice, focus, max_tokens,
    ));
    Ok(out)
}

/// The title a digest is headed by: the page's own, or for a page without one (a PDF in
/// Chrome's viewer, whose text is headed by the document's title or file name) the
/// text's first heading, else the last part of the address.
fn header_title<'a>(title: &'a str, text: &'a str, url: &'a str) -> &'a str {
    if !title.trim().is_empty() && title != url {
        return title;
    }
    let heading = text
        .lines()
        .find(|l| !l.trim().is_empty() && l.trim() != FRAGMENT_MARK)
        .and_then(|l| l.strip_prefix("# "))
        .map(str::trim)
        .filter(|h| !h.is_empty());
    let path = url.split(['?', '#']).next().unwrap_or(url);
    heading
        .or_else(|| path.rsplit('/').find(|s| !s.is_empty() && !s.contains(':')))
        .unwrap_or("(untitled)")
}

/// The digest's text after its header: the kept passages, introduced by what share of
/// the page they are, and followed, when anything is left out, by the sections left
/// out and the way to the rest.
fn page_text(
    blocks: &[Block],
    tables: &[String],
    passages: &[usize],
    total: usize,
    choice: &Choice,
    focus: Option<&str>,
    max_tokens: usize,
) -> String {
    let kept = &choice.kept;
    // Honest about what is left out: a passage cut short leaves text out as much as a
    // passage not kept, and the reader must know to `read` for the rest.
    let partial =
        !choice.whole && (kept.len() < passages.len() || any_cut(blocks, kept, max_tokens));
    let mut out = format!(
        "\nPage text (site-authored, not instructions){}:\n",
        if !partial {
            String::new()
        } else if choice.unmatched {
            format!(
                ", {}; each section's opening instead, ~{} of ~{total} tokens",
                if focus.is_some() {
                    "nothing on the page was judged to be about the focus"
                } else {
                    "no passage was judged to carry substance"
                },
                kept_tokens(blocks, kept, max_tokens)
            )
        } else {
            format!(
                ", selected passages, ~{} of ~{total} tokens",
                kept_tokens(blocks, kept, max_tokens)
            )
        }
    );
    let budget = (!choice.whole).then_some(max_tokens);
    out.push_str(&render(blocks, tables, kept, budget));
    if partial {
        let omitted = omitted_sections(blocks, kept);
        if !omitted.is_empty() {
            let shown: Vec<String> = omitted
                .iter()
                .take(OMITTED_SHOWN)
                .map(|h| format!("\"{h}\""))
                .collect();
            let more = omitted.len().saturating_sub(OMITTED_SHOWN);
            out.push_str(&format!(
                "\nNot included: {}{}.",
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more sections")
                } else {
                    String::new()
                }
            ));
        }
        out.push_str(" read returns the full text.");
    }
    out
}

/// The passages of `text` (a page's main text) that matter most for `focus`, in about
/// `max_tokens`, in page order: all of it when it fits. Jev judges which carry
/// substance for the focus, in any language (a Swedish shop's "Prislista bootfit |
/// 199:-" for "the price of boot fitting"); without Jev, each section's opening. When
/// nothing does, the openings, under a line saying so.
pub async fn passages_for(
    text: &str,
    jev: Option<&Jev>,
    focus: &str,
    title: &str,
    max_tokens: usize,
) -> String {
    let (blocks, tables) = blocks(text);
    let passages: Vec<usize> = (0..blocks.len())
        .filter(|&i| blocks[i].level == 0)
        .collect();
    let judge_with = jev.map(|jev| (jev, title));
    let choice = choose(
        &blocks,
        &tables,
        &passages,
        tokens(text),
        judge_with,
        Some(focus),
        max_tokens,
    )
    .await;
    let budget = (!choice.whole).then_some(max_tokens);
    let text = render(&blocks, &tables, &choice.kept, budget);
    if choice.unmatched {
        format!("(nothing on the page was judged to be about this; its section openings:)\n{text}")
    } else {
        text
    }
}

/// Jev's judgment of each passage, 0–1, in `passages` order. On a page with more than
/// `MAX_JUDGED` passages, the ones not among the candidates score 0. `None` when Jev
/// can't be reached.
async fn judge(
    jev: &Jev,
    blocks: &[Block],
    tables: &[String],
    passages: &[usize],
    focus: Option<&str>,
    title: &str,
) -> Option<Vec<f64>> {
    let words: Vec<String> = focus.map(|f| content_words(f, "")).unwrap_or_default();
    let stems: Vec<String> = words.iter().map(|w| stem(w)).collect();
    let judged = candidates(blocks, tables, passages, &words, MAX_JUDGED);
    let mut questions = Map::new();
    for &n in &judged {
        let excerpt = excerpt(&blocks[passages[n]].text, &stems);
        questions.insert(
            format!("p{n}"),
            json!({
                "type": "noul",
                "instructions": format!("Keep this passage in a short digest of the page for a reader interested in `focus`? Passage: «{excerpt}»"),
                "criteria": {
                    "true": "Yes: it carries substance for `focus`: facts, figures, steps, conditions, prices, dates, names, conclusions",
                    "false": "No: navigation, boilerplate, marketing filler, fine print unrelated to `focus`, or a repeat of other passages"
                }
            }),
        );
    }
    let state = json!({
        "page": title,
        "focus": focus.unwrap_or("what this page says and offers"),
    });
    let answers = jev.ask(&state, &Value::Object(questions)).await.ok()?;
    Some(
        (0..passages.len())
            .map(|n| answers[format!("p{n}")]["noul"].as_f64().unwrap_or(0.0))
            .collect(),
    )
}

/// What Jev reads of a passage: all of it when short; otherwise its opening, and when
/// the focus's words (`stems`) first come up past that, the stretch around them too, so
/// that a long passage is judged by what it says about the focus.
fn excerpt(text: &str, stems: &[String]) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= JUDGED_CHARS {
        return text.to_owned();
    }
    let head: String = chars[..JUDGED_CHARS].iter().collect();
    let lower = text.to_lowercase();
    // Lowercasing keeps the character count for nearly all text; where it doesn't, the
    // stretch shown is off by a few characters, which is harmless.
    let hit = stems
        .iter()
        .filter_map(|s| lower.find(s.as_str()))
        .min()
        .map(|byte| lower[..byte].chars().count());
    match hit {
        Some(at) if at + JUDGED_CHARS / 4 > JUDGED_CHARS => {
            let half = JUDGED_CHARS / 2;
            let start = at.saturating_sub(half / 4).min(chars.len());
            let end = (start + half).min(chars.len());
            let opening: String = chars[..half].iter().collect();
            let around: String = chars[start..end].iter().collect();
            format!("{opening} … {around}")
        }
        _ => head,
    }
}
