//! Agent front doors (docs/design.md §5.10): what a site publishes for agents rather than
//! for people, discovered by probing each origin once.
//!
//! - llms.txt and llms-full.txt: markdown indexes written for language models;
//! - A2A Agent Cards: the site's own agent, which tasks can be delegated to;
//! - MCP Server Cards: the site's MCP server;
//! - API catalogs (RFC 9727) and `service-desc` links (RFC 8631): documented APIs;
//! - robots.txt rules for AI user agents, and Content Signals;
//! - sitemaps, read on demand: the pages a site lists for crawlers.
//!
//! Probes run in the background over plain cookie-less HTTP, are size-capped and
//! time-boxed, and are cached per origin, misses included. Everything found is
//! site-authored and untrusted: it reaches the agent summarized and quoted, and nothing
//! here ever connects to a discovered endpoint.

mod agent_card;
mod api_catalog;
mod cache;
mod doc;
mod fetch;
mod llms;
mod mcp_card;
mod openapi;
mod probe;
pub mod render;
mod robots;
mod sitemap;
mod text;

pub use agent_card::{AgentCard, Endpoint, Skill};
pub use api_catalog::{Api, ApiCatalog, ApiLink};
pub use cache::{FrontDoors, SiteStore, origin_of};
pub use doc::Document;
pub use fetch::Fetcher;
pub use llms::LlmsTxt;
pub use mcp_card::McpServer;
pub use openapi::{ApiSpec, Operation, Param, candidates as spec_candidates, looks_like_spec};
pub use probe::SiteInfo;
pub use robots::{Access, AiRule, ContentSignal, Robots};
pub use sitemap::SitePages;
pub use text::{clip, ident, quote};
