//! robots.txt, read for what it says about AI: the rules for AI user agents (crawlers
//! like GPTBot, and user-triggered fetchers like Claude-User), and Content Signals
//! (<https://contentsignals.org>: `Content-Signal: search=yes, ai-train=no`), the site's
//! declared preferences for how its content may be used. These feed the agent and, later,
//! the policy engine (docs/design.md §5.7).

use serde::{Deserialize, Serialize};

use crate::text::ident;

/// Product tokens of AI crawlers and agents, matched case-insensitively.
const AI_AGENTS: &[&str] = &[
    "GPTBot",
    "ChatGPT-User",
    "OAI-SearchBot",
    "ClaudeBot",
    "Claude-User",
    "Claude-SearchBot",
    "anthropic-ai",
    "Claude-Web",
    "Google-Extended",
    "CCBot",
    "PerplexityBot",
    "Perplexity-User",
    "Applebot-Extended",
    "Bytespider",
    "meta-externalagent",
    "meta-externalfetcher",
    "Amazonbot",
    "cohere-ai",
    "MistralAI-User",
    "DuckAssistBot",
    "YouBot",
    "AI2Bot",
    "Diffbot",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Robots {
    pub url: String,
    /// Rules naming AI user agents, one entry per agent.
    pub ai_rules: Vec<AiRule>,
    /// Content Signals, each with the user agents it applies to (`*` for everyone).
    pub signals: Vec<ContentSignal>,
    /// `User-agent: *` disallows the whole site.
    pub blocks_everyone: bool,
    /// The sitemaps it declares (`Sitemap:` lines), in order.
    #[serde(default)]
    pub sitemaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiRule {
    /// The agent's canonical product token.
    pub agent: String,
    pub access: Access,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// `Disallow: /` and no exceptions.
    Blocked,
    /// `Disallow: /` with `Allow` exceptions, or some paths disallowed.
    Partial,
    /// Named, but nothing disallowed.
    Allowed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentSignal {
    pub agents: Vec<String>,
    /// e.g. [("search", "yes"), ("ai-input", "yes"), ("ai-train", "no")].
    pub signals: Vec<(String, String)>,
}

/// One `User-agent` group: its agents, and the rules that follow them.
#[derive(Default)]
struct Group {
    agents: Vec<String>,
    disallow: Vec<String>,
    allow: Vec<String>,
    signals: Vec<(String, String)>,
    /// A rule line was seen, so the next `User-agent` starts a new group.
    has_rules: bool,
}

/// Parses robots.txt `text`, or `None` if it looks like something else (an HTML page
/// served for every path).
pub fn parse(url: &str, text: &str) -> Option<Robots> {
    if text.trim_start().starts_with('<') {
        return None;
    }
    let mut groups: Vec<Group> = Vec::new();
    // Content-Signal lines outside any group apply to everyone.
    let mut global_signals = Vec::new();
    let mut sitemaps = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        match key.as_str() {
            "user-agent" => {
                if groups.last().is_none_or(|g| g.has_rules) {
                    groups.push(Group::default());
                }
                let group = groups.last_mut().expect("a group was just ensured");
                group.agents.push(value.to_owned());
            }
            // Not part of any group: it applies whatever user agent is reading.
            "sitemap" if !value.is_empty() => sitemaps.push(value.to_owned()),
            "disallow" | "allow" | "content-signal" => {
                let Some(group) = groups.last_mut() else {
                    if key == "content-signal" {
                        global_signals.extend(signal_pairs(value));
                    }
                    continue;
                };
                group.has_rules = true;
                match key.as_str() {
                    "disallow" if !value.is_empty() => group.disallow.push(value.to_owned()),
                    "allow" if !value.is_empty() => group.allow.push(value.to_owned()),
                    "content-signal" => group.signals.extend(signal_pairs(value)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let mut ai_rules: Vec<AiRule> = Vec::new();
    let mut signals = Vec::new();
    if !global_signals.is_empty() {
        signals.push(ContentSignal {
            agents: vec!["*".into()],
            signals: global_signals,
        });
    }
    let mut blocks_everyone = false;
    for group in &groups {
        let access = access(group);
        for agent in &group.agents {
            if agent == "*" {
                blocks_everyone |= access == Access::Blocked;
            } else if let Some(known) = AI_AGENTS.iter().find(|k| k.eq_ignore_ascii_case(agent))
                && !ai_rules.iter().any(|r| r.agent == *known)
            {
                ai_rules.push(AiRule {
                    agent: (*known).to_owned(),
                    access,
                });
            }
        }
        if !group.signals.is_empty() {
            signals.push(ContentSignal {
                agents: group
                    .agents
                    .iter()
                    .filter_map(|a| ident(a).or_else(|| (a == "*").then(|| "*".into())))
                    .collect(),
                signals: group.signals.clone(),
            });
        }
    }
    Some(Robots {
        url: url.to_owned(),
        ai_rules,
        signals,
        blocks_everyone,
        sitemaps,
    })
}

fn access(group: &Group) -> Access {
    let root_blocked = group.disallow.iter().any(|d| d == "/" || d == "/*");
    match (
        root_blocked,
        group.allow.is_empty(),
        group.disallow.is_empty(),
    ) {
        (true, true, _) => Access::Blocked,
        (true, false, _) | (false, _, false) => Access::Partial,
        (false, _, true) => Access::Allowed,
    }
}

/// `search=yes, ai-train=no` → [("search", "yes"), ("ai-train", "no")].
fn signal_pairs(value: &str) -> Vec<(String, String)> {
    value
        .split([',', ';'])
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let key = ident(key.trim())?.to_ascii_lowercase();
            let value = ident(value.trim())?.to_ascii_lowercase();
            Some((key, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOUDFLARE_STYLE: &str = "\
# As a condition of accessing this website, you agree to abide by the following
# content signals: ...
User-Agent: *
Content-Signal: search=yes, ai-train=no
Allow: /

User-agent: GPTBot
user-agent: CCBot
Disallow: /

User-agent: ClaudeBot
Disallow: /private/
Disallow: /drafts/

User-agent: Claude-User
Disallow:

User-agent: Googlebot
Disallow: /search
";

    #[test]
    fn reads_ai_rules_and_content_signals() {
        let robots = parse("https://news.example/robots.txt", CLOUDFLARE_STYLE).unwrap();
        assert_eq!(
            robots.ai_rules,
            [
                AiRule {
                    agent: "GPTBot".into(),
                    access: Access::Blocked
                },
                AiRule {
                    agent: "CCBot".into(),
                    access: Access::Blocked
                },
                AiRule {
                    agent: "ClaudeBot".into(),
                    access: Access::Partial
                },
                AiRule {
                    agent: "Claude-User".into(),
                    access: Access::Allowed
                },
            ]
        );
        assert_eq!(
            robots.signals,
            [ContentSignal {
                agents: vec!["*".into()],
                signals: vec![
                    ("search".into(), "yes".into()),
                    ("ai-train".into(), "no".into())
                ],
            }]
        );
        assert!(!robots.blocks_everyone);
    }

    #[test]
    fn signals_before_any_group_apply_to_everyone() {
        let robots = parse(
            "u",
            "Content-Signal: ai-input=yes\nUser-agent: *\nDisallow: /\n",
        )
        .unwrap();
        assert_eq!(robots.signals[0].agents, ["*"]);
        assert_eq!(
            robots.signals[0].signals,
            [("ai-input".to_owned(), "yes".to_owned())]
        );
        assert!(robots.blocks_everyone);
        assert!(robots.ai_rules.is_empty());
    }

    #[test]
    fn root_block_with_exceptions_is_partial() {
        let robots = parse("u", "User-agent: GPTBot\nDisallow: /\nAllow: /public/\n").unwrap();
        assert_eq!(robots.ai_rules[0].access, Access::Partial);
    }

    #[test]
    fn rejects_html() {
        assert!(parse("u", "<!DOCTYPE html><html></html>").is_none());
    }
}
