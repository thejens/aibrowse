//! Passages alike but for their numbers: a forecast's sunrise line for each of ten
//! days, a converter's twenty "N EUR = M SEK" rows. Within a budget, a few of them say
//! what the rest would, and the rest are counted instead of shown.

use std::collections::HashMap;

use super::blocks::Block;

/// Passages of one kind a budgeted digest keeps, at most.
pub(super) const ALIKE_KEPT: usize = 3;
/// Passages of one kind there must be for any to be left out: fewer cost little.
const ALIKE_MIN: usize = 5;

/// Per block, the kind of passage it is (an id shared by passages alike), for
/// passages of a kind there are at least `ALIKE_MIN` of. Alike are passages that read
/// the same with their numbers taken out, in one section (or, for table rows, one
/// table): under different headings the same words can mean different things.
pub(super) fn kinds(blocks: &[Block]) -> Vec<Option<usize>> {
    let mut section = 0;
    let keys: Vec<Option<(usize, String)>> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if b.level > 0 {
                section = i;
                return None;
            }
            // Table ids and section block indexes are told apart by the flag.
            let scope = b.table.map_or(section * 2, |t| t * 2 + 1);
            shape(&b.text).map(|s| (scope, s))
        })
        .collect();
    let mut counts: HashMap<&(usize, String), usize> = HashMap::new();
    for key in keys.iter().flatten() {
        *counts.entry(key).or_default() += 1;
    }
    let mut ids: HashMap<&(usize, String), usize> = HashMap::new();
    keys.iter()
        .map(|key| {
            let key = key.as_ref()?;
            if counts[key] < ALIKE_MIN {
                return None;
            }
            let next = ids.len();
            Some(*ids.entry(key).or_insert(next))
        })
        .collect()
}

/// A passage's words with each number as `#`, lowercased: `None` for one without a
/// number (nothing told apart by numbers) or without a word (a row of figures, every
/// one of which may be what is asked for).
fn shape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let (mut numbers, mut letters) = (false, false);
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            numbers = true;
            // A number's own separators ("07:46", "11.158", "1,435,749") stay in it.
            while chars
                .peek()
                .is_some_and(|n| n.is_ascii_digit() || matches!(n, '.' | ',' | ':'))
            {
                chars.next();
            }
            out.push('#');
        } else {
            letters |= c.is_alphabetic();
            out.extend(c.to_lowercase());
        }
    }
    (numbers && letters).then_some(out)
}

/// Whether to leave passage `i` out because `ALIKE_KEPT` of its kind are kept already.
pub(super) fn crowded(kinds: &[Option<usize>], kept: &HashMap<usize, usize>, i: usize) -> bool {
    kinds[i].is_some_and(|k| kept.get(&k).copied().unwrap_or(0) >= ALIKE_KEPT)
}

/// For each kind with passages left out of `kept`: the last kept passage of it, and how
/// many of its kind are left out, so that "(N more like it)" follows that passage.
pub(super) fn left_out(kinds: &[Option<usize>], kept: &[usize]) -> HashMap<usize, usize> {
    let mut total: HashMap<usize, usize> = HashMap::new();
    for kind in kinds.iter().flatten() {
        *total.entry(*kind).or_default() += 1;
    }
    let mut last: HashMap<usize, (usize, usize)> = HashMap::new();
    for &i in kept {
        if let Some(kind) = kinds[i] {
            let entry = last.entry(kind).or_insert((i, 0));
            entry.0 = entry.0.max(i);
            entry.1 += 1;
        }
    }
    last.into_iter()
        .filter(|(kind, (_, shown))| total[kind] > *shown)
        .map(|(kind, (at, shown))| (at, total[&kind] - shown))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::shape;

    #[test]
    fn shapes_take_numbers_out() {
        assert_eq!(
            shape("Solen går upp klockan 07:46 och ned klockan 18:08"),
            shape("Solen går upp klockan 08:07 och ned klockan 17:45")
        );
        assert_eq!(
            shape("| 5 EUR | 55.97 SEK |"),
            shape("| 1,000 EUR | 11,193.80 SEK |")
        );
        assert_ne!(
            shape("Avgår 10:12 Stockholm"),
            shape("Avgår 10:12 Göteborg")
        );
        assert_eq!(shape("| 1 | 2.5 | 3 |"), None);
        assert_eq!(shape("No numbers here"), None);
    }
}
