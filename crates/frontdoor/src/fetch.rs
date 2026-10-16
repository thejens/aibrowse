//! Fetching what a site publishes for agents: plain HTTP from this machine, outside the
//! browser.
//!
//! The client keeps no cookies and sends no credentials, so a probe is never tied to the
//! user's logged-in session, and it never shows up inside the profile's own browsing as
//! odd requests. It leaves from the same machine, so from the same egress IP as the
//! browser, and presents the browser's real user agent. Every fetch is size-capped and
//! time-boxed: a site can't stall or flood us.

use std::time::Duration;

use reqwest::header::{ACCEPT, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{Client, StatusCode, redirect};
use url::Url;

/// Per-request ceiling, including reading the body.
const TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_REDIRECTS: usize = 5;

/// What came back for a URL.
#[derive(Debug)]
pub enum Outcome {
    Found(Fetched),
    /// The server says there is nothing there (404, 410, other client errors). Reliable
    /// enough to remember.
    Missing,
    /// Timeouts, network errors, server errors: no answer, so not worth remembering long.
    Failed(String),
}

#[derive(Debug)]
pub struct Fetched {
    /// Where the body came from, after redirects.
    pub url: Url,
    pub content_type: Option<String>,
    /// At most the cap passed to `get`.
    pub body: Vec<u8>,
    /// The body was longer than the cap.
    pub truncated: bool,
    /// The full size, when the server said.
    pub content_length: Option<u64>,
    /// `Cache-Control: max-age`, in seconds.
    pub max_age: Option<u64>,
}

impl Fetched {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The server labeled it HTML: an app shell, an error or login page, not a document
    /// for agents.
    pub fn is_html(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|t| t.contains("html"))
    }
}

/// A cookie-less HTTP client presenting the browser's user agent, leaving through the
/// browser's pinned egress when it has one: a site sees one address per profile.
#[derive(Clone)]
pub struct Fetcher {
    client: Client,
}

impl Fetcher {
    /// `proxy` is the profile's egress as a client takes it (`Egress::client_proxy`).
    pub fn new(user_agent: &str, proxy: Option<&str>) -> Self {
        let mut builder = Client::builder();
        if let Some(proxy) = proxy {
            match reqwest::Proxy::all(proxy) {
                Ok(proxy) => builder = builder.proxy(proxy),
                // A pinned profile's probes never go out directly: one that can't use
                // the exit sends nothing (every fetch fails) rather than leak.
                Err(err) => {
                    tracing::warn!(%err, "egress unusable for probes; probes are off");
                    builder = builder.proxy(
                        reqwest::Proxy::all("socks5h://127.0.0.1:9").expect("a proxy address"),
                    );
                }
            }
        }
        let client = builder
            .user_agent(user_agent)
            .redirect(redirect::Policy::limited(MAX_REDIRECTS))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .build()
            .unwrap_or_default();
        Self { client }
    }

    /// GETs `url`, reading at most `cap` bytes of the body.
    pub async fn get(&self, url: &Url, accept: &str, cap: usize) -> Outcome {
        let response = match self
            .client
            .get(url.clone())
            .header(ACCEPT, accept)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => return Outcome::Failed(describe(&err)),
        };
        let status = response.status();
        if status.is_client_error() {
            return Outcome::Missing;
        }
        if status != StatusCode::OK {
            return Outcome::Failed(format!("HTTP {}", status.as_u16()));
        }
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_owned)
        };
        let content_type = header(CONTENT_TYPE).map(|t| t.to_ascii_lowercase());
        let content_length = header(CONTENT_LENGTH).and_then(|l| l.parse().ok());
        let max_age = header(CACHE_CONTROL).and_then(|c| max_age(&c));
        let final_url = response.url().clone();
        let (body, truncated) = match read_capped(response, cap).await {
            Ok(read) => read,
            Err(err) => return Outcome::Failed(describe(&err)),
        };
        Outcome::Found(Fetched {
            url: final_url,
            content_type,
            body,
            truncated,
            content_length,
            max_age,
        })
    }
}

async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> reqwest::Result<(Vec<u8>, bool)> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let room = cap - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

fn describe(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "timed out".into()
    } else if err.is_connect() {
        "could not connect".into()
    } else {
        err.to_string()
    }
}

/// `max-age` from a Cache-Control header; `no-store`/`no-cache` count as zero.
fn max_age(cache_control: &str) -> Option<u64> {
    cache_control.split(',').find_map(|directive| {
        let directive = directive.trim().to_ascii_lowercase();
        if directive == "no-store" || directive == "no-cache" {
            return Some(0);
        }
        directive.strip_prefix("max-age=")?.trim().parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::max_age;

    #[test]
    fn reads_max_age() {
        assert_eq!(max_age("public, max-age=3600"), Some(3600));
        assert_eq!(max_age("no-store"), Some(0));
        assert_eq!(max_age("public"), None);
    }
}
