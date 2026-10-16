//! Outlines as changes. After an action on the same page, most of the outline is what
//! the agent already has; refs stay valid while their elements exist, so only the lines
//! that appeared or went away are news. Sending just those keeps every step cheap; when
//! the page changed a lot (or is a new document), the full outline is clearer.

use aibrowse_perception::Detail;

/// Outlines shorter than this are sent whole: a diff saves nothing worth the indirection.
const MIN_LINES: usize = 15;
/// Past this share of changed lines, the full outline reads better than a diff.
const MAX_CHANGED: f64 = 0.4;
/// Line diffs are quadratic; beyond this many lines a page is sent whole.
const MAX_DIFF_LINES: usize = 1500;

/// The last outline the agent was given, per tab and document.
pub struct Seen {
    target: String,
    url: String,
    detail: Detail,
    lines: Vec<String>,
}

impl Seen {
    pub fn new(target: &str, url: &str, detail: Detail, outline: &str) -> Self {
        Self {
            target: target.to_owned(),
            url: url.to_owned(),
            detail,
            lines: outline.lines().map(str::to_owned).collect(),
        }
    }

    /// The changes from this outline to `outline` of the same tab, document and detail,
    /// or `None` when the full outline should be sent instead.
    pub fn changes(
        &self,
        target: &str,
        url: &str,
        detail: Detail,
        outline: &str,
    ) -> Option<String> {
        if self.target != target || self.url != url || self.detail != detail {
            return None;
        }
        let new: Vec<&str> = outline.lines().collect();
        if new.len() < MIN_LINES || new.len() > MAX_DIFF_LINES || self.lines.len() > MAX_DIFF_LINES
        {
            return None;
        }
        let old: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        let ops = diff(&old, &new);
        let changed = ops.iter().filter(|op| !matches!(op, Op::Same(_))).count();
        if changed as f64 > MAX_CHANGED * new.len() as f64 {
            return None;
        }
        if changed == 0 {
            return Some("No changes since the last outline: the page looks the same.".into());
        }
        let mut out = format!(
            "Changes since the last outline ({changed} lines; {} unchanged, refs there still valid):",
            new.len() - ops.iter().filter(|op| matches!(op, Op::Add(_))).count()
        );
        for op in ops {
            match op {
                Op::Add(line) => out.push_str(&format!("\n+ {line}")),
                Op::Remove(line) => out.push_str(&format!("\n- {line}")),
                Op::Same(_) => {}
            }
        }
        Some(out)
    }
}

#[derive(Debug, PartialEq)]
enum Op<'a> {
    Same(&'a str),
    Add(&'a str),
    Remove(&'a str),
}

/// A line diff by longest common subsequence, in the new outline's order.
fn diff<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Op<'a>> {
    let (n, m) = (old.len(), new.len());
    // lcs[i][j]: length of the LCS of old[i..] and new[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut ops = Vec::with_capacity(n.max(m));
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(Op::Same(new[j]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(Op::Remove(old[i]));
            i += 1;
        } else {
            ops.push(Op::Add(new[j]));
            j += 1;
        }
    }
    ops.extend(old[i..].iter().map(|l| Op::Remove(l)));
    ops.extend(new[j..].iter().map(|l| Op::Add(l)));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(extra: &[&str]) -> String {
        let mut lines: Vec<String> = (0..20)
            .map(|i| format!("- button \"B{i}\" [e{i}]"))
            .collect();
        lines.extend(extra.iter().map(|l| (*l).to_owned()));
        lines.join("\n")
    }

    #[test]
    fn small_changes_come_as_a_diff() {
        let seen = Seen::new("t", "u", Detail::Interactive, &outline(&[]));
        let after = outline(&["- dialog \"Saved\"", "  - button \"OK\" [e99]"]);
        let changes = seen.changes("t", "u", Detail::Interactive, &after).unwrap();
        assert!(
            changes.starts_with("Changes since the last outline (2 lines"),
            "{changes}"
        );
        assert!(
            changes.ends_with("\n+ - dialog \"Saved\"\n+   - button \"OK\" [e99]"),
            "{changes}"
        );
        assert!(
            seen.changes("t", "u", Detail::Interactive, &outline(&[]))
                .unwrap()
                .starts_with("No changes")
        );
    }

    #[test]
    fn new_documents_and_big_changes_come_whole() {
        let seen = Seen::new("t", "u", Detail::Interactive, &outline(&[]));
        assert!(
            seen.changes("t", "other", Detail::Interactive, &outline(&[]))
                .is_none()
        );
        assert!(
            seen.changes("t2", "u", Detail::Interactive, &outline(&[]))
                .is_none()
        );
        assert!(
            seen.changes("t", "u", Detail::Full, &outline(&[]))
                .is_none()
        );
        let rewritten: String = (0..20).map(|i| format!("- link \"L{i}\"\n")).collect();
        assert!(
            seen.changes("t", "u", Detail::Interactive, &rewritten)
                .is_none()
        );
    }

    #[test]
    fn diff_orders_by_the_new_outline() {
        let ops = diff(&["a", "b", "c"], &["a", "x", "c", "y"]);
        assert_eq!(
            ops,
            vec![
                Op::Same("a"),
                Op::Remove("b"),
                Op::Add("x"),
                Op::Same("c"),
                Op::Add("y")
            ]
        );
    }
}
