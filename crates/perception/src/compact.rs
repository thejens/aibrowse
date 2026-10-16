//! Keeping outlines small. Agents pay for every line in tokens, and Jev decides worse
//! with a pile of irrelevant options, so the default outline is focused on what the
//! user would see now, without repeats, and summarizes the rest.

use std::collections::HashMap;

use crate::model::{Node, NodeKind, Position};

/// Upper bound on actionable nodes, to keep even "all"/"full" outlines within reason.
pub(crate) const MAX_ACTIONABLE: usize = 300;
/// How far outside the viewport, in screen heights, the default outline still lists
/// elements individually; anything further away is summarized.
const NEAR: f64 = 1.0;
/// Off-screen elements listed individually, at most. Link-dense pages (news front pages,
/// wikis) put a hundred links within a screen of the viewport; past this many, the rest
/// is summarized like far content, and scrolling lists it in full.
const NEAR_OFFSCREEN_BUDGET: usize = 40;
/// A summary of this few elements costs more than listing them.
const MIN_SUMMARY: usize = 3;

pub(crate) fn count_actionable(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Actionable)
        .count()
}

/// Whether the node at `i` has children in the outline (deeper nodes right after it).
fn has_children(nodes: &[Node], i: usize) -> bool {
    nodes
        .get(i + 1)
        .is_some_and(|next| next.depth > nodes[i].depth)
}

/// Accessibility shortcuts for keyboard users ("Skip to main content"); the agent
/// addresses elements directly and never needs them.
fn is_skip_link(node: &Node) -> bool {
    let name = node.name.to_ascii_lowercase();
    name.starts_with("skip to")
        || name.starts_with("jump to")
        || name == "close jump menu"
        || name == "keyboard shortcuts"
}

/// Removes skip links, and links repeated within a section: social sites link the same
/// profile or post several times per card (avatar, name, "View profile"). One entry per
/// destination is kept, under the most descriptive name.
pub(crate) fn tidy(nodes: &mut Vec<Node>) {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    // (depth, scope id) of open sections, innermost last.
    let mut scopes: Vec<(usize, usize)> = Vec::new();
    let mut seen: HashMap<(usize, String), usize> = HashMap::new();
    let input = std::mem::take(nodes);
    for (i, node) in input.iter().enumerate() {
        while scopes.last().is_some_and(|(depth, _)| *depth >= node.depth) {
            scopes.pop();
        }
        let leaf = !has_children(&input, i);
        if node.kind == NodeKind::Actionable && leaf {
            if is_skip_link(node)
                || (node.name.is_empty() && node.href.is_none() && node.role == "link")
            {
                continue;
            }
            // Links without a destination of their own (an empty `href`, a bare `#`,
            // `javascript:`) are each what their script makes them, not repeats of one
            // place: REI's filter options ("add filter: $50.00 to $99.99") all have
            // `href=""`, and merged as one destination, all but one disappeared.
            if let Some(href) = node.href.as_ref().filter(|h| has_destination(h)) {
                let scope = scopes.last().map_or(0, |(_, id)| *id);
                if let Some(&kept) = seen.get(&(scope, href.clone())) {
                    let kept: &mut Node = &mut out[kept];
                    if descriptiveness(&node.name) > descriptiveness(&kept.name) {
                        kept.name = node.name.clone();
                    }
                    continue;
                }
                seen.insert((scope, href.clone()), out.len());
            }
        }
        if node.kind == NodeKind::Structure {
            scopes.push((node.depth, i + 1));
        }
        out.push(node.clone());
    }
    *nodes = out;
}

/// Whether a link's address names a place to go, rather than leaving it to a script.
fn has_destination(href: &str) -> bool {
    let href = href.trim();
    !href.is_empty() && href != "#" && !href.to_ascii_lowercase().starts_with("javascript:")
}

/// How well a name describes where a link goes: words over a bare address (a search
/// result's title over the URL printed under it, though the URL is longer), then the
/// longer name.
fn descriptiveness(name: &str) -> (bool, usize) {
    let lower = name.trim().to_ascii_lowercase();
    let bare_address = lower.starts_with("http://")
        || lower.starts_with("https://")
        || (lower.starts_with("www.") && !lower.contains(' '));
    (!bare_address, name.chars().count())
}

/// What a run of summarized nodes held.
#[derive(Default)]
struct Run {
    /// The summarized nodes, listed instead when there are too few to summarize.
    nodes: Vec<Node>,
    depth: usize,
    actionable: usize,
    headings: usize,
    below: usize,
    above: usize,
    names: HashMap<String, usize>,
    order: Vec<String>,
}

impl Run {
    fn add(&mut self, node: &Node) {
        self.nodes.push(node.clone());
        if self.actionable + self.headings == 0 {
            self.depth = node.depth;
        }
        self.depth = self.depth.min(node.depth);
        match node.position {
            Some(Position::Above) => self.above += 1,
            _ => self.below += 1,
        }
        if node.kind == NodeKind::Actionable {
            self.actionable += 1;
            let label = if node.name.is_empty() {
                node.role.clone()
            } else {
                node.name.chars().take(30).collect()
            };
            let count = self.names.entry(label.clone()).or_insert(0);
            if *count == 0 {
                self.order.push(label);
            }
            *count += 1;
        } else {
            self.headings += 1;
        }
    }

    /// The summary line, or the nodes themselves when there are too few to summarize.
    fn into_nodes(self) -> Vec<Node> {
        if self.actionable + self.headings < MIN_SUMMARY {
            return self.nodes;
        }
        let direction = if self.above > self.below {
            "above"
        } else {
            "below"
        };
        let mut text = if self.actionable > 0 {
            format!("{} more actionable elements {direction}", self.actionable)
        } else {
            format!("{} more sections {direction}", self.headings)
        };
        // The most frequent controls say what kind of content is there.
        let mut common: Vec<(&String, &usize)> =
            self.names.iter().filter(|(_, n)| **n > 1).collect();
        common.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let examples: Vec<String> = if common.is_empty() {
            self.order
                .iter()
                .take(4)
                .map(|name| format!("\"{name}\""))
                .collect()
        } else {
            common
                .iter()
                .take(4)
                .map(|(name, n)| format!("{n}× \"{name}\""))
                .collect()
        };
        if !examples.is_empty() {
            text.push_str(&format!(", incl. {}", examples.join(", ")));
        }
        vec![Node {
            kind: NodeKind::Summary,
            depth: self.depth,
            role: "summary".into(),
            name: text,
            reference: None,
            value: None,
            description: None,
            states: Vec::new(),
            effects: Vec::new(),
            href: None,
            position: None,
            distance: 0.0,
            rect: None,
            form_part: None,
        }]
    }
}

/// Lists what is in view or near it, and summarizes the rest per stretch of the page
/// ("… 140 more below, incl. 12× Comment"). Scrolling brings summarized parts in.
pub(crate) fn focus(nodes: &mut Vec<Node>) {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    let mut run = Run::default();
    let mut offscreen_listed = 0;
    for node in std::mem::take(nodes) {
        let offscreen = node.kind == NodeKind::Actionable
            && node.position.is_some_and(|p| p != Position::Visible);
        let far = node.kind != NodeKind::Structure
            && (node.distance > NEAR || (offscreen && offscreen_listed >= NEAR_OFFSCREEN_BUDGET));
        if far {
            run.add(&node);
            continue;
        }
        out.extend(std::mem::take(&mut run).into_nodes());
        if offscreen {
            offscreen_listed += 1;
        }
        out.push(node);
    }
    out.extend(run.into_nodes());
    *nodes = prune_empty_structures(out);
}

/// Keeps the outline within `MAX_ACTIONABLE` actionable nodes, dropping off-screen ones
/// first (from the bottom of the page up), since what is in view is what the agent is
/// most likely to act on next. Returns how many were dropped.
pub(crate) fn apply_budget(nodes: &mut Vec<Node>) -> usize {
    let total = count_actionable(nodes);
    if total <= MAX_ACTIONABLE {
        return 0;
    }
    let mut excess = total - MAX_ACTIONABLE;
    let mut keep = vec![true; nodes.len()];
    for offscreen_only in [true, false] {
        for (i, node) in nodes.iter().enumerate().rev() {
            if excess == 0 {
                break;
            }
            let droppable = node.kind == NodeKind::Actionable
                && keep[i]
                && (!offscreen_only || node.position != Some(Position::Visible));
            if droppable {
                keep[i] = false;
                excess -= 1;
            }
        }
    }
    let kept: Vec<Node> = std::mem::take(nodes)
        .into_iter()
        .zip(keep)
        .filter_map(|(node, keep)| keep.then_some(node))
        .collect();
    *nodes = prune_empty_structures(kept);
    total - MAX_ACTIONABLE
}

/// Drops landmarks that have nothing left inside them.
fn prune_empty_structures(mut kept: Vec<Node>) -> Vec<Node> {
    let mut next_depth: Option<usize> = None;
    let mut pruned: Vec<Node> = Vec::with_capacity(kept.len());
    while let Some(node) = kept.pop() {
        if node.kind == NodeKind::Structure && next_depth.is_none_or(|d| d <= node.depth) {
            continue;
        }
        next_depth = Some(node.depth);
        pruned.push(node);
    }
    pruned.reverse();
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, depth: usize, name: &str, href: Option<&str>, distance: f64) -> Node {
        Node {
            kind,
            depth,
            role: if kind == NodeKind::Structure {
                "region".into()
            } else {
                "link".into()
            },
            name: name.into(),
            reference: None,
            value: None,
            description: None,
            states: Vec::new(),
            effects: Vec::new(),
            href: href.map(str::to_owned),
            position: Some(if distance > 0.0 {
                Position::Below
            } else {
                Position::Visible
            }),
            distance,
            rect: None,
            form_part: None,
        }
    }

    #[test]
    fn links_without_a_destination_are_not_merged() {
        let mut nodes = vec![
            node(NodeKind::Structure, 0, "Filter by Price", None, 0.0),
            node(
                NodeKind::Actionable,
                1,
                "add filter: $50.00 to $99.99(28)",
                Some(""),
                0.0,
            ),
            node(
                NodeKind::Actionable,
                1,
                "add filter: $100.00 to $199.99(289)",
                Some(""),
                0.0,
            ),
            node(NodeKind::Actionable, 1, "Size guide", Some("#"), 0.0),
            node(NodeKind::Actionable, 1, "Fit guide", Some("#"), 0.0),
            // A place on the page is a destination: a footnote cited twice is one link.
            node(NodeKind::Actionable, 1, "[2]", Some("#cite_note-2"), 0.0),
            node(NodeKind::Actionable, 1, "[2]", Some("#cite_note-2"), 0.0),
        ];
        tidy(&mut nodes);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Filter by Price",
                "add filter: $50.00 to $99.99(28)",
                "add filter: $100.00 to $199.99(289)",
                "Size guide",
                "Fit guide",
                "[2]"
            ]
        );
    }

    #[test]
    fn keeps_one_link_per_destination_with_the_best_name() {
        let mut nodes = vec![
            node(NodeKind::Structure, 0, "Feed", None, 0.0),
            node(NodeKind::Actionable, 1, "", Some("/in/ada"), 0.0),
            node(
                NodeKind::Actionable,
                1,
                "View Ada’s profile",
                Some("/in/ada"),
                0.0,
            ),
            node(
                NodeKind::Actionable,
                1,
                "Ada Lovelace Premium 2nd",
                Some("/in/ada"),
                0.0,
            ),
            node(NodeKind::Actionable, 1, "Skip to main content", None, 0.0),
        ];
        tidy(&mut nodes);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].name, "Ada Lovelace Premium 2nd");

        // A search result's title wins over the longer address printed under it.
        let url = "https://www.microsoft.com/en-us/download/details.aspx?id=55963";
        let mut results = vec![
            node(
                NodeKind::Actionable,
                1,
                "Office Cheat Sheet: Outlook Mail download",
                Some(url),
                0.0,
            ),
            node(NodeKind::Actionable, 1, url, Some(url), 0.0),
        ];
        tidy(&mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Office Cheat Sheet: Outlook Mail download");
    }

    #[test]
    fn few_far_elements_are_listed_not_summarized() {
        let mut nodes = vec![
            node(NodeKind::Actionable, 0, "Visible", Some("/a"), 0.0),
            node(NodeKind::Actionable, 0, "Far", Some("/b"), 3.0),
        ];
        focus(&mut nodes);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].name, "Far");
    }

    #[test]
    fn near_offscreen_elements_are_listed_up_to_a_budget() {
        let mut nodes = vec![node(NodeKind::Actionable, 0, "Visible", Some("/a"), 0.0)];
        for i in 0..60 {
            nodes.push(node(
                NodeKind::Actionable,
                0,
                "Link",
                Some(&format!("/l{i}")),
                0.5,
            ));
        }
        focus(&mut nodes);
        assert_eq!(nodes.len(), 1 + NEAR_OFFSCREEN_BUDGET + 1);
        assert!(
            nodes
                .last()
                .unwrap()
                .name
                .starts_with("20 more actionable elements below")
        );
    }

    #[test]
    fn summarizes_what_is_far_away() {
        let mut nodes = vec![node(NodeKind::Actionable, 0, "Visible", Some("/a"), 0.0)];
        for i in 0..6 {
            nodes.push(node(
                NodeKind::Actionable,
                0,
                "Comment",
                Some(&format!("/c{i}")),
                3.0,
            ));
        }
        focus(&mut nodes);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].kind, NodeKind::Summary);
        assert!(
            nodes[1]
                .name
                .contains("6 more actionable elements below, incl. 6× \"Comment\""),
            "{}",
            nodes[1].name
        );
    }
}
