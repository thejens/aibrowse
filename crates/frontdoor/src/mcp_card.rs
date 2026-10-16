//! MCP Server Cards: a site describing its own MCP server. The format is still a draft
//! (SEP-2127, an experimental MCP extension as of late 2026) and has moved around, so
//! this reads the shapes in circulation:
//!
//! - registry style (SEP-2127): `name`, `title`, `version`, `remotes: [{type, url}]`,
//!   with authentication inside each remote;
//! - the earlier draft (SEP-1649): `serverInfo: {name, version}`,
//!   `transport: {type, endpoint}`, `authentication: {required, schemes}`, `tools`;
//! - a list of either, as `/.well-known/mcp/server-cards.json` proposes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::agent_card::Endpoint;
use crate::text::{clip, ident, web_url};

/// Servers kept from one card list.
const MAX_SERVERS: usize = 5;
const MAX_TOOLS: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    pub card_url: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub endpoints: Vec<Endpoint>,
    pub auth: Vec<String>,
    /// Tool names, when the card lists them (newer drafts deliberately don't).
    pub tools: Vec<String>,
}

/// The servers described by the card at `card_url`; empty if `json` isn't a card.
pub fn parse(card_url: &Url, json: &Value) -> Vec<McpServer> {
    let list = match json {
        Value::Array(items) => items.as_slice(),
        Value::Object(map) => match ["serverCards", "servers", "cards"]
            .iter()
            .find_map(|k| map.get(*k)?.as_array())
        {
            Some(items) => items.as_slice(),
            None => std::slice::from_ref(json),
        },
        _ => &[],
    };
    list.iter()
        .filter_map(|card| server(card_url, card))
        .take(MAX_SERVERS)
        .collect()
}

fn server(card_url: &Url, card: &Value) -> Option<McpServer> {
    let info = if card["serverInfo"].is_object() {
        &card["serverInfo"]
    } else {
        card
    };
    let name = info["name"].as_str().and_then(ident)?;
    let mut endpoints = Vec::new();
    let mut auth = Vec::new();
    for remote in card["remotes"].as_array().into_iter().flatten() {
        if let Some(url) = remote["url"].as_str().and_then(|u| web_url(u, card_url)) {
            endpoints.push(Endpoint {
                url: url.into(),
                transport: remote["type"].as_str().and_then(ident),
            });
        }
        collect_auth(&remote["authentication"], &mut auth);
    }
    let transport = &card["transport"];
    if let Some(url) = transport["endpoint"]
        .as_str()
        .or(transport["url"].as_str())
        .and_then(|u| web_url(u, card_url))
    {
        endpoints.push(Endpoint {
            url: url.into(),
            transport: transport["type"].as_str().and_then(ident),
        });
    }
    collect_auth(&card["authentication"], &mut auth);
    // A card must say where the server is; a name alone could be any JSON document.
    if endpoints.is_empty() {
        return None;
    }
    let tools = card["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str().or(t.as_str()).and_then(ident))
        .take(MAX_TOOLS)
        .collect();
    Some(McpServer {
        card_url: card_url.to_string(),
        name,
        title: info["title"].as_str().map(|t| clip(t, 80)),
        description: card["description"]
            .as_str()
            .or(info["description"].as_str())
            .map(|d| clip(d, 200)),
        version: info["version"].as_str().map(|v| clip(v, 20)),
        endpoints,
        auth,
        tools,
    })
}

/// `{"schemes": ["oauth2"]}`, `{"type": "oauth2"}` or `"oauth2"`; `{"required": false}`
/// alone means none is needed.
fn collect_auth(value: &Value, out: &mut Vec<String>) {
    let mut kinds: Vec<Option<String>> = Vec::new();
    match value {
        Value::String(kind) => kinds.push(ident(kind)),
        Value::Object(map) => {
            for scheme in map
                .get("schemes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                kinds.push(scheme.as_str().and_then(ident));
            }
            kinds.push(map.get("type").and_then(Value::as_str).and_then(ident));
            // Required, but without saying how: worth knowing before trying it.
            if map.get("required") == Some(&Value::Bool(true)) && kinds.iter().all(Option::is_none)
            {
                kinds.push(Some("required".into()));
            }
        }
        _ => {}
    }
    for kind in kinds.into_iter().flatten() {
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn card_url() -> Url {
        Url::parse("https://bex.co/.well-known/mcp/server-card.json").unwrap()
    }

    #[test]
    fn parses_a_registry_style_card() {
        let card = json!({
            "name": "co.bex/control-plane",
            "version": "2.4.0",
            "title": "Bex Control Plane",
            "description": "Deploy, rollback, and log access.",
            "remotes": [
                { "type": "streamable-http", "url": "https://mcp.bex.co/v1",
                  "authentication": { "schemes": ["oauth2"] } }
            ]
        });
        let servers = parse(&card_url(), &card);
        assert_eq!(servers.len(), 1);
        let server = &servers[0];
        assert_eq!(server.name, "co.bex/control-plane");
        assert_eq!(server.title.as_deref(), Some("Bex Control Plane"));
        assert_eq!(server.endpoints[0].url, "https://mcp.bex.co/v1");
        assert_eq!(
            server.endpoints[0].transport.as_deref(),
            Some("streamable-http")
        );
        assert_eq!(server.auth, ["oauth2"]);
        assert!(server.tools.is_empty());
    }

    #[test]
    fn parses_an_early_draft_card() {
        let card = json!({
            "serverInfo": { "name": "shop", "title": "Shop", "version": "1.0" },
            "transport": { "type": "streamable-http", "endpoint": "/mcp" },
            "authentication": { "required": true },
            "tools": [{ "name": "search_products" }, { "name": "get_order" }]
        });
        let server = &parse(&card_url(), &card)[0];
        assert_eq!(server.endpoints[0].url, "https://bex.co/mcp");
        assert_eq!(server.auth, ["required"]);
        assert_eq!(server.tools, ["search_products", "get_order"]);
    }

    #[test]
    fn parses_a_card_list() {
        let card = json!({ "serverCards": [
            { "name": "a", "remotes": [{ "type": "sse", "url": "https://a.example/sse" }] },
            { "name": "b" }
        ]});
        let servers = parse(&card_url(), &card);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "a");
    }

    #[test]
    fn rejects_other_json() {
        assert!(parse(&card_url(), &json!({ "name": "x" })).is_empty());
        assert!(parse(&card_url(), &json!("text")).is_empty());
    }
}
