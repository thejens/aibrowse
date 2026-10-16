//! Documented public APIs: an API catalog at `/.well-known/api-catalog` (RFC 9727, a
//! linkset in JSON, RFC 9264), and `service-desc` / `service-doc` links in a page
//! (RFC 8631). A documented API is preferred over the private ones learned from traffic.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::text::web_url;

const MAX_APIS: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiCatalog {
    pub url: String,
    pub apis: Vec<Api>,
    /// Entries in the catalog, when more than were kept.
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Api {
    /// The API's identifying URL (the linkset `anchor`, or an `item` href).
    pub anchor: String,
    /// Machine-readable description (OpenAPI and the like).
    pub spec: Option<String>,
    /// Human-readable documentation.
    pub docs: Option<String>,
}

/// A `<link>` in a page pointing at API descriptions or a catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiLink {
    /// "service-desc", "service-doc" or "api-catalog".
    pub rel: String,
    pub href: String,
}

/// Parses a linkset-JSON API catalog, or `None` if `json` isn't one.
pub fn parse(url: &Url, json: &Value) -> Option<ApiCatalog> {
    let linkset = json["linkset"].as_array()?;
    let first_href = |entry: &Value, rel: &str| {
        entry[rel]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|t| t["href"].as_str().and_then(|h| web_url(h, url)))
            .map(String::from)
    };
    let mut apis: Vec<Api> = Vec::new();
    for entry in linkset {
        let spec = first_href(entry, "service-desc");
        let docs = first_href(entry, "service-doc");
        if let Some(anchor) = entry["anchor"].as_str().and_then(|a| web_url(a, url))
            && (spec.is_some() || docs.is_some())
        {
            apis.push(Api {
                anchor: anchor.into(),
                spec,
                docs,
            });
        }
        // The catalog's own entry lists the APIs it covers as `item` links.
        for item in entry["item"].as_array().into_iter().flatten() {
            if let Some(href) = item["href"].as_str().and_then(|h| web_url(h, url))
                && !apis.iter().any(|a| a.anchor == href.as_str())
            {
                apis.push(Api {
                    anchor: href.into(),
                    spec: None,
                    docs: None,
                });
            }
        }
    }
    let count = apis.len();
    apis.truncate(MAX_APIS);
    Some(ApiCatalog {
        url: url.to_string(),
        apis,
        count,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_an_rfc9727_catalog() {
        let url = Url::parse("https://www.example.com/.well-known/api-catalog").unwrap();
        let catalog = json!({ "linkset": [
            { "anchor": "https://developer.example.com/apis/foo_api",
              "service-desc": [{ "href": "https://developer.example.com/apis/foo_api/spec", "type": "application/yaml" }],
              "service-doc": [{ "href": "/apis/foo_api/doc", "type": "text/html" }] },
            { "anchor": "https://www.example.com/.well-known/api-catalog",
              "item": [{ "href": "https://developer.example.com/apis/foo_api" },
                       { "href": "https://developer.example.com/apis/bar_api" }] }
        ]});
        let parsed = parse(&url, &catalog).unwrap();
        assert_eq!(parsed.count, 2);
        assert_eq!(
            parsed.apis[0].spec.as_deref(),
            Some("https://developer.example.com/apis/foo_api/spec")
        );
        assert_eq!(
            parsed.apis[0].docs.as_deref(),
            Some("https://www.example.com/apis/foo_api/doc")
        );
        assert_eq!(
            parsed.apis[1].anchor,
            "https://developer.example.com/apis/bar_api"
        );
    }

    #[test]
    fn rejects_other_json() {
        let url = Url::parse("https://x.example/.well-known/api-catalog").unwrap();
        assert!(parse(&url, &json!({ "apis": [] })).is_none());
    }
}
