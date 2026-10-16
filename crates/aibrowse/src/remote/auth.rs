//! Who may use the server over HTTP: a personal bearer token, optionally scoped to
//! profiles (docs/design.md §5.8).
//!
//! The token is an OAuth-shaped credential without the OAuth: clients send it as
//! `Authorization: Bearer <token>` on every request (RFC 6750), and failures answer the
//! way an OAuth 2.1 resource server does (401 with a `WWW-Authenticate: Bearer`
//! challenge). B2 swaps [`Auth::check`] for access-token validation against an
//! authorization server and serves Protected Resource Metadata (RFC 9728); the
//! [`Grant`] it yields, and everything downstream of it, stays the same.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Shorter tokens are refused: the token alone guards a browser holding the user's
/// logins, reachable from the internet. Generated tokens are 64 hex characters.
const MIN_TOKEN_LEN: usize = 32;

/// What a valid token lets its bearer do. Attached to the HTTP request, from where the
/// MCP handler reads it (rmcp hands the request's parts to every call).
#[derive(Debug, Clone, Default)]
pub struct Grant {
    /// The profiles the bearer may use; `None`: all of them.
    profiles: Option<Arc<[String]>>,
}

impl Grant {
    pub fn allows(&self, profile: &str) -> bool {
        self.profiles
            .as_ref()
            .is_none_or(|allowed| allowed.iter().any(|p| p == profile))
    }

    /// The allowed profiles, for telling the agent what it may use instead.
    pub fn profiles(&self) -> Option<&[String]> {
        self.profiles.as_deref()
    }
}

/// Why a request was turned away. Both are a 401: the client must (re)authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// No `Authorization: Bearer` header at all.
    Missing,
    /// A bearer token that isn't ours.
    Invalid,
}

/// The server's credentials: one personal token for now (B1 is single-user).
#[derive(Clone)]
pub struct Auth {
    token: Arc<str>,
    grant: Grant,
}

impl std::fmt::Debug for Auth {
    // The token never reaches logs, not even through a stray `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("token", &"<redacted>")
            .field("grant", &self.grant)
            .finish()
    }
}

/// Where the token came from, for the startup log line.
pub enum Source {
    Env,
    /// Read from, or generated into, this file.
    File {
        path: PathBuf,
        created: bool,
    },
}

impl Auth {
    pub fn new(token: &str, profiles: Option<Vec<String>>) -> anyhow::Result<Self> {
        let token = token.trim();
        if token.len() < MIN_TOKEN_LEN {
            bail!("the token must be at least {MIN_TOKEN_LEN} characters");
        }
        if !token.bytes().all(|b| b.is_ascii_graphic()) {
            bail!("the token must be printable ASCII without spaces");
        }
        Ok(Self {
            token: token.into(),
            grant: Grant {
                profiles: profiles.map(Into::into),
            },
        })
    }

    /// `AIBROWSE_TOKEN`, else the token in `~/.aibrowse/http-token` (generated there,
    /// readable only by the user, on first start). `AIBROWSE_TOKEN_PROFILES=a,b` scopes
    /// it to those profiles.
    pub fn from_env() -> anyhow::Result<(Self, Source)> {
        let profiles = std::env::var("AIBROWSE_TOKEN_PROFILES")
            .ok()
            .map(|v| parse_profiles(&v))
            .filter(|p| !p.is_empty());
        if let Ok(token) = std::env::var("AIBROWSE_TOKEN") {
            let auth = Self::new(&token, profiles).context("AIBROWSE_TOKEN")?;
            return Ok((auth, Source::Env));
        }
        let path = dirs::home_dir()
            .context("cannot determine the home directory for the token file")?
            .join(".aibrowse/http-token");
        let (token, created) = load_or_create(&path)?;
        let auth = Self::new(&token, profiles).with_context(|| path.display().to_string())?;
        Ok((auth, Source::File { path, created }))
    }

    /// The grant behind an `Authorization` header value.
    pub fn check(&self, authorization: Option<&HeaderValue>) -> Result<Grant, Denied> {
        let value = authorization
            .and_then(|v| v.to_str().ok())
            .ok_or(Denied::Missing)?;
        let (scheme, token) = value.trim().split_once(' ').ok_or(Denied::Missing)?;
        // The auth scheme is case-insensitive (RFC 7235 §2.1).
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(Denied::Missing);
        }
        if aibrowse_handoff::same_token(&self.token, token.trim()) {
            Ok(self.grant.clone())
        } else {
            Err(Denied::Invalid)
        }
    }
}

/// `a, b,,c` → `["a", "b", "c"]`.
fn parse_profiles(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Reads the token file, or creates it with a fresh 256-bit token. The file is created
/// with mode 0600 in one step, so the token is never readable by others, not even
/// briefly.
fn load_or_create(path: &Path) -> anyhow::Result<(String, bool)> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::read_to_string(path) {
        Ok(token) => return Ok((token.trim().to_owned(), false)),
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            return Err(err).with_context(|| format!("reading {}", path.display()));
        }
        Err(_) => {}
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let token = format!("{}{}", aibrowse_handoff::token(), aibrowse_handoff::token());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    writeln!(file, "{token}").with_context(|| format!("writing {}", path.display()))?;
    Ok((token, true))
}

/// Axum middleware: lets a request through only with the token, carrying its [`Grant`].
pub async fn require_token(State(auth): State<Auth>, mut request: Request, next: Next) -> Response {
    match auth.check(request.headers().get(header::AUTHORIZATION)) {
        Ok(grant) => {
            request.extensions_mut().insert(grant);
            next.run(request).await
        }
        Err(denied) => unauthorized(denied),
    }
}

/// An RFC 6750 challenge. The body says nothing about the server beyond that.
fn unauthorized(denied: Denied) -> Response {
    let challenge = match denied {
        Denied::Missing => r#"Bearer realm="aibrowse""#,
        Denied::Invalid => r#"Bearer realm="aibrowse", error="invalid_token""#,
    };
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge)],
        "unauthorized\n",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn bearer(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).unwrap()
    }

    #[test]
    fn accepts_only_the_exact_bearer_token() {
        let auth = Auth::new(TOKEN, None).unwrap();
        assert!(
            auth.check(Some(&bearer(&format!("Bearer {TOKEN}"))))
                .is_ok()
        );
        assert!(
            auth.check(Some(&bearer(&format!("bearer {TOKEN}"))))
                .is_ok()
        );
        let wrong = format!("Bearer {}x", &TOKEN[..TOKEN.len() - 1]);
        assert_eq!(
            auth.check(Some(&bearer(&wrong))).unwrap_err(),
            Denied::Invalid
        );
        let longer = format!("Bearer {TOKEN}0");
        assert_eq!(
            auth.check(Some(&bearer(&longer))).unwrap_err(),
            Denied::Invalid
        );
        assert_eq!(
            auth.check(Some(&bearer("Bearer "))).unwrap_err(),
            Denied::Missing
        );
    }

    #[test]
    fn missing_or_other_schemes_are_missing() {
        let auth = Auth::new(TOKEN, None).unwrap();
        assert_eq!(auth.check(None).unwrap_err(), Denied::Missing);
        assert_eq!(
            auth.check(Some(&bearer(TOKEN))).unwrap_err(),
            Denied::Missing
        );
        let basic = format!("Basic {TOKEN}");
        assert_eq!(
            auth.check(Some(&bearer(&basic))).unwrap_err(),
            Denied::Missing
        );
    }

    #[test]
    fn weak_tokens_are_refused() {
        assert!(Auth::new("short", None).is_err());
        assert!(Auth::new(&format!("{TOKEN} spaced"), None).is_err());
        // Surrounding whitespace (a trailing newline in an env file) is not the token.
        assert!(Auth::new(&format!(" {TOKEN}\n"), None).is_ok());
    }

    #[test]
    fn grants_are_scoped_to_profiles() {
        let auth = Auth::new(TOKEN, Some(parse_profiles(" work, ,personal "))).unwrap();
        let grant = auth
            .check(Some(&bearer(&format!("Bearer {TOKEN}"))))
            .unwrap();
        assert!(grant.allows("work"));
        assert!(grant.allows("personal"));
        assert!(!grant.allows("default"));
        assert!(!grant.allows("work2"));
        assert!(Grant::default().allows("anything"));
    }

    #[test]
    fn the_token_file_is_created_private_and_reused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/http-token");
        let (token, created) = load_or_create(&path).unwrap();
        assert!(created);
        assert_eq!(token.len(), 64);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(load_or_create(&path).unwrap(), (token, false));
    }

    #[test]
    fn debug_output_redacts_the_token() {
        let auth = Auth::new(TOKEN, None).unwrap();
        assert!(!format!("{auth:?}").contains(TOKEN));
    }
}
