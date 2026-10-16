//! Golden tests: recorded CDP captures in `fixtures/pages/` (made with
//! `aibrowse fixture <url> <name>`) rendered through the Page Model. Review changes with
//! `cargo insta review`.

use std::collections::HashSet;
use std::path::PathBuf;

use aibrowse_perception::{Detail, NodeKind, Position, RawObservation, fuse};

fn fixtures() -> Vec<(String, RawObservation)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pages");
    let mut out: Vec<(String, RawObservation)> = std::fs::read_dir(&dir)
        .expect("fixtures/pages exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            let raw = serde_json::from_slice(&std::fs::read(&p).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            (name, raw)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn interactive_outline() {
    for (name, raw) in fixtures() {
        insta::assert_snapshot!(
            format!("{name}-interactive"),
            fuse(&raw, Detail::Interactive, None).render()
        );
    }
}

#[test]
fn full_outline() {
    for (name, raw) in fixtures() {
        insta::assert_snapshot!(
            format!("{name}-full"),
            fuse(&raw, Detail::Full, None).render()
        );
    }
}

/// What each fixture is classified as, at a glance: a regression in one rule shows up
/// as one changed line.
#[test]
fn page_types() {
    let summary: Vec<String> = fixtures()
        .into_iter()
        .map(|(name, raw)| {
            let model = fuse(&raw, Detail::Interactive, None);
            let page = model.page.map_or("-".to_owned(), |p| {
                format!("{} ({})", p.kind, p.evidence.join(", "))
            });
            let auth = model.auth.unwrap_or_else(|| "-".into());
            format!("{name}: page: {page}; auth: {auth}")
        })
        .collect();
    insta::assert_snapshot!(summary.join("\n"));
}

/// Set-of-marks labels every visible actionable element: all the outline lists, plus
/// the repeated links and summarized parts it leaves out (still refs `act` accepts).
#[test]
fn marks_cover_what_the_outline_leaves_out() {
    let mut unlisted = 0;
    for (name, raw) in fixtures() {
        let model = fuse(&raw, Detail::Interactive, None);
        let marked: HashSet<String> = model
            .marks()
            .iter()
            .map(|m| m.reference.to_string())
            .collect();
        let listed: HashSet<String> = model
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Actionable && n.position == Some(Position::Visible))
            .filter(|n| n.rect.is_some())
            .filter_map(|n| n.reference.map(|r| r.to_string()))
            .collect();
        assert!(
            listed.is_subset(&marked),
            "{name}: outlined but unmarked: {:?}",
            listed.difference(&marked).collect::<Vec<_>>()
        );
        unlisted += marked.difference(&listed).count();
    }
    assert!(
        unlisted > 0,
        "no fixture has visible elements the outline merges"
    );
}

#[test]
fn region_outlines() {
    let all = fixtures();
    let raw = |wanted: &str| {
        &all.iter()
            .find(|(name, _)| name == wanted)
            .expect("fixture exists")
            .1
    };
    for (name, fixture, region) in [
        ("signup-footer", "synthetic-signup", "footer"),
        ("signup-help", "synthetic-signup", "help"),
        ("signup-form", "synthetic-signup", "form"),
        ("signup-missing", "synthetic-signup", "dialog"),
        (
            "wikipedia-contents",
            "wikipedia-article",
            "navigation \"Contents\"",
        ),
    ] {
        insta::assert_snapshot!(
            format!("region-{name}"),
            fuse(raw(fixture), Detail::Interactive, Some(region)).render()
        );
    }
}
