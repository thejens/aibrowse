//! Navigation paths: the steps that reached a goal on a site, so the navigator can take
//! them again without deciding each one (docs/design.md §5.5, "learned paths").
//!
//! Like the rest of the knowledge base this keeps structure, not personal data. A goal is
//! the agent's free text ("open my chat with Ada"), so only a hash of it is stored. Steps
//! are element labels (`link "Careers" -> /careers`); a path is stored only if every
//! label is short and structural, never one carrying page text or query strings.

use std::path::Path;

use super::store::Store;

/// Labels longer than this likely carry page content (a headline, a message preview).
const MAX_LABEL: usize = 80;

/// The stored path to `goal` on `site`, if a previous session found one.
pub fn recall_path(db: &Path, site: &str, goal: &str) -> Option<Vec<String>> {
    let store = Store::open(db)
        .inspect_err(|err| tracing::warn!(%err, "knowledge base unavailable"))
        .ok()?;
    store
        .path(site, &goal_key(goal))
        .inspect_err(|err| tracing::warn!(%err, "could not read a learned path"))
        .ok()
        .flatten()
}

/// Remembers that `steps` reached `goal` on `site`. Paths with labels that may carry
/// personal content are not stored.
pub fn learn_path(db: &Path, site: &str, goal: &str, steps: &[String]) {
    if steps.is_empty() || !steps.iter().all(|s| structural(s)) {
        return;
    }
    let saved = Store::open(db)
        .and_then(|mut store| store.save_path(site, &goal_key(goal), steps, super::unix_now()));
    if let Err(err) = saved {
        tracing::warn!(%err, "could not store a learned path");
    }
}

/// Forgets the path to `goal` on `site`: taken again, it didn't reach the goal (the site
/// changed, or the path held a detour that leads nowhere on its own), and a path kept
/// after that fails the same way every time.
pub fn forget_path(db: &Path, site: &str, goal: &str) {
    let deleted = Store::open(db).and_then(|mut store| store.delete_path(site, &goal_key(goal)));
    if let Err(err) = deleted {
        tracing::warn!(%err, "could not forget a learned path");
    }
}

/// Whether a label names structure (a role, a short name, a link path) rather than
/// content: short, and no query string or long digit runs (ids) in it.
fn structural(label: &str) -> bool {
    let digits = label
        .split(|c: char| !c.is_ascii_digit())
        .map(str::len)
        .max()
        .unwrap_or(0);
    label.chars().count() <= MAX_LABEL && !label.contains('?') && digits < 5
}

/// A stable key for a goal: normalized (case, spacing) and hashed with FNV-1a, which,
/// unlike std's hasher, gives the same value in every build.
fn goal_key(goal: &str) -> String {
    let normalized = goal
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip_and_goals_are_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("kb.sqlite");
        let steps = vec![
            "link \"Careers\" -> /careers".to_owned(),
            "link \"Open roles\" -> /jobs".to_owned(),
        ];
        learn_path(&db, "example.com", "the  Careers page", &steps);
        assert_eq!(
            recall_path(&db, "example.com", "the careers page"),
            Some(steps)
        );
        assert_eq!(recall_path(&db, "other.com", "the careers page"), None);
        forget_path(&db, "example.com", "the careers page");
        assert_eq!(recall_path(&db, "example.com", "the careers page"), None);
        let bytes = std::fs::read(&db).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("careers page"),
            "the goal itself is never stored"
        );
    }

    #[test]
    fn content_labels_are_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("kb.sqlite");
        learn_path(
            &db,
            "x.com",
            "goal",
            &["link \"Ada\" -> /messages?id=1".to_owned()],
        );
        learn_path(
            &db,
            "x.com",
            "goal2",
            &["link \"Order 1234567\" -> /orders/1234567".to_owned()],
        );
        assert_eq!(recall_path(&db, "x.com", "goal"), None);
        assert_eq!(recall_path(&db, "x.com", "goal2"), None);
    }
}
