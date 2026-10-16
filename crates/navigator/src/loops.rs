//! Keeping the navigator from going around in circles.
//!
//! Coming back to a page is normal (back to the search results, then the next result),
//! so places alone don't make a loop. What does is repeating *steps*: taking the same
//! step again from the same state, or a sequence of steps that has already been done
//! twice in a row (A → B → A → B). A single step may repeat on a page whose content
//! changes (a "Next page" or "Load more" button), but only a few times in a row.
//!
//! Steps are identified by what they mean, never by element refs: refs are DOM node ids
//! that change on every page load, so a ref-based memory never recognizes a page it has
//! been on before.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use aibrowse_perception::PageModel;

/// The longest step sequence checked for repetition. Longer cycles run into the step
/// budget soon enough.
const MAX_CYCLE: usize = 3;
/// How many times in a row one step may repeat while the page keeps changing.
const MAX_REPEATS: usize = 3;

/// Query parameters that track visits rather than select content; a URL that differs only
/// in these is the same place.
const TRACKING: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "fbclid",
    "gclid",
    "msclkid",
    "ref",
    "ref_src",
    "trk",
    "trackingId",
    "_hsenc",
    "_hsmi",
    "mc_cid",
    "mc_eid",
];

/// Where the navigator is: the URL without fragment or tracking parameters.
pub fn place(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_owned();
    };
    parsed.set_fragment(None);
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !TRACKING.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(kept);
    }
    parsed.to_string()
}

/// What the page shows, independent of element refs and scroll position: its elements'
/// roles, names, values and states. Two visits to an unchanged page fingerprint alike;
/// one whose stepper went from 4 to 6, or whose filter got checked, doesn't, so the same
/// step may be taken again there (a recipe scaled 4 → 6 → 8 portions).
pub fn fingerprint(model: &PageModel) -> u64 {
    let mut hasher = DefaultHasher::new();
    for node in &model.nodes {
        node.role.hash(&mut hasher);
        node.name.hash(&mut hasher);
        node.value.hash(&mut hasher);
        node.states.hash(&mut hasher);
    }
    hasher.finish()
}

/// The steps taken so far.
#[derive(Default)]
pub struct Memory {
    /// (place, fingerprint, label) of every step taken: never taken twice.
    taken: HashSet<(String, u64, String)>,
    /// (place, label) of steps that led nowhere and were gone back on: never taken
    /// again from that place, whatever the page shows by then (it may re-render).
    rejected: Vec<(String, String)>,
    /// (place, label) in order, for spotting repeated sequences.
    history: Vec<(String, String)>,
}

impl Memory {
    /// Whether `label` hasn't been acted on in this exact state yet.
    pub fn allows(&self, place: &str, fingerprint: u64, label: &str) -> bool {
        !self
            .taken
            .contains(&(place.to_owned(), fingerprint, label.to_owned()))
            && !self.rejected.iter().any(|(p, l)| p == place && l == label)
    }

    /// Remembers that `label`, taken from `place`, led nowhere.
    pub fn reject(&mut self, place: &str, label: &str) {
        self.rejected.push((place.to_owned(), label.to_owned()));
    }

    /// The steps from `place` that led nowhere, in the order they were given up on.
    pub fn rejected_at(&self, place: &str) -> Vec<String> {
        self.rejected
            .iter()
            .filter(|(p, _)| p == place)
            .map(|(_, l)| l.clone())
            .collect()
    }

    /// Takes back the record of `label` in this state: the step failed before it did
    /// anything (an overlay that came up late covered it), so it is still to be tried.
    pub fn forget(&mut self, place: &str, fingerprint: u64, label: &str) {
        self.taken
            .remove(&(place.to_owned(), fingerprint, label.to_owned()));
        if self
            .history
            .last()
            .is_some_and(|(p, l)| p == place && l == label)
        {
            self.history.pop();
        }
    }

    pub fn record(&mut self, place: &str, fingerprint: u64, label: &str) {
        self.taken
            .insert((place.to_owned(), fingerprint, label.to_owned()));
        self.history.push((place.to_owned(), label.to_owned()));
    }

    /// The repeating steps, if the latest ones go around in a circle.
    pub fn looping(&self) -> Option<Vec<String>> {
        let h = &self.history;
        let describe = |steps: &[(String, String)]| {
            steps
                .iter()
                .map(|(place, label)| format!("{label} on {place}"))
                .collect()
        };
        // One step again and again (the page changing each time, or it would have been
        // refused): fine for paging through results, but not without end.
        if h.len() >= MAX_REPEATS
            && h[h.len() - MAX_REPEATS..]
                .iter()
                .all(|s| *s == h[h.len() - 1])
        {
            return Some(describe(&h[h.len() - 1..]));
        }
        // A sequence of steps done twice in a row.
        (2..=MAX_CYCLE)
            .find(|&n| h.len() >= 2 * n && h[h.len() - n..] == h[h.len() - 2 * n..h.len() - n])
            .map(|n| describe(&h[h.len() - n..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_gone_back_on_is_never_taken_again_from_there() {
        let mut memory = Memory::default();
        memory.record("https://x.com/", 1, "link \"A\"");
        memory.reject("https://x.com/", "link \"A\"");
        // The page re-rendered (another fingerprint): still refused there, fine elsewhere.
        assert!(!memory.allows("https://x.com/", 2, "link \"A\""));
        assert!(memory.allows("https://x.com/b", 2, "link \"A\""));
        assert_eq!(memory.rejected_at("https://x.com/"), ["link \"A\""]);
    }

    #[test]
    fn places_ignore_fragments_and_tracking() {
        assert_eq!(
            place("https://x.com/a?utm_source=y&page=2#top"),
            "https://x.com/a?page=2"
        );
        assert_eq!(place("https://x.com/a?fbclid=1"), "https://x.com/a");
    }

    #[test]
    fn coming_back_is_fine_repeating_is_not() {
        let mut m = Memory::default();
        // Search results, open a result, back, open another: no loop.
        m.record("results", 1, "result A");
        m.record("a", 2, "back");
        assert!(m.allows("results", 1, "result B"));
        m.record("results", 1, "result B");
        assert_eq!(m.looping(), None);
        // The same step from the same state is refused.
        assert!(!m.allows("results", 1, "result A"));
    }

    #[test]
    fn cycles_are_caught() {
        let mut m = Memory::default();
        for fingerprint in 0..2 {
            m.record("a", fingerprint, "to b");
            m.record("b", fingerprint, "to a");
        }
        assert_eq!(
            m.looping(),
            Some(vec!["to b on a".to_owned(), "to a on b".to_owned()])
        );
    }

    #[test]
    fn paging_may_repeat_a_few_times() {
        let mut m = Memory::default();
        m.record("list", 1, "Next page");
        m.record("list", 2, "Next page");
        assert_eq!(m.looping(), None);
        m.record("list", 3, "Next page");
        assert!(m.looping().is_some());
    }
}
