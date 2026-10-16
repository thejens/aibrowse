//! A2A Agent Cards (<https://a2a-protocol.org>): a site's own agent, which another agent
//! can delegate tasks to. Read in all three shapes seen in the wild: v1.0
//! (`supportedInterfaces`, wrapped security schemes), v0.3 (`url`, `preferredTransport`,
//! `additionalInterfaces`) and the pre-0.3 `/.well-known/agent.json` (`authentication`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::text::{clip, ident, web_url};

/// Skills kept per card; a card can list hundreds.
const MAX_SKILLS: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCard {
    /// Where the card was found.
    pub card_url: String,
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub endpoints: Vec<Endpoint>,
    /// Security scheme types the agent accepts (e.g. "oauth2", "apiKey", "http bearer").
    pub auth: Vec<String>,
    pub skills: Vec<Skill>,
    /// How many skills the card lists, when more than were kept.
    pub skill_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    pub url: String,
    /// "JSONRPC", "GRPC", "HTTP+JSON"; the card's own label.
    pub transport: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Parses an Agent Card served at `card_url`, or `None` if `json` isn't one.
pub fn parse(card_url: &Url, json: &Value) -> Option<AgentCard> {
    let name = clip(json["name"].as_str()?, 80);
    let mut endpoints = Vec::new();
    let mut add = |url: &Value, transport: &Value| {
        if let Some(url) = url.as_str().and_then(|u| web_url(u, card_url))
            && !endpoints.iter().any(|e: &Endpoint| e.url == url.as_str())
        {
            endpoints.push(Endpoint {
                url: url.into(),
                transport: transport.as_str().and_then(ident),
            });
        }
    };
    for interface in json["supportedInterfaces"].as_array().into_iter().flatten() {
        add(&interface["url"], &interface["protocolBinding"]);
    }
    add(&json["url"], &json["preferredTransport"]);
    for interface in json["additionalInterfaces"]
        .as_array()
        .into_iter()
        .flatten()
    {
        add(&interface["url"], &interface["transport"]);
    }
    let skills_json = json["skills"].as_array();
    // A card needs a way in: an endpoint, or at least skills to describe.
    if endpoints.is_empty() && skills_json.is_none_or(Vec::is_empty) {
        return None;
    }
    let skills: Vec<Skill> = skills_json
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s["id"].as_str().or(s["name"].as_str()).and_then(ident)?;
            Some(Skill {
                id,
                name: s["name"].as_str().map(|n| clip(n, 60)),
                description: s["description"].as_str().map(|d| clip(d, 160)),
            })
        })
        .collect();
    Some(AgentCard {
        card_url: card_url.to_string(),
        name,
        description: json["description"].as_str().map(|d| clip(d, 200)),
        version: json["version"].as_str().map(|v| clip(v, 20)),
        endpoints,
        auth: auth_schemes(json),
        skill_count: skills.len(),
        skills: skills.into_iter().take(MAX_SKILLS).collect(),
    })
}

/// The kinds of credentials the agent accepts, from `securitySchemes` (v0.3 plain
/// OpenAPI-style objects, or v1.0 objects wrapped by kind) or the legacy
/// `authentication.schemes` list.
fn auth_schemes(json: &Value) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    let mut push = |kind: String| {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    };
    if let Some(schemes) = json["securitySchemes"].as_object() {
        for scheme in schemes.values() {
            let kind = match scheme["type"].as_str() {
                Some("http") => scheme["scheme"]
                    .as_str()
                    .and_then(ident)
                    .map_or("http".to_owned(), |s| format!("http {}", s.to_lowercase())),
                Some(kind) => ident(kind).unwrap_or_default(),
                // v1.0: {"oauth2SecurityScheme": {...}}, {"httpAuthSecurityScheme": {...}}
                None => scheme
                    .as_object()
                    .and_then(|o| o.keys().next())
                    .map(|k| {
                        let k = k.trim_end_matches("SecurityScheme");
                        match k {
                            "httpAuth" => "http".to_owned(),
                            "mtls" => "mutualTLS".to_owned(),
                            other => ident(other).unwrap_or_default(),
                        }
                    })
                    .unwrap_or_default(),
            };
            if !kind.is_empty() {
                push(kind);
            }
        }
    }
    for scheme in json["authentication"]["schemes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(kind) = scheme.as_str().and_then(ident) {
            push(kind.to_lowercase());
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn card_url() -> Url {
        Url::parse("https://travel.example/.well-known/agent-card.json").unwrap()
    }

    #[test]
    fn parses_a_v03_card() {
        let card = json!({
            "protocolVersion": "0.3.0",
            "name": "Travel Agent",
            "description": "Books flights\nand hotels.",
            "url": "https://travel.example/a2a/v1",
            "preferredTransport": "JSONRPC",
            "additionalInterfaces": [
                { "url": "https://travel.example/a2a/v1", "transport": "JSONRPC" },
                { "url": "https://travel.example/a2a/grpc", "transport": "GRPC" }
            ],
            "version": "1.2.0",
            "securitySchemes": {
                "google": { "type": "openIdConnect", "openIdConnectUrl": "https://accounts.google.com/.well-known/openid-configuration" },
                "bearer": { "type": "http", "scheme": "Bearer" }
            },
            "skills": [
                { "id": "search-flights", "name": "Search flights", "description": "Finds flights", "tags": ["travel"] },
                { "id": "book", "name": "Book" }
            ]
        });
        let parsed = parse(&card_url(), &card).unwrap();
        assert_eq!(parsed.name, "Travel Agent");
        assert_eq!(
            parsed.description.as_deref(),
            Some("Books flights and hotels.")
        );
        assert_eq!(
            parsed.endpoints,
            [
                Endpoint {
                    url: "https://travel.example/a2a/v1".into(),
                    transport: Some("JSONRPC".into())
                },
                Endpoint {
                    url: "https://travel.example/a2a/grpc".into(),
                    transport: Some("GRPC".into())
                },
            ]
        );
        assert_eq!(parsed.auth, ["http bearer", "openIdConnect"]);
        assert_eq!(parsed.skills[0].id, "search-flights");
        assert_eq!(parsed.skill_count, 2);
    }

    #[test]
    fn parses_a_v1_card() {
        let card = json!({
            "name": "Shop",
            "supportedInterfaces": [
                { "url": "/a2a", "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0" }
            ],
            "securitySchemes": { "oauth": { "oauth2SecurityScheme": { "flows": {} } } },
            "skills": []
        });
        let parsed = parse(&card_url(), &card).unwrap();
        assert_eq!(parsed.endpoints[0].url, "https://travel.example/a2a");
        assert_eq!(parsed.endpoints[0].transport.as_deref(), Some("HTTP+JSON"));
        assert_eq!(parsed.auth, ["oauth2"]);
    }

    #[test]
    fn parses_a_legacy_card() {
        let card = json!({
            "name": "Legacy",
            "url": "https://travel.example/agent",
            "authentication": { "schemes": ["Bearer"] },
            "skills": [{ "id": "chat" }]
        });
        let parsed = parse(&card_url(), &card).unwrap();
        assert_eq!(parsed.auth, ["bearer"]);
        assert_eq!(parsed.skills[0].name, None);
    }

    #[test]
    fn rejects_other_json() {
        assert!(parse(&card_url(), &json!({ "name": "x" })).is_none());
        assert!(parse(&card_url(), &json!({ "url": "https://x" })).is_none());
        assert!(parse(&card_url(), &json!([1, 2])).is_none());
    }
}
