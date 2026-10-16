//! Building and running `api_call` requests. The request runs *inside the page*, as a
//! `fetch` from the page's origin, so cookies, CORS and the site's auth behave exactly as
//! when the site's own code makes the call.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::capture::NetworkLog;
use crate::catalog::{Endpoint, Sample};
use crate::template::NEXT_ACTION;
use crate::template::Segment;

/// Response characters handed back from the page; the agent-facing cap is applied later.
pub const MAX_FETCHED_CHARS: usize = 1_000_000;
const FETCH_TIMEOUT_MS: u64 = 25_000;

/// Headers a page script can't set (the browser owns them) or that would be wrong to
/// replay: `fetch` would reject or silently drop them.
const BROWSER_OWNED: &[&str] = &[
    "accept-charset",
    "accept-encoding",
    "access-control-request-headers",
    "access-control-request-method",
    "connection",
    "content-length",
    "cookie",
    "cookie2",
    "date",
    "dnt",
    "expect",
    "host",
    "keep-alive",
    "origin",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "via",
    "priority",
];

/// A fully resolved request, exactly what will run (and what the user approves).
#[derive(Debug, Clone, Serialize)]
pub struct ApiRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// Send cookies even cross-origin, as the page's own request did. Requests the page
    /// makes without them (public APIs answering `Access-Control-Allow-Origin: *`) must
    /// stay that way: the browser rejects credentialed requests to wildcard CORS.
    pub with_cookies: bool,
}

/// What the agent varies on a known endpoint. Anything not given is taken from the last
/// successful call the page made.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    /// Template path parameters, by name (`invitations_id`).
    pub path: BTreeMap<String, String>,
    /// Query parameters to set; `None` removes one.
    pub query: BTreeMap<String, Option<String>>,
    pub body: Option<Value>,
}

/// Header values shorter than this are never traced to a cookie.
const MIN_TOKEN_LEN: usize = 8;

/// A header the page sent, and where its value comes from at call time.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayHeader {
    pub name: String,
    pub value: String,
    /// The cookie this value was copied from (double-submit CSRF), re-read at call time
    /// so a rotated token doesn't make replays fail.
    pub cookie: Option<String>,
    /// Whether the page sends the cookie's value with its surrounding quotes.
    pub quoted: bool,
}

/// The headers worth replaying from a request the page made.
pub fn replay_headers(
    headers: &[(String, String)],
    cookies: &[(String, String)],
) -> Vec<ReplayHeader> {
    headers
        .iter()
        .filter(|(name, _)| {
            let lower = name.to_ascii_lowercase();
            !(lower.starts_with(':')
                || lower.starts_with("sec-")
                || lower.starts_with("proxy-")
                || BROWSER_OWNED.contains(&lower.as_str()))
        })
        .map(|(name, value)| ReplayHeader {
            name: name.clone(),
            value: value.clone(),
            // Only token-like values count: a short constant ("true", "1") equal to some
            // cookie's value is a coincidence, not a dependency.
            cookie: cookies
                .iter()
                .find(|(_, c)| {
                    value.len() >= MIN_TOKEN_LEN && (c == value || c.trim_matches('"') == value)
                })
                .map(|(cookie, _)| cookie.clone()),
            quoted: value.starts_with('"'),
        })
        .collect()
}

/// The headers and credentials mode for a call to `endpoint` at `url`: from its last
/// successful request if this session saw one, otherwise borrowed from the page's latest
/// call to the same origin, plus the cookie-sourced headers an earlier session learned
/// the endpoint needs (its CSRF header, typically).
pub fn replay_for_endpoint(
    endpoint: &Endpoint,
    log: &NetworkLog,
    url: &str,
) -> (Vec<ReplayHeader>, bool) {
    let (headers, with_cookies) = borrowed_headers(endpoint, log, url);
    (with_own_action(headers, endpoint), with_cookies)
}

/// A server action is called by its id: the endpoint's own, never one carried over
/// from another request (that would call a different action, or turn a plain request
/// into an action call).
fn with_own_action(mut headers: Vec<ReplayHeader>, endpoint: &Endpoint) -> Vec<ReplayHeader> {
    headers.retain(|h| !h.name.eq_ignore_ascii_case(NEXT_ACTION));
    if let Some(action) = &endpoint.template.action {
        headers.push(ReplayHeader {
            name: NEXT_ACTION.to_owned(),
            value: action.clone(),
            cookie: None,
            quoted: false,
        });
    }
    headers
}

fn borrowed_headers(endpoint: &Endpoint, log: &NetworkLog, url: &str) -> (Vec<ReplayHeader>, bool) {
    if let Some(sample) = &endpoint.sample {
        return (
            replay_headers(&sample.headers, &sample.cookies),
            !sample.cookies.is_empty(),
        );
    }
    let (mut headers, mut with_cookies) = log.replay_for(url);
    if let Some(learned) = &endpoint.learned {
        for header in &learned.headers {
            if !headers
                .iter()
                .any(|h| h.name.eq_ignore_ascii_case(&header.name))
            {
                headers.push(header.clone());
            }
        }
        with_cookies |= learned.with_cookies;
    }
    // Headers borrowed from another call say nothing about this one's body; the
    // description does.
    if let Some(body) = endpoint.documented.as_ref().and_then(|d| d.body.as_ref()) {
        headers.retain(|h| !h.name.eq_ignore_ascii_case("content-type"));
        headers.push(ReplayHeader {
            name: "Content-Type".to_owned(),
            value: body.clone(),
            cookie: None,
            quoted: false,
        });
    }
    (headers, with_cookies)
}

/// Resolves cookie-sourced header values against the current cookie jar.
pub fn resolve_headers(
    headers: &[ReplayHeader],
    jar: &[(String, String)],
) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|h| {
            let current = h
                .cookie
                .as_ref()
                .and_then(|name| jar.iter().find(|(n, _)| n == name))
                .map(|(_, v)| {
                    // The page sends the cookie's value without its quotes (LinkedIn's
                    // JSESSIONID is `"ajax:123"`, its csrf-token `ajax:123`).
                    if h.quoted {
                        v.clone()
                    } else {
                        v.trim_matches('"').to_owned()
                    }
                });
            // Headers learned in an earlier session carry no value of their own: without
            // their cookie there is nothing to send.
            match current {
                Some(value) => Some((h.name.clone(), value)),
                None if h.cookie.is_some() && h.value.is_empty() => None,
                None => Some((h.name.clone(), h.value.clone())),
            }
        })
        .collect()
}

/// The URL and body for a call to `endpoint`, from its last sample and the overrides.
/// Errors name what the agent must provide.
pub fn plan(
    endpoint: &Endpoint,
    sample: Option<&Sample>,
    overrides: &Overrides,
) -> Result<(String, Option<String>), String> {
    let base = match sample {
        Some(sample) => Url::parse(&sample.url).map_err(|e| e.to_string())?,
        None => Url::parse(&endpoint.template.origin).map_err(|e| e.to_string())?,
    };
    let sample_segments: Vec<String> = base
        .path_segments()
        .map(|s| s.map(str::to_owned).collect())
        .unwrap_or_default();
    let mut path = Vec::with_capacity(endpoint.template.segments.len());
    for (i, segment) in endpoint.template.segments.iter().enumerate() {
        match segment {
            Segment::Literal(literal) => path.push(literal.clone()),
            Segment::Param(name) => match overrides.path.get(name) {
                Some(value) => path.push(encode_segment(value)),
                None if sample.is_some() => {
                    path.push(sample_segments.get(i).cloned().unwrap_or_default())
                }
                None => {
                    return Err(format!(
                        "path parameter {name:?} is required (path: {{\"{name}\": ...}})"
                    ));
                }
            },
        }
    }
    for name in overrides.path.keys() {
        if !endpoint.template.params().any(|p| p == name) {
            let known: Vec<&str> = endpoint.template.params().collect();
            return Err(format!(
                "endpoint {} has no path parameter {name:?} (it has: {known:?})",
                endpoint.id
            ));
        }
    }
    let mut url = base.clone();
    url.set_path(&format!("/{}", path.join("/")));
    // The query is kept exactly as the page encoded it: re-serializing would
    // percent-encode characters some servers parse literally (LinkedIn's Rest.li
    // `variables=(start:0,count:10)` is rejected as `%28start%3A0...`).
    let mut query: Vec<String> = match (sample, base.query()) {
        (Some(_), Some(raw)) => raw
            .split('&')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect(),
        (Some(_), None) => Vec::new(),
        (None, _) => endpoint
            .template
            .selectors
            .iter()
            .map(|(k, v)| format!("{}={}", encode_query(k), encode_query(v)))
            .collect(),
    };
    for (key, value) in &overrides.query {
        query.retain(|pair| raw_key(pair) != *key);
        if let Some(value) = value {
            query.push(format!("{}={}", encode_query(key), encode_query(value)));
        }
    }
    url.set_query((!query.is_empty()).then(|| query.join("&")).as_deref());
    let body = match &overrides.body {
        Some(Value::String(text)) => Some(text.clone()),
        Some(value) => Some(value.to_string()),
        None => sample.and_then(|s| s.body.clone()),
    };
    Ok((url.to_string(), body))
}

/// The decoded key of a raw `key=value` query pair.
fn raw_key(pair: &str) -> String {
    let key = pair.split('=').next().unwrap_or_default();
    url::form_urlencoded::parse(format!("{key}=").as_bytes())
        .next()
        .map(|(k, _)| k.into_owned())
        .unwrap_or_default()
}

/// Encodes a query key or value minimally: only what would end or corrupt the pair is
/// escaped, so structured values like `(start:0,count:10)` stay literal as pages send
/// them. Values the agent copied already encoded (containing `%XX`) are kept as is.
fn encode_query(value: &str) -> String {
    let already_encoded = value
        .as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit());
    if already_encoded {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'&' | b'=' | b'#' | b'+' | b'%' | b'"' | b'<' | b'>' | b'`' | b'{' | b'}' | b'|'
            | b'\\' | b'^' => {
                out.push_str(&format!("%{byte:02X}"));
            }
            0..=0x20 | 0x7f.. => out.push_str(&format!("%{byte:02X}")),
            _ => out.push(byte as char),
        }
    }
    out
}

/// Path segments are taken verbatim when already percent-encoded (copied from a URL),
/// and encoded otherwise.
fn encode_segment(value: &str) -> String {
    if value.contains('%') {
        return value.to_owned();
    }
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// JavaScript that performs `request` with `fetch` and returns what came back.
pub fn fetch_script(request: &ApiRequest) -> String {
    let headers: serde_json::Map<String, Value> = request
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let init = json!({
        "method": request.method,
        "headers": headers,
        "body": request.body,
        "credentials": if request.with_cookies { "include" } else { "same-origin" },
    });
    format!(
        r#"(async () => {{
  const init = {init};
  if (init.body === null) delete init.body;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), {FETCH_TIMEOUT_MS});
  try {{
    const r = await fetch({url}, {{ ...init, signal: controller.signal }});
    const text = await r.text();
    return {{ status: r.status, statusText: r.statusText, url: r.url, redirected: r.redirected,
      contentType: r.headers.get('content-type') || '', length: text.length,
      body: text.slice(0, {MAX_FETCHED_CHARS}) }};
  }} catch (e) {{
    return {{ error: String(e && e.message || e) }};
  }} finally {{
    clearTimeout(timer);
  }}
}})()"#,
        url = Value::String(request.url.clone()),
    )
}

/// What `fetch_script` returns.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApiResponse {
    pub status: u16,
    pub status_text: String,
    pub url: String,
    pub redirected: bool,
    pub content_type: String,
    pub length: usize,
    pub body: String,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    fn endpoint(url: &str, body: Option<&str>) -> (Endpoint, Sample) {
        let catalog = Catalog::default();
        let id = catalog.classify("POST", url, body, &[], url).unwrap();
        let sample = Sample {
            request_id: "1".into(),
            url: url.into(),
            headers: Vec::new(),
            body: body.map(str::to_owned),
            cookies: Vec::new(),
        };
        (catalog.get(&id).unwrap(), sample)
    }

    #[test]
    fn server_actions_are_endpoints_of_their_own_and_replay_their_own_id() {
        let catalog = Catalog::default();
        let page = "https://app.example.com/invitations";
        let action = |id: &str| vec![("Next-Action".to_owned(), id.to_owned())];
        let accept = catalog
            .classify("POST", page, Some("[1]"), &action("7f3a"), page)
            .unwrap();
        let ignore = catalog
            .classify("POST", page, Some("[1]"), &action("9c0d"), page)
            .unwrap();
        let plain = catalog
            .classify("POST", page, Some("[1]"), &[], page)
            .unwrap();
        assert_ne!(accept, ignore);
        assert_ne!(accept, plain);
        let accept = catalog.get(&accept).unwrap();
        assert!(
            accept
                .template
                .to_string()
                .ends_with("(server action 7f3a)")
        );

        // Headers borrowed from another action's request carry its id: replaced.
        let borrowed = replay_headers(
            &[
                ("Next-Action".to_owned(), "9c0d".to_owned()),
                ("Accept".to_owned(), "text/x-component".to_owned()),
            ],
            &[],
        );
        let headers = with_own_action(borrowed.clone(), &accept);
        let ids: Vec<&str> = headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("next-action"))
            .map(|h| h.value.as_str())
            .collect();
        assert_eq!(ids, ["7f3a"]);
        // A plain endpoint never sends an action id.
        let plain = catalog.get(&plain).unwrap();
        assert!(
            with_own_action(borrowed, &plain)
                .iter()
                .all(|h| !h.name.eq_ignore_ascii_case("next-action"))
        );
    }

    #[test]
    fn replays_the_sample_with_overrides() {
        let (e, s) = endpoint(
            "https://www.linkedin.com/voyager/api/relationships/invitations/7123?action=accept&x=1",
            Some(r#"{"invitationId":"7123"}"#),
        );
        let mut o = Overrides::default();
        assert_eq!(plan(&e, Some(&s), &o).unwrap().0, s.url);
        o.path.insert("invitations_id".into(), "999".into());
        o.query.insert("x".into(), None);
        o.body = Some(json!({"invitationId": "999"}));
        let (url, body) = plan(&e, Some(&s), &o).unwrap();
        assert_eq!(
            url,
            "https://www.linkedin.com/voyager/api/relationships/invitations/999?action=accept"
        );
        assert_eq!(body.unwrap(), r#"{"invitationId":"999"}"#);
        o.path.insert("nope".into(), "1".into());
        assert!(
            plan(&e, Some(&s), &o)
                .unwrap_err()
                .contains("no path parameter")
        );
    }

    #[test]
    fn queries_keep_the_pages_encoding() {
        let (e, s) = endpoint(
            "https://www.linkedin.com/voyager/api/graphql?queryId=x.1&variables=(mailboxUrn:urn%3Ali%3A1)",
            None,
        );
        assert_eq!(plan(&e, Some(&s), &Overrides::default()).unwrap().0, s.url);
        let mut o = Overrides::default();
        o.query
            .insert("variables".into(), Some("(start:10,count:5)".into()));
        assert_eq!(
            plan(&e, Some(&s), &o).unwrap().0,
            "https://www.linkedin.com/voyager/api/graphql?queryId=x.1&variables=(start:10,count:5)"
        );
        o.query.insert("variables".into(), Some("a b&c".into()));
        assert!(
            plan(&e, Some(&s), &o)
                .unwrap()
                .0
                .ends_with("variables=a%20b%26c")
        );
    }

    #[test]
    fn without_a_sample_params_are_required() {
        let (e, _) = endpoint("https://x.com/api/users/42", None);
        assert!(
            plan(&e, None, &Overrides::default())
                .unwrap_err()
                .contains("users_id")
        );
        let mut o = Overrides::default();
        o.path.insert("users_id".into(), "a b".into());
        assert_eq!(
            plan(&e, None, &o).unwrap().0,
            "https://x.com/api/users/a%20b"
        );
    }

    #[test]
    fn csrf_headers_follow_their_cookie() {
        let headers = vec![
            ("csrf-token".to_owned(), "ajax:1234567".to_owned()),
            ("x-stream".to_owned(), "true".to_owned()),
            ("Accept".to_owned(), "application/json".to_owned()),
            ("User-Agent".to_owned(), "x".to_owned()),
            ("sec-ch-ua".to_owned(), "x".to_owned()),
        ];
        let sent = vec![
            ("JSESSIONID".to_owned(), "\"ajax:1234567\"".to_owned()),
            ("liap".to_owned(), "true".to_owned()),
        ];
        let replay = replay_headers(&headers, &sent);
        assert_eq!(replay.len(), 3);
        assert_eq!(replay[0].cookie.as_deref(), Some("JSESSIONID"));
        assert_eq!(replay[1].cookie, None, "short values are never traced");
        let jar = vec![
            ("JSESSIONID".to_owned(), "\"ajax:7654321\"".to_owned()),
            ("liap".to_owned(), "false".to_owned()),
        ];
        assert_eq!(
            resolve_headers(&replay, &jar),
            vec![
                ("csrf-token".to_owned(), "ajax:7654321".to_owned()),
                ("x-stream".to_owned(), "true".to_owned()),
                ("Accept".to_owned(), "application/json".to_owned())
            ]
        );
    }
}
