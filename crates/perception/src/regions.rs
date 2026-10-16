//! Regions: the page's landmarks (header, navigation, main, aside, footer, dialogs,
//! named sections), listed so the agent can ask for just one of them with
//! `observe(region=…)`. Reading the footer of a busy page that way costs a few lines
//! instead of the whole outline.

use crate::model::{Node, NodeKind};

/// Landmark roles worth listing as regions.
fn is_region(role: &str) -> bool {
    matches!(
        role,
        "banner"
            | "navigation"
            | "main"
            | "complementary"
            | "contentinfo"
            | "search"
            | "form"
            | "region"
            | "dialog"
            | "alertdialog"
    )
}

/// HTML element names and everyday words for the ARIA landmark roles.
fn alias(query: &str) -> &str {
    match query {
        "header" => "banner",
        "footer" => "contentinfo",
        "aside" | "sidebar" => "complementary",
        "nav" => "navigation",
        "content" => "main",
        other => other,
    }
}

/// How a node is named in the region list and accepted back: `main`,
/// `navigation "Contents"`.
fn label(node: &Node) -> String {
    if node.name.is_empty() {
        node.role.clone()
    } else {
        format!("{} {:?}", node.role, node.name)
    }
}

/// The outermost regions, in page order.
pub(crate) fn list(nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside: Option<usize> = None;
    for node in nodes {
        if inside.is_some_and(|depth| node.depth > depth) {
            continue;
        }
        inside = None;
        if node.kind == NodeKind::Structure && is_region(&node.role) {
            out.push(label(node));
            inside = Some(node.depth);
        }
    }
    out
}

/// The subtrees of the regions matching `query`, re-rooted at depth 0: by role
/// ("main", "footer", "dialog"), by label as listed (`navigation "Contents"`), or else by
/// any landmark whose name contains the query. `None` when nothing matches.
pub(crate) fn select(nodes: &[Node], query: &str) -> Option<Vec<Node>> {
    let query = query.trim().to_lowercase();
    let role = alias(&query);
    let by_label = |n: &Node| label(n).to_lowercase() == query;
    let by_role = |n: &Node| n.role == role || (role == "dialog" && n.role == "alertdialog");
    let by_name = |n: &Node| !n.name.is_empty() && n.name.to_lowercase().contains(&query);
    let matchers: [&dyn Fn(&Node) -> bool; 3] = [&by_label, &by_role, &by_name];
    matchers
        .iter()
        .map(|matches| subtrees(nodes, |n| n.kind == NodeKind::Structure && matches(n)))
        .find(|out| !out.is_empty())
}

fn subtrees(nodes: &[Node], matches: impl Fn(&Node) -> bool) -> Vec<Node> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < nodes.len() {
        if !matches(&nodes[i]) {
            i += 1;
            continue;
        }
        let base = nodes[i].depth;
        let end = i + 1 + nodes[i + 1..].iter().take_while(|n| n.depth > base).count();
        out.extend(nodes[i..end].iter().cloned().map(|mut n| {
            n.depth -= base;
            n
        }));
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, depth: usize, role: &str, name: &str) -> Node {
        Node {
            kind,
            depth,
            role: role.into(),
            name: name.into(),
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
        }
    }

    fn page() -> Vec<Node> {
        vec![
            node(NodeKind::Structure, 0, "banner", ""),
            node(NodeKind::Structure, 1, "navigation", "Primary"),
            node(NodeKind::Actionable, 2, "link", "Home"),
            node(NodeKind::Structure, 0, "main", ""),
            node(NodeKind::Content, 1, "heading", "Welcome"),
            node(NodeKind::Structure, 1, "navigation", "Contents"),
            node(NodeKind::Actionable, 2, "link", "History"),
            node(NodeKind::Structure, 0, "contentinfo", ""),
            node(NodeKind::Actionable, 1, "link", "Terms"),
        ]
    }

    #[test]
    fn lists_outermost_regions() {
        assert_eq!(list(&page()), ["banner", "main", "contentinfo"]);
    }

    #[test]
    fn selects_by_alias_label_and_name() {
        let footer = select(&page(), "footer").unwrap();
        assert_eq!(footer.len(), 2);
        assert_eq!((footer[0].depth, footer[1].name.as_str()), (0, "Terms"));

        let navs = select(&page(), "navigation").unwrap();
        assert_eq!(navs.len(), 4, "both navigations");

        let contents = select(&page(), "navigation \"contents\"").unwrap();
        assert_eq!(contents[1].name, "History");

        let by_name = select(&page(), "primary").unwrap();
        assert_eq!(by_name[1].name, "Home");

        assert!(select(&page(), "dialog").is_none());
    }
}
