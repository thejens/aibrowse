//! A profile's pinned egress (design.md §5.7): the proxy every request of the profile
//! leaves through, so sites see one stable address for its accounts: the user's home
//! exit (a WireGuard client turned SOCKS proxy, e.g. `wireproxy`), or a static ISP IP.
//!
//! Set as `"egress": "socks5://host:port"` (or `http://`, `https://`) in the profile's
//! `policy.json`, or for every profile without one by `AIBROWSE_EGRESS`. A profile with
//! an egress never browses without it: an exit that can't be reached stops the launch,
//! rather than signing in to the profile's accounts from the machine's own address.

use std::time::Duration;

use url::Url;

use crate::error::{BrowserError, Result};

/// How long the exit gets to accept a connection before the launch gives up.
const REACH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    url: Url,
}

impl Egress {
    pub fn parse(spec: &str) -> Result<Self> {
        let invalid = |why: &str| BrowserError::Profile(format!("egress {spec:?}: {why}"));
        let url = Url::parse(spec.trim()).map_err(|e| invalid(&e.to_string()))?;
        if !matches!(url.scheme(), "socks5" | "socks5h" | "http" | "https") {
            return Err(invalid("use socks5://, http:// or https://"));
        }
        // Chrome takes no proxy credentials on its command line, and answering its
        // proxy-auth challenges isn't built: an exit that needs them would fail every
        // request, so it is refused up front.
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid(
                "a proxy that needs a user name and password isn't supported yet; run one that doesn't (bound to localhost)",
            ));
        }
        if url.host_str().is_none() || url.port_or_known_default().is_none() {
            return Err(invalid("name its host and port"));
        }
        Ok(Self { url })
    }

    /// The profile's egress: `policy.json`'s `egress`, else `AIBROWSE_EGRESS`.
    pub fn for_policy(policy: Option<&serde_json::Value>) -> Result<Option<Self>> {
        let spec = policy
            .and_then(|p| p.get("egress"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .or_else(|| std::env::var("AIBROWSE_EGRESS").ok())
            .filter(|s| !s.trim().is_empty());
        spec.as_deref().map(Self::parse).transpose()
    }

    /// The proxy's address as Chrome takes it. Chrome sends host names through a SOCKS5
    /// proxy, so names resolve at the exit too.
    fn chrome_proxy(&self) -> String {
        let scheme = match self.url.scheme() {
            "socks5h" => "socks5",
            other => other,
        };
        format!(
            "{scheme}://{}:{}",
            self.url.host_str().unwrap_or_default(),
            self.url.port_or_known_default().unwrap_or_default()
        )
    }

    /// The proxy's address for an HTTP client of our own (reqwest), resolving names at
    /// the exit as Chrome does.
    pub fn client_proxy(&self) -> String {
        match self.url.scheme() {
            "socks5" => self.chrome_proxy().replacen("socks5://", "socks5h://", 1),
            _ => self.chrome_proxy(),
        }
    }

    pub(crate) fn chrome_switches(&self) -> Vec<String> {
        vec![
            format!("--proxy-server={}", self.chrome_proxy()),
            // WebRTC would otherwise reach STUN servers over UDP outside the proxy and
            // show sites the machine's own address.
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
        ]
    }

    /// Fails unless the exit accepts a connection.
    pub(crate) async fn ensure_reachable(&self) -> Result<()> {
        let host = self.url.host_str().unwrap_or_default().to_owned();
        let port = self.url.port_or_known_default().unwrap_or_default();
        match tokio::time::timeout(REACH_TIMEOUT, tokio::net::TcpStream::connect((host, port)))
            .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(err)) => Err(self.unreachable(&err.to_string())),
            Err(_) => Err(self.unreachable("no answer")),
        }
    }

    fn unreachable(&self, why: &str) -> BrowserError {
        BrowserError::Profile(format!(
            "the profile's egress {} can't be reached ({why}); not browsing from another address. Start the exit (e.g. its WireGuard proxy) or change \"egress\" in the profile's policy.json",
            self.chrome_proxy()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exits_are_proxies_without_credentials() {
        let socks = Egress::parse("socks5://127.0.0.1:25344").unwrap();
        assert_eq!(
            socks.chrome_switches()[0],
            "--proxy-server=socks5://127.0.0.1:25344"
        );
        assert_eq!(socks.client_proxy(), "socks5h://127.0.0.1:25344");
        assert_eq!(
            Egress::parse("http://exit.example:3128")
                .unwrap()
                .client_proxy(),
            "http://exit.example:3128"
        );
        assert!(Egress::parse("socks5://me:secret@exit.example:1080").is_err());
        assert!(Egress::parse("ftp://exit.example").is_err());
        assert!(Egress::parse("not a url").is_err());
    }

    #[test]
    fn the_policy_names_it() {
        let policy = json!({ "confirm": "off", "egress": "socks5://10.0.0.2:1080" });
        let egress = Egress::for_policy(Some(&policy)).unwrap().unwrap();
        assert_eq!(egress.client_proxy(), "socks5h://10.0.0.2:1080");
        assert!(Egress::for_policy(Some(&json!({ "egress": "bogus" }))).is_err());
    }

    #[tokio::test]
    async fn an_exit_that_isnt_there_stops_the_launch() {
        // A port nothing listens on: bound, then released.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let egress = Egress::parse(&format!("socks5://127.0.0.1:{port}")).unwrap();
        let err = egress.ensure_reachable().await.unwrap_err().to_string();
        assert!(err.contains("not browsing from another address"), "{err}");
    }
}
