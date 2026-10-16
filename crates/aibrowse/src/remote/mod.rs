//! `aibrowse serve --http <addr>`: the MCP server over streamable HTTP, for agents on
//! other machines (docs/server.md). Same tools as stdio, behind a personal bearer token.
//!
//! Routes:
//!   POST/GET/DELETE /mcp   the MCP endpoint (rmcp's streamable HTTP transport)
//!   GET /healthz           liveness, unauthenticated; says "ok" and nothing else
//!   anything else          401 without the token, 404 with it
//!
//! TLS is the reverse proxy's job (docs/server.md); this listens on plain HTTP.

mod auth;
mod scope;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use axum::Router;
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use crate::server::Aibrowse;
use auth::{Auth, Source};

/// Where `serve` listens without `--http` or `AIBROWSE_HTTP_BIND`: this machine only.
const DEFAULT_BIND: &str = "127.0.0.1:8790";

/// How long open MCP streams get to wind down on shutdown before the browser is closed
/// regardless: the profile must be flushed before a container's stop timeout.
const DRAIN: Duration = Duration::from_secs(5);

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let bind = bind_address(args)?;
    let (auth, source) = Auth::from_env()?;
    match source {
        Source::Env => tracing::info!("MCP token from AIBROWSE_TOKEN"),
        Source::File { path, created } => tracing::info!(
            "MCP token {} {}; clients send it as \"Authorization: Bearer <token>\"",
            if created {
                "generated into"
            } else {
                "read from"
            },
            path.display(),
        ),
    }
    crate::respond::serve_remotely();
    let handler = Aibrowse::new();
    handler.watch_idle();

    let stop = CancellationToken::new();
    let config = StreamableHttpServerConfig::default().with_cancellation_token(stop.child_token());
    let config = match allowed_hosts(bind)? {
        Some(hosts) => config.with_allowed_hosts(hosts),
        None => config.disable_allowed_hosts(),
    };
    // Every MCP session (and every request of a stateless 2026-07-28 client) is served
    // by a clone of this one handler. Clones share all state through `Arc`s: one browser
    // session per process, whichever client calls, with tool calls serialized by the
    // session lock. That is the B1 model (one user, maybe several agents); per-tenant
    // sessions are B2.
    let mcp = StreamableHttpService::new(
        {
            let handler = handler.clone();
            move || Ok(handler.clone())
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let app = Router::new()
        .route_service("/mcp", mcp)
        .fallback(|| async { StatusCode::NOT_FOUND })
        // Everything above, the fallback included, needs the token.
        .layer(middleware::from_fn_with_state(auth, auth::require_token))
        .route("/healthz", get(|| async { "ok\n" }));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!("aibrowse MCP on http://{}/mcp", listener.local_addr()?);
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let mut serving = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = drain_rx.await;
            })
            .into_future(),
    );
    tokio::select! {
        result = &mut serving => {
            handler.shutdown().await;
            return result.context("HTTP server task")?.context("HTTP server");
        }
        () = crate::termination() => {}
    }
    // Stop taking requests and end the MCP sessions' streams, then close Chrome cleanly
    // so the profile is flushed to disk.
    stop.cancel();
    let _ = drain_tx.send(());
    let _ = tokio::time::timeout(DRAIN, serving).await;
    handler.shutdown().await;
    Ok(())
}

/// `--http <addr>`, else `AIBROWSE_HTTP_BIND`, else localhost.
fn bind_address(args: &[String]) -> anyhow::Result<SocketAddr> {
    let mut chosen = std::env::var("AIBROWSE_HTTP_BIND").ok();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--http" => chosen = Some(iter.next().context("--http needs an address")?.clone()),
            other => bail!("unknown argument {other:?}; usage: aibrowse serve [--http ADDR]"),
        }
    }
    let address = chosen.unwrap_or_else(|| DEFAULT_BIND.into());
    address
        .parse()
        .with_context(|| format!("{address:?} is not an address like 127.0.0.1:8790"))
}

/// The `Host` values the MCP endpoint answers to (DNS-rebinding protection; `None`:
/// any). `AIBROWSE_ALLOWED_HOSTS=a,b` (or `*`) sets them; by default loopback names, the
/// bind address when it is a specific one, and the host of `AIBROWSE_PUBLIC_URL`, the
/// name a reverse proxy forwards.
fn allowed_hosts(bind: SocketAddr) -> anyhow::Result<Option<Vec<String>>> {
    if let Ok(list) = std::env::var("AIBROWSE_ALLOWED_HOSTS") {
        let hosts: Vec<String> = list
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_owned)
            .collect();
        return Ok((!hosts.iter().any(|h| h == "*")).then_some(hosts));
    }
    let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"].map(Into::into).into();
    if !bind.ip().is_unspecified() && !bind.ip().is_loopback() {
        hosts.push(bind.ip().to_string());
    }
    if let Ok(public) = std::env::var("AIBROWSE_PUBLIC_URL") {
        let url = url::Url::parse(public.trim())
            .with_context(|| format!("AIBROWSE_PUBLIC_URL {public:?} is not a URL"))?;
        if let Some(host) = url.host_str() {
            hosts.push(
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_owned(),
            );
        }
    }
    Ok(Some(hosts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_comes_from_the_flag() {
        let args = ["--http".to_owned(), "0.0.0.0:9000".to_owned()];
        assert_eq!(
            bind_address(&args).unwrap(),
            "0.0.0.0:9000".parse().unwrap()
        );
        assert!(bind_address(&["--http".to_owned()]).is_err());
        assert!(bind_address(&["--port".to_owned()]).is_err());
    }
}
