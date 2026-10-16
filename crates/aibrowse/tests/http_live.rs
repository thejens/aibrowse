//! `aibrowse serve --http` end to end: the bearer token guards everything but /healthz,
//! rmcp's streamable HTTP client lists the tools and drives the browser with it, a token
//! scoped to profiles keeps others out, and live-view links start with
//! `AIBROWSE_PUBLIC_URL`. Ignored by default; run with `make test-live`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::json;

const PROFILE: &str = "test-http";
const TOKEN: &str = "test-http-0123456789abcdef0123456789abcdef";
const PUBLIC_URL: &str = "https://browse.example.test";

fn headless() -> bool {
    std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Serves one small page on every path, one thread per connection.
fn serve_page() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = "<!doctype html><title>Remote</title><h1>Served over HTTP</h1>\
                            <a href=\"/next\">Next page</a>";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    port
}

/// The server process; terminated (and its Chrome closed) even when the test fails.
struct Server(Child);

impl Server {
    /// Asks the server to stop as a container runtime would, and waits for it.
    fn terminate(&mut self) -> Duration {
        let started = Instant::now();
        Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .unwrap();
        while self.0.try_wait().unwrap().is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "server did not exit on SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        started.elapsed()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.terminate();
        }
    }
}

fn spawn_server(port: u16, live_view_port: u16) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_aibrowse"))
        .args(["serve", "--http", &format!("127.0.0.1:{port}")])
        .env("AIBROWSE_TOKEN", TOKEN)
        .env("AIBROWSE_TOKEN_PROFILES", PROFILE)
        .env("AIBROWSE_PUBLIC_URL", format!("{PUBLIC_URL}/"))
        .env(
            "AIBROWSE_HANDOFF_BIND",
            format!("127.0.0.1:{live_view_port}"),
        )
        .env("AIBROWSE_KB", "off")
        .env("AIBROWSE_TRACES", "off")
        .env("AIBROWSE_PROFILE", PROFILE)
        .env("AIBROWSE_HEADLESS", if headless() { "1" } else { "0" })
        .env("AIBROWSE_PACE", "fast")
        .env("AIBROWSE_HANDOFF_OPEN", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if std::env::var_os("AIBROWSE_LOG").is_some() {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .spawn()
        .expect("spawn aibrowse serve");
    let server = Server(child);
    let deadline = Instant::now() + Duration::from_secs(20);
    while request(port, "GET", "/healthz", None, "").0 != 200 {
        assert!(Instant::now() < deadline, "server never became healthy");
        std::thread::sleep(Duration::from_millis(100));
    }
    server
}

/// A raw HTTP/1.1 exchange: (status, response head and body). An unreachable server is
/// status 0.
fn request(port: u16, method: &str, path: &str, bearer: Option<&str>, body: &str) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let auth = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, response)
}

fn texts(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn args(value: serde_json::Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

#[test]
#[ignore = "launches Chrome; run with make test-live"]
fn serves_mcp_over_http_behind_a_token() {
    let page = serve_page();
    let (port, live_view_port) = (free_port(), free_port());
    let mut server = spawn_server(port, live_view_port);

    // Health says nothing but that it's up.
    let (status, health) = request(port, "GET", "/healthz", None, "");
    assert_eq!(status, 200);
    assert!(health.ends_with("ok\n"), "{health}");

    // Everything else needs the token: the MCP endpoint, and paths that don't exist.
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" } }
    })
    .to_string();
    let (status, denied) = request(port, "POST", "/mcp", None, &initialize);
    assert_eq!(status, 401, "{denied}");
    assert!(
        denied.contains("www-authenticate: Bearer realm=\"aibrowse\""),
        "{denied}"
    );
    let wrong = format!("{}x", &TOKEN[..TOKEN.len() - 1]);
    let (status, denied) = request(port, "POST", "/mcp", Some(&wrong), &initialize);
    assert_eq!(status, 401);
    assert!(denied.contains("error=\"invalid_token\""), "{denied}");
    assert!(!denied.contains(TOKEN), "the response echoes the token");
    assert_eq!(request(port, "GET", "/anything", None, "").0, 401);
    assert_eq!(request(port, "GET", "/anything", Some(TOKEN), "").0, 404);
    assert_eq!(
        request(port, "POST", "/mcp", Some(TOKEN), &initialize).0,
        200
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://127.0.0.1:{port}/mcp"))
                .auth_header(TOKEN),
        );
        let client = ().serve(transport).await.expect("MCP handshake over HTTP");

        let tools = client.list_all_tools().await.unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        for expected in ["navigate", "act", "observe", "request_handoff", "watch"] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }

        // The token is scoped to PROFILE: another one is refused before Chrome starts.
        let other = client
            .call_tool(
                CallToolRequestParams::new("session_open")
                    .with_arguments(args(json!({ "profile": "test-http-other" }))),
            )
            .await
            .unwrap();
        assert_eq!(other.is_error, Some(true));
        assert!(
            texts(&other).contains("limited to: test-http"),
            "{}",
            texts(&other)
        );

        let navigated = client
            .call_tool(
                CallToolRequestParams::new("navigate")
                    .with_arguments(args(json!({ "url": format!("http://127.0.0.1:{page}/") }))),
            )
            .await
            .unwrap();
        let outline = texts(&navigated);
        assert_ne!(navigated.is_error, Some(true), "{outline}");
        assert!(outline.contains("Served over HTTP"), "{outline}");
        assert!(outline.contains("Next page"), "{outline}");

        // A second MCP session shares the same browser session (one per process).
        let second = ()
            .serve(StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(format!(
                    "http://127.0.0.1:{port}/mcp"
                ))
                .auth_header(TOKEN),
            ))
            .await
            .unwrap();
        let tabs = second
            .call_tool(CallToolRequestParams::new("tabs"))
            .await
            .unwrap();
        assert!(texts(&tabs).contains("Remote"), "{}", texts(&tabs));

        // The live view link is built from the public URL, for a reverse proxy to route
        // to the live view port.
        let watch = client
            .call_tool(
                CallToolRequestParams::new("watch").with_arguments(args(json!({ "open": false }))),
            )
            .await
            .unwrap();
        let message = texts(&watch);
        let link = message
            .split_whitespace()
            .find(|w| w.starts_with(PUBLIC_URL))
            .unwrap_or_else(|| panic!("no {PUBLIC_URL} link in: {message}"));
        let path = link.strip_prefix(PUBLIC_URL).unwrap();
        assert!(path.starts_with("/w/"), "{link}");
        let (status, _) = request(live_view_port, "GET", path, None, "");
        assert_eq!(status, 200, "the live view does not serve {path}");

        let _ = second.cancel().await;
        let _ = client.cancel().await;
    });

    // SIGTERM closes Chrome cleanly within a container's stop timeout.
    let took = server.terminate();
    assert!(took < Duration::from_secs(10), "shutdown took {took:?}");
}
