use std::fmt::Write;

use crate::forms::fold;
use crate::model::{Detail, Node, NodeKind, PageModel, Position};

impl PageModel {
    /// The page's forms, one signature line each (the fields to fill, typed, and the
    /// submit button), in page order, as the outline shows them.
    /// The buttons that submit a form with fields, as the outline's form lines name
    /// them (a `<button>` alone, outside any form, submits nothing).
    pub fn form_submits(&self) -> Vec<aibrowse_browser::ElementRef> {
        crate::forms::fold(&self.nodes)
            .submits
            .into_iter()
            .filter_map(|i| self.nodes[i].reference)
            .collect()
    }

    pub fn forms(&self) -> Vec<String> {
        let folded = fold(&self.nodes);
        let mut forms: Vec<(usize, String)> = folded.signatures.into_iter().collect();
        forms.sort_by_key(|(i, _)| *i);
        forms.into_iter().map(|(_, signature)| signature).collect()
    }

    /// Renders the model as a compact outline, the format agents receive.
    ///
    /// ```text
    /// # Sign in to GitHub · GitHub
    /// url: https://github.com/login
    /// viewport: 1440x757, whole page visible
    /// page: login (password field, button "Sign in")
    /// auth: only the user can complete this; …
    /// regions: main, contentinfo
    ///
    /// - main
    ///   - form (login) → fill(username: "Username or email address" [e10] required, password: [e24] required) submit: button "Sign in" [e38]
    ///   - link "Forgot password?" [e265]
    /// - contentinfo
    ///   - link "Docs" [e297] -> docs.github.com
    /// ```
    pub fn render(&self) -> String {
        let mut out = String::new();
        let v = &self.viewport;
        let _ = writeln!(
            out,
            "# {}",
            if self.title.is_empty() {
                "(untitled)"
            } else {
                &self.title
            }
        );
        let _ = writeln!(out, "url: {}", self.url);
        let max_scroll = (v.content_height - v.height).max(0.0);
        let at = if max_scroll < 1.0 {
            "whole page visible".to_owned()
        } else if v.scroll_y <= 1.0 {
            format!("at top of {:.0}px page", v.content_height)
        } else if v.scroll_y >= max_scroll - 1.0 {
            format!("at bottom of {:.0}px page", v.content_height)
        } else {
            format!("scrolled to {:.0} of {:.0}px", v.scroll_y, v.content_height)
        };
        let _ = writeln!(out, "viewport: {:.0}x{:.0}, {at}", v.width, v.height);
        if let Some(notice) = &self.notice {
            let _ = writeln!(out, "note: {notice}");
        }
        if let Some(page) = &self.page {
            let _ = writeln!(out, "page: {} ({})", page.kind, page.evidence.join(", "));
        }
        if let Some(auth) = &self.auth {
            let _ = writeln!(out, "auth: {auth}");
        }
        match &self.region {
            Some(region) => {
                let _ = writeln!(
                    out,
                    "region: {region} (only this part of the page, in full)"
                );
            }
            // The list tells the agent what `observe(region=…)` can narrow to.
            None if self.regions.len() > 1 => {
                let _ = writeln!(out, "regions: {}", self.regions.join(", "));
            }
            None => {}
        }
        out.push('\n');
        let folded = fold(&self.nodes);
        for (i, node) in self.nodes.iter().enumerate() {
            if folded.skip[i] {
                continue;
            }
            match folded.signatures.get(&i) {
                Some(signature) => {
                    let _ = writeln!(out, "{}- {signature}", "  ".repeat(node.depth));
                }
                None => render_node(&mut out, node, self.detail),
            }
        }
        if self.nodes.is_empty() {
            out.push_str("(no visible content)\n");
        }
        if self.omitted > 0 {
            let _ = writeln!(
                out,
                "\n… {} more off-screen actionable elements omitted. Scroll to them, then observe again.",
                self.omitted
            );
        }
        out
    }
}

fn render_node(out: &mut String, node: &Node, detail: Detail) {
    for _ in 0..node.depth {
        out.push_str("  ");
    }
    out.push_str("- ");
    if node.kind == NodeKind::Summary {
        let _ = writeln!(out, "… {}", node.name);
        return;
    }
    if node.role == "text" {
        let _ = write!(out, "text: {}", node.name);
    } else {
        out.push_str(&node.role);
        if !node.name.is_empty() {
            let _ = write!(out, " {:?}", node.name);
        }
    }
    if let Some(reference) = node.reference {
        let _ = write!(out, " [{reference}]");
    }
    if let Some(value) = &node.value {
        let _ = write!(out, " value={value:?}");
    }
    if let Some(description) = &node.description {
        let _ = write!(out, " ({description})");
    }
    for word in node
        .states
        .iter()
        .map(String::as_str)
        .chain(node.effects.iter().copied())
    {
        let _ = write!(out, " {word}");
    }
    // In the focused outline only links leaving the site show where they go: the agent
    // clicks by ref, and internal paths cost tokens without helping it choose.
    if let Some(href) = node
        .href
        .as_deref()
        .filter(|h| !h.trim().is_empty())
        .filter(|h| detail != Detail::Interactive || leaves_site(h))
    {
        let href = if detail == Detail::Interactive {
            short_external(href)
        } else {
            href.to_owned()
        };
        let _ = write!(out, " -> {href}");
    }
    out.push_str(position_suffix(node.position));
    out.push('\n');
}

/// " (below)" and the like for off-screen nodes; nothing for visible ones.
pub(crate) fn position_suffix(position: Option<Position>) -> &'static str {
    match position.unwrap_or(Position::Visible) {
        Position::Visible => "",
        Position::Above => " (above)",
        Position::Below => " (below)",
        Position::Left => " (left)",
        Position::Right => " (right)",
    }
}

/// Hrefs are made relative to the page's origin, so an absolute one leaves the site.
fn leaves_site(href: &str) -> bool {
    href.starts_with("http://") || href.starts_with("https://")
}

/// "https://www.example.com/a/very/long/path?x=1" → "example.com/a/very/long/path…".
fn short_external(href: &str) -> String {
    let rest = href.split_once("://").map_or(href, |(_, rest)| rest);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest
        .split(['?', '#'])
        .next()
        .unwrap_or(rest)
        .trim_end_matches('/');
    if rest.chars().count() > 40 {
        let cut: String = rest.chars().take(40).collect();
        format!("{cut}…")
    } else {
        rest.to_owned()
    }
}
