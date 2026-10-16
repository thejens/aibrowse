//! Normalizing concrete request URLs into endpoint templates, so that
//! `/voyager/api/relationships/invitations/7123?action=accept` and the same call for
//! another invitation are recognized as one endpoint with a parameter.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

/// Query keys whose value selects *which* operation a URL performs rather than carrying
/// data (`?action=accept`, `?q=search`, LinkedIn's `queryId=voyagerX.abc`). They stay
/// literal in the template; every other query value is a parameter.
const IDENTITY_KEYS: &[&str] = &[
    "action",
    "_action",
    "q",
    "queryid",
    "operationname",
    "op",
    "operation",
    "method",
    "decorationid",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Segment {
    Literal(String),
    /// A variable path segment, named for the agent (`{invitations_id}`).
    Param(String),
}

/// GraphQL operations multiplex one URL, so the operation is part of the identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphQl {
    pub operation: String,
    /// "query", "mutation" or "subscription"; "query" when it can't be told.
    pub kind: String,
}

/// The identity of an endpoint: what stays the same across calls.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Template {
    /// `scheme://host[:port]`.
    pub origin: String,
    pub segments: Vec<Segment>,
    /// Identity query keys and their literal values, sorted.
    pub selectors: Vec<(String, String)>,
    pub graphql: Option<GraphQl>,
    /// A Next.js server action's id (its `Next-Action` request header). Every action
    /// posts to the page's own URL, so the id is what tells one from another.
    /// Templates without one serialize as before, so stored knowledge keeps its keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// The header Next.js sends a server action's id in.
pub const NEXT_ACTION: &str = "Next-Action";

impl Template {
    /// The template of a request, or `None` for URLs that aren't HTTP(S).
    pub fn of(url: &str, body: Option<&str>) -> Option<Self> {
        let parsed = Url::parse(url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return None;
        }
        let raw: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.collect())
            .unwrap_or_default();
        let mut segments = Vec::with_capacity(raw.len());
        for (i, segment) in raw.iter().enumerate() {
            // A short number leading the path is an API version (`/1/indexes/...`).
            let version =
                i == 0 && segment.len() <= 2 && segment.bytes().all(|b| b.is_ascii_digit());
            if !version && is_variable(segment) {
                let name = match segments.last() {
                    Some(Segment::Literal(prev)) => format!("{}_id", ident(prev)),
                    _ => format!("p{}", i + 1),
                };
                segments.push(Segment::Param(unique(&segments, name)));
            } else {
                segments.push(Segment::Literal((*segment).to_owned()));
            }
        }
        let mut selectors: Vec<(String, String)> = parsed
            .query_pairs()
            .filter(|(k, v)| {
                let key = k.to_ascii_lowercase();
                // `q` names an operation on some APIs (LinkedIn's `q=findAlerts`) but is
                // the user's search text on most: only an identifier-like value counts.
                if key == "q" {
                    return is_identifier(v);
                }
                IDENTITY_KEYS.contains(&key.as_str()) || is_operation_name(v)
            })
            .map(|(k, v)| (k.to_string(), selector_value(&k, &v)))
            .collect();
        selectors.sort();
        selectors.dedup();
        let graphql = raw
            .iter()
            .any(|s| s.eq_ignore_ascii_case("graphql"))
            .then(|| graphql_operation(&parsed, body))
            .flatten();
        Some(Self {
            origin: parsed.origin().ascii_serialization(),
            segments,
            selectors,
            graphql,
            action: None,
        })
    }

    /// The template of a documented operation: `path` ("/users/{id}", braces naming
    /// parameters) under `base` ("https://api.example.com/v1").
    pub fn documented(base: &str, path: &str) -> Option<Self> {
        let base = Url::parse(base).ok()?;
        if !matches!(base.scheme(), "http" | "https") {
            return None;
        }
        let segments = base
            .path()
            .split('/')
            .chain(path.split('/'))
            .filter(|s| !s.is_empty())
            .map(
                |s| match s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    Some(name) => Segment::Param(name.to_owned()),
                    None => Segment::Literal(s.to_owned()),
                },
            )
            .collect();
        Some(Self {
            origin: base.origin().ascii_serialization(),
            segments,
            selectors: Vec::new(),
            graphql: None,
            action: None,
        })
    }

    /// Whether this template, learned from requests, is the `documented` one: same
    /// origin and length, and every segment the same, except where the documentation
    /// has a parameter. A segment the requests showed varying is never one the
    /// documentation spells out (`/users/{users_id}` isn't `/users/search`).
    /// Multiplexed endpoints (GraphQL operations, server actions) aren't documented
    /// this way.
    pub fn fits(&self, documented: &Template) -> bool {
        self.fit(documented).is_some()
    }

    /// How specifically this template fits a documented one, if it does: the number of
    /// segments the documentation spells out. `/users/me` fits both `/users/me` and
    /// `/users/{id}`, and is the first.
    pub fn fit(&self, documented: &Template) -> Option<usize> {
        let fits = self.origin == documented.origin
            && self.graphql.is_none()
            && self.action.is_none()
            && self.segments.len() == documented.segments.len()
            && self
                .segments
                .iter()
                .zip(&documented.segments)
                .all(|(seen, doc)| match (seen, doc) {
                    (_, Segment::Param(_)) => true,
                    (Segment::Literal(a), Segment::Literal(b)) => a == b,
                    (Segment::Param(_), Segment::Literal(_)) => false,
                });
        fits.then(|| {
            documented
                .segments
                .iter()
                .filter(|s| matches!(s, Segment::Literal(_)))
                .count()
        })
    }

    /// This template with the path segments where it and `other` differ made
    /// parameters, when the two are clearly one endpoint called for different things:
    /// GitHub's `/tokio-rs/tokio/_sidebar` and `/seanmonstar/reqwest/_sidebar` are
    /// `/{p1}/{p2}/_sidebar`. Only when they differ in one or two literal segments of a
    /// path of at least three, and end alike: `/api/users` and `/api/orders` are
    /// different endpoints, as are `/repo/issues` and `/repo/pulls`.
    pub fn generalize(&self, other: &Template) -> Option<Template> {
        let comparable = self.origin == other.origin
            && self.graphql.is_none()
            && other.graphql.is_none()
            && self.action.is_none()
            && other.action.is_none()
            && self.selectors == other.selectors
            && self.segments.len() == other.segments.len()
            && self.segments.len() >= 3
            && self.segments.last() == other.segments.last()
            && matches!(self.segments.last(), Some(Segment::Literal(_)));
        if !comparable {
            return None;
        }
        let differing: Vec<usize> = (0..self.segments.len())
            .filter(|&i| self.segments[i] != other.segments[i])
            .collect();
        let literal = |s: &Segment| matches!(s, Segment::Literal(_));
        if differing.is_empty()
            || differing.len() > 2
            || !differing
                .iter()
                .all(|&i| literal(&self.segments[i]) && literal(&other.segments[i]))
        {
            return None;
        }
        let mut segments: Vec<Segment> = Vec::with_capacity(self.segments.len());
        for (i, segment) in self.segments.iter().enumerate() {
            if differing.contains(&i) {
                let name = match segments.last() {
                    Some(Segment::Literal(prev)) => format!("{}_id", ident(prev)),
                    _ => format!("p{}", i + 1),
                };
                segments.push(Segment::Param(unique(&segments, name)));
            } else {
                segments.push(segment.clone());
            }
        }
        Some(Template {
            segments,
            ..self.clone()
        })
    }

    /// The server action a request calls, from its headers (`Next-Action`), if any.
    pub fn with_action(mut self, headers: &[(String, String)]) -> Self {
        self.action = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(NEXT_ACTION))
            .map(|(_, value)| value.trim().to_owned())
            .filter(|id| !id.is_empty());
        self
    }

    /// The path with placeholders, e.g. `/api/users/{users_id}/repos`.
    pub fn path(&self) -> String {
        let mut path = String::new();
        for segment in &self.segments {
            path.push('/');
            match segment {
                Segment::Literal(s) => path.push_str(s),
                Segment::Param(name) => {
                    path.push('{');
                    path.push_str(name);
                    path.push('}');
                }
            }
        }
        if path.is_empty() {
            path.push('/');
        }
        path
    }

    pub fn params(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Param(name) => Some(name.as_str()),
            Segment::Literal(_) => None,
        })
    }
}

impl fmt::Display for Template {
    /// `/path/{param}?action=accept (graphql mutation AcceptInvite)`, origin omitted.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path())?;
        for (i, (key, value)) in self.selectors.iter().enumerate() {
            write!(f, "{}{key}={value}", if i == 0 { '?' } else { '&' })?;
        }
        if let Some(gql) = &self.graphql {
            write!(f, " (graphql {} {})", gql.kind, gql.operation)?;
        }
        if let Some(action) = &self.action {
            write!(f, " (server action {action})")?;
        }
        Ok(())
    }
}

/// Whether a path segment carries data (an id, a key, a token) rather than naming a
/// resource. Errs toward literal: a missed id only splits one endpoint into several,
/// while a wrongly merged literal would make two different endpoints look like one.
pub(crate) fn is_variable(segment: &str) -> bool {
    let decoded = url::form_urlencoded::parse(format!("x={segment}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| segment.to_owned());
    let s = decoded.as_str();
    if s.is_empty() {
        return false;
    }
    let digits = s.bytes().filter(u8::is_ascii_digit).count();
    let upper = s.bytes().filter(u8::is_ascii_uppercase).count();
    let lower = s.bytes().filter(u8::is_ascii_lowercase).count();
    let all_hex = s.bytes().all(|b| b.is_ascii_hexdigit());
    s.bytes().all(|b| b.is_ascii_digit())
        || is_uuid(s)
        || (all_hex && s.len() >= 12 && digits > 0)
        || s.starts_with("urn:")
        // Rest.li keys `(id:123)`, compound keys, emails.
        || s.contains(['(', ':', '@', '=', ','])
        // Opaque ids and tokens: long, or short but digit-heavy, or random-cased
        // (`dQw4w9WgXcQ`); words in paths rarely mix digits with both cases.
        || (s.len() >= 20 && digits > 0)
        || (s.len() >= 8 && digits >= 3 && upper + lower > 0)
        || (s.len() >= 8 && digits > 0 && upper >= 2 && lower >= 2)
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, len)| p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A parameter name from a path segment: `relationships` stays, `my-items` → `my_items`.
fn ident(segment: &str) -> String {
    let name: String = segment
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    name.trim_matches('_').to_ascii_lowercase()
}

fn unique(existing: &[Segment], name: String) -> String {
    let taken = |n: &str| existing.iter().any(|s| *s == Segment::Param(n.to_owned()));
    if !taken(&name) {
        return name;
    }
    (2..)
        .map(|i| format!("{name}{i}"))
        .find(|n| !taken(n))
        .expect("an unused name exists")
}

/// Whether a query value names an operation rather than carrying data: a dotted,
/// code-like identifier such as `com.example.requests.inbox.archiveAction`. Generic
/// action endpoints (server-component and RPC gateways) multiplex every action through
/// one URL and say which one in such a parameter. Requiring three or more segments and
/// a camelCase segment keeps host names (`www.example.com`) and versions out.
fn is_operation_name(value: &str) -> bool {
    let segments: Vec<&str> = value.split('.').collect();
    let identifier = |s: &&str| {
        s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    let camel = |s: &&str| {
        s.chars().any(|c| c.is_ascii_lowercase())
            && s.chars().skip(1).any(|c| c.is_ascii_uppercase())
    };
    segments.len() >= 3 && segments.iter().all(identifier) && segments.iter().any(camel)
}

/// A camelCase identifier (`findAlerts`, `receivedInvitation`): how APIs name an
/// operation, and unlike what people type into a search box.
fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && value.chars().all(|c| c.is_ascii_alphanumeric())
        && value.chars().skip(1).any(|c| c.is_ascii_uppercase())
}

/// LinkedIn-style `queryId=voyagerFeedDashUpdates.8f2c...` names the operation before the
/// dot; the hash after it changes with every frontend deploy.
fn selector_value(key: &str, value: &str) -> String {
    if key.eq_ignore_ascii_case("queryId") {
        return value.split('.').next().unwrap_or(value).to_owned();
    }
    value.to_owned()
}

fn graphql_operation(url: &Url, body: Option<&str>) -> Option<GraphQl> {
    let body: Option<Value> = body.and_then(|b| serde_json::from_str(b).ok());
    // Batched requests send an array; the first operation names the batch.
    let first = match &body {
        Some(Value::Array(items)) => items.first(),
        other => other.as_ref(),
    };
    let document = first
        .and_then(|b| b["query"].as_str())
        .map(str::to_owned)
        .or_else(|| {
            url.query_pairs()
                .find(|(k, _)| k == "query")
                .map(|(_, v)| v.into_owned())
        });
    let (doc_kind, doc_name) = document.as_deref().map(parse_document).unwrap_or_default();
    let name = first
        .and_then(|b| b["operationName"].as_str())
        .map(str::to_owned)
        .or_else(|| {
            url.query_pairs()
                .find(|(k, _)| k == "operationName" || k == "queryId")
                .map(|(k, v)| selector_value(&k, &v))
        })
        .or(doc_name)?;
    Some(GraphQl {
        operation: name,
        kind: doc_kind.unwrap_or_else(|| "query".into()),
    })
}

/// `mutation AcceptInvite($id: ID!) {...}` → ("mutation", "AcceptInvite"); a bare `{...}`
/// is an anonymous query.
fn parse_document(document: &str) -> (Option<String>, Option<String>) {
    let mut words = document
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty());
    match words.next() {
        Some(kind @ ("query" | "mutation" | "subscription")) => {
            (Some(kind.to_owned()), words.next().map(str::to_owned))
        }
        _ => (Some("query".into()), None),
    }
}

/// Whether a request changes state on the server. GraphQL reads sent as POST are reads.
pub fn is_mutating(method: &str, graphql: Option<&GraphQl>) -> bool {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return false;
    }
    graphql.is_none_or(|g| g.kind != "query")
}

/// The registrable part of a host, approximately (`www.linkedin.com` → `linkedin.com`,
/// `www.bbc.co.uk` → `bbc.co.uk`). Good enough to tell a site's own API hosts from
/// third parties (analytics, ads) without shipping the public suffix list.
pub fn site(host: &str) -> String {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    if labels.len() <= 2 || host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_owned();
    }
    let second_level = labels[labels.len() - 2];
    let keep = if second_level.len() <= 3
        && matches!(
            second_level,
            "co" | "com" | "org" | "net" | "ac" | "gov" | "edu" | "ne" | "or"
        ) {
        3
    } else {
        2
    };
    labels[labels.len().saturating_sub(keep)..].join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(url: &str) -> String {
        Template::of(url, None).unwrap().to_string()
    }

    #[test]
    fn ids_become_named_params() {
        assert_eq!(
            shown("https://x.com/api/users/42/repos/9f8e7d6c5b4a3210"),
            "/api/users/{users_id}/repos/{repos_id}"
        );
        assert_eq!(
            shown("https://x.com/v2/items/550e8400-e29b-41d4-a716-446655440000"),
            "/v2/items/{items_id}"
        );
        assert_eq!(
            shown("https://x.com/watch/dQw4w9WgXcQ"),
            "/watch/{watch_id}"
        );
    }

    #[test]
    fn literal_words_stay_literal() {
        assert_eq!(shown("https://x.com/api/v2/oauth2/me"), "/api/v2/oauth2/me");
        assert_eq!(
            shown("https://x-dsn.algolia.net/1/indexes/Item_dev/query"),
            "/1/indexes/Item_dev/query"
        );
        assert_eq!(shown("https://x.com/"), "/");
    }

    #[test]
    fn restli_keys_and_urns_are_params() {
        assert_eq!(
            shown("https://www.linkedin.com/voyager/api/identity/profiles/(id:abc)/views"),
            "/voyager/api/identity/profiles/{profiles_id}/views"
        );
        assert_eq!(
            shown(
                "https://www.linkedin.com/voyager/api/messaging/conversations/urn%3Ali%3Afsd_conversation%3A2-abc"
            ),
            "/voyager/api/messaging/conversations/{conversations_id}"
        );
    }

    #[test]
    fn selector_keys_are_part_of_the_identity() {
        let accept = Template::of(
            "https://www.linkedin.com/voyager/api/relationships/invitations/7123?action=accept&x=1",
            None,
        )
        .unwrap();
        let other = Template::of(
            "https://www.linkedin.com/voyager/api/relationships/invitations/8000?action=accept&x=2",
            None,
        )
        .unwrap();
        let ignore = Template::of(
            "https://www.linkedin.com/voyager/api/relationships/invitations/7123?action=ignore",
            None,
        )
        .unwrap();
        assert_eq!(accept, other);
        assert_ne!(accept, ignore);
        assert_eq!(
            accept.to_string(),
            "/voyager/api/relationships/invitations/{invitations_id}?action=accept"
        );
    }

    #[test]
    fn operation_names_in_queries_split_endpoints() {
        let ignore = Template::of(
            "https://x.com/rsc-action/actions/server-request?sduiid=com.x.requests.mynetwork.addaInvitationAction&span=1",
            None,
        )
        .unwrap();
        let pixels = Template::of(
            "https://x.com/rsc-action/actions/server-request?sduiid=com.x.impl.homenav.getThirdPartyPixels&span=2",
            None,
        )
        .unwrap();
        assert_ne!(ignore, pixels);
        assert_eq!(
            ignore.to_string(),
            "/rsc-action/actions/server-request?sduiid=com.x.requests.mynetwork.addaInvitationAction"
        );
        assert!(!is_operation_name("www.example.com"));
        assert!(!is_operation_name("1.2.3"));
        assert!(!is_operation_name("hello world.a.b"));
    }

    #[test]
    fn search_text_is_never_part_of_an_endpoint() {
        assert_eq!(
            shown("https://github.com/_filter/labels/validate?q=E-easy"),
            "/_filter/labels/validate"
        );
        assert_eq!(shown("https://x.com/search?q=ada+lovelace"), "/search");
        assert_eq!(shown("https://x.com/search?q=rust"), "/search");
        assert_eq!(
            shown("https://www.linkedin.com/voyager/api/voyagerGlobalAlerts?q=findAlerts"),
            "/voyager/api/voyagerGlobalAlerts?q=findAlerts"
        );
    }

    #[test]
    fn graphql_operations_from_url_and_body() {
        assert_eq!(
            shown(
                "https://www.linkedin.com/voyager/api/graphql?variables=(start:0)&queryId=voyagerRelationshipsDashInvitationViews.ab12cd"
            ),
            "/voyager/api/graphql?queryId=voyagerRelationshipsDashInvitationViews (graphql query voyagerRelationshipsDashInvitationViews)"
        );
        let body = r#"{"query":"mutation Star($id: ID!) { addStar(input: {starrableId: $id}) { clientMutationId } }","variables":{"id":"R_1"}}"#;
        let t = Template::of("https://api.github.com/graphql", Some(body)).unwrap();
        assert_eq!(t.graphql.as_ref().unwrap().operation, "Star");
        assert!(is_mutating("POST", t.graphql.as_ref()));
        let read = Template::of(
            "https://api.github.com/graphql",
            Some(r#"{"operationName":"Viewer","query":"query Viewer { viewer { login } }"}"#),
        )
        .unwrap();
        assert!(!is_mutating("POST", read.graphql.as_ref()));
    }

    #[test]
    fn sites() {
        assert_eq!(site("www.linkedin.com"), "linkedin.com");
        assert_eq!(site("api.github.com"), "github.com");
        assert_eq!(site("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(site("localhost"), "localhost");
        assert_eq!(site("127.0.0.1"), "127.0.0.1");
    }
}
