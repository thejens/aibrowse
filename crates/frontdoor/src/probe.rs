//! Probing one origin for everything it publishes for agents, all at once.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::agent_card::{self, AgentCard};
use crate::api_catalog::{self, ApiCatalog};
use crate::fetch::{Fetched, Fetcher, Outcome};
use crate::llms::{self, LlmsTxt};
use crate::mcp_card::{self, McpServer};
use crate::robots::{self, Robots};

/// Google's robots.txt limit; rules past it are ignored by crawlers too.
const ROBOTS_CAP: usize = 500 * 1024;
const LLMS_CAP: usize = 256 * 1024;
/// llms-full.txt is often megabytes; its beginning is enough to recognize it.
const LLMS_FULL_CAP: usize = 64 * 1024;
const CARD_CAP: usize = 256 * 1024;

const JSON: &str = "application/json";
const TEXT: &str = "text/plain, text/markdown;q=0.9, */*;q=0.1";

/// A2A card locations: v0.3+, then the earlier name.
const AGENT_CARD_PATHS: &[&str] = &["/.well-known/agent-card.json", "/.well-known/agent.json"];
/// MCP Server Card locations, most common first. The draft spec hasn't settled: deployed
/// cards mostly use the first, SEP-2127 proposes the second, earlier drafts and this
/// project's design used the others.
const MCP_CARD_PATHS: &[&str] = &[
    "/.well-known/mcp/server-card.json",
    "/.well-known/mcp/server-cards.json",
    "/.well-known/mcp-server-card",
    "/mcp/server-card",
];

/// How long results stay fresh: what the server's `Cache-Control` says, within bounds.
const MIN_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_TTL: Duration = Duration::from_secs(24 * 3600);
const DEFAULT_TTL: Duration = Duration::from_secs(6 * 3600);
/// An origin with nothing for agents is remembered for a day, so its pages are never
/// re-probed on every visit.
const MISS_TTL: Duration = MAX_TTL;
/// When a probe got no answer (timeout, server error), try again sooner.
const FAILURE_TTL: Duration = Duration::from_secs(10 * 60);

/// Everything one origin publishes for agents.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SiteInfo {
    /// e.g. "https://example.com".
    pub origin: String,
    /// Unix seconds.
    pub probed_at: u64,
    /// How long this stays fresh, in seconds.
    pub ttl_secs: u64,
    pub llms_txt: Option<LlmsTxt>,
    pub llms_full: Option<LlmsTxt>,
    pub agent_card: Option<AgentCard>,
    pub mcp_servers: Vec<McpServer>,
    pub api_catalog: Option<ApiCatalog>,
    pub robots: Option<Robots>,
    /// Probes that got no answer, e.g. "llms.txt: timed out".
    pub failed: Vec<String>,
}

impl SiteInfo {
    /// Whether the site offers an interface built for agents (robots.txt alone isn't one).
    pub fn has_interfaces(&self) -> bool {
        self.llms_txt.is_some()
            || self.llms_full.is_some()
            || self.agent_card.is_some()
            || !self.mcp_servers.is_empty()
            || self.api_catalog.is_some()
    }

    /// Whether robots.txt says anything specific to AI.
    pub fn has_ai_rules(&self) -> bool {
        self.robots
            .as_ref()
            .is_some_and(|r| !r.ai_rules.is_empty() || !r.signals.is_empty())
    }

    pub fn age(&self) -> Duration {
        Duration::from_secs(now().saturating_sub(self.probed_at))
    }

    pub fn is_fresh(&self) -> bool {
        self.age() < Duration::from_secs(self.ttl_secs)
    }
}

/// Probes `origin` (scheme, host and port) for every known front door, concurrently.
pub async fn probe(fetcher: &Fetcher, origin: &Url) -> SiteInfo {
    let at = |path: &str| origin.join(path).expect("well-known paths are valid");
    let (robots, llms_txt, llms_full, agent_card, mcp, api_catalog) = tokio::join!(
        fetch(fetcher, at("/robots.txt"), TEXT, ROBOTS_CAP, |f| {
            (!f.is_html())
                .then(|| robots::parse(f.url.as_str(), &f.text()))
                .flatten()
        }),
        fetch(fetcher, at("/llms.txt"), TEXT, LLMS_CAP, llms_doc),
        fetch(fetcher, at("/llms-full.txt"), TEXT, LLMS_FULL_CAP, llms_doc),
        first(fetcher, origin, AGENT_CARD_PATHS, |f| {
            agent_card::parse(&f.url, &json(f)?)
        }),
        first(fetcher, origin, MCP_CARD_PATHS, |f| {
            let servers = mcp_card::parse(&f.url, &json(f)?);
            (!servers.is_empty()).then_some(servers)
        }),
        fetch(
            fetcher,
            at("/.well-known/api-catalog"),
            "application/linkset+json, application/json;q=0.9",
            CARD_CAP,
            |f| api_catalog::parse(&f.url, &json(f)?),
        ),
    );
    let mut info = SiteInfo {
        origin: origin.origin().ascii_serialization(),
        probed_at: now(),
        ..SiteInfo::default()
    };
    let mut ttl = Ttl::default();
    info.robots = ttl.take("robots.txt", robots, &mut info.failed);
    info.llms_txt = ttl.take("llms.txt", llms_txt, &mut info.failed);
    info.llms_full = ttl.take("llms-full.txt", llms_full, &mut info.failed);
    info.agent_card = ttl.take("agent card", agent_card, &mut info.failed);
    info.mcp_servers = ttl
        .take("MCP server card", mcp, &mut info.failed)
        .unwrap_or_default();
    info.api_catalog = ttl.take("api-catalog", api_catalog, &mut info.failed);
    info.ttl_secs = ttl.finish(info.has_interfaces()).as_secs();
    info
}

/// The result of one probe, and what it implies for caching.
struct Probe<T> {
    found: Option<T>,
    max_age: Option<u64>,
    failure: Option<String>,
}

async fn fetch<T>(
    fetcher: &Fetcher,
    url: Url,
    accept: &str,
    cap: usize,
    parse: impl Fn(&Fetched) -> Option<T>,
) -> Probe<T> {
    match fetcher.get(&url, accept, cap).await {
        Outcome::Found(fetched) => Probe {
            found: parse(&fetched),
            max_age: fetched.max_age,
            failure: None,
        },
        Outcome::Missing => Probe {
            found: None,
            max_age: None,
            failure: None,
        },
        Outcome::Failed(reason) => Probe {
            found: None,
            max_age: None,
            failure: Some(reason),
        },
    }
}

/// Tries `paths` in order and returns the first that parses.
async fn first<T>(
    fetcher: &Fetcher,
    origin: &Url,
    paths: &[&str],
    parse: impl Fn(&Fetched) -> Option<T>,
) -> Probe<T> {
    let mut failure = None;
    for path in paths {
        let url = origin.join(path).expect("well-known paths are valid");
        let probe = fetch(fetcher, url, JSON, CARD_CAP, &parse).await;
        if probe.found.is_some() {
            return probe;
        }
        failure = failure.or(probe.failure);
    }
    Probe {
        found: None,
        max_age: None,
        failure,
    }
}

fn llms_doc(fetched: &Fetched) -> Option<LlmsTxt> {
    if fetched.is_html() {
        return None;
    }
    let mut doc = llms::parse(fetched.url.as_str(), &fetched.text(), fetched.truncated)?;
    doc.bytes = doc.bytes.or(fetched.content_length);
    Some(doc)
}

fn json(fetched: &Fetched) -> Option<serde_json::Value> {
    if fetched.is_html() || fetched.truncated {
        return None;
    }
    serde_json::from_slice(&fetched.body).ok()
}

/// Folds probe results into the cache lifetime of the whole origin's record.
#[derive(Default)]
struct Ttl {
    shortest_max_age: Option<u64>,
    failed: bool,
}

impl Ttl {
    fn take<T>(&mut self, what: &str, probe: Probe<T>, failed: &mut Vec<String>) -> Option<T> {
        if let Some(reason) = probe.failure {
            self.failed = true;
            failed.push(format!("{what}: {reason}"));
        }
        if probe.found.is_some()
            && let Some(age) = probe.max_age
        {
            self.shortest_max_age = Some(self.shortest_max_age.map_or(age, |s| s.min(age)));
        }
        probe.found
    }

    fn finish(self, found: bool) -> Duration {
        if self.failed {
            FAILURE_TTL
        } else if !found {
            MISS_TTL
        } else {
            self.shortest_max_age
                .map_or(DEFAULT_TTL, Duration::from_secs)
                .clamp(MIN_TTL, MAX_TTL)
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_follows_cache_control_within_bounds() {
        let ttl = |max_age: Option<u64>, failed: bool, found: bool| {
            let mut t = Ttl {
                shortest_max_age: max_age,
                failed,
            };
            std::mem::take(&mut t).finish(found)
        };
        assert_eq!(ttl(None, false, false), MISS_TTL);
        assert_eq!(ttl(None, true, true), FAILURE_TTL);
        assert_eq!(ttl(None, false, true), DEFAULT_TTL);
        assert_eq!(ttl(Some(0), false, true), MIN_TTL);
        assert_eq!(ttl(Some(7200), false, true), Duration::from_secs(7200));
        assert_eq!(ttl(Some(10_000_000), false, true), MAX_TTL);
    }
}
