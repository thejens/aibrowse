//! The MCP client and helpers shared by the live tests: the real binary, spoken to over
//! stdio the way an agent host does.
#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

pub struct Client {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// A server without a site knowledge base or traces, so tests never touch the user's.
    pub fn spawn(profile: &str, headless: bool) -> Self {
        Self::spawn_with(profile, headless, &[])
    }

    /// A server with extra environment, e.g. `AIBROWSE_KB` or `AIBROWSE_TRACES` pointing
    /// at a temporary location.
    pub fn spawn_with(profile: &str, headless: bool, env: &[(&str, &str)]) -> Self {
        Self::launch(Command::new(server_binary()), profile, headless, env)
    }

    /// The hot-reload front (`aibrowse reload`) running the binary at `exe`, so a test
    /// can stand in for `make install` by touching that copy.
    pub fn spawn_reloading(exe: &std::path::Path, profile: &str, headless: bool) -> Self {
        let mut command = Command::new(exe);
        command.arg("reload");
        Self::launch(command, profile, headless, &[])
    }

    fn launch(mut command: Command, profile: &str, headless: bool, env: &[(&str, &str)]) -> Self {
        let mut child = command
            .env("AIBROWSE_KB", "off")
            .env("AIBROWSE_TRACES", "off")
            .envs(env.iter().copied())
            .env("AIBROWSE_PROFILE", profile)
            .env("AIBROWSE_HEADLESS", if headless { "1" } else { "0" })
            .env("AIBROWSE_PACE", "fast")
            .env("AIBROWSE_MUTE", "1")
            .env("AIBROWSE_HANDOFF_OPEN", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Set AIBROWSE_LOG (e.g. "aibrowse_browser=debug") to see the server's logs.
            .stderr(if std::env::var_os("AIBROWSE_LOG").is_some() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .expect("spawn aibrowse");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        client.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
        );
        client.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        client
    }

    pub fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                assert!(message.get("error").is_none(), "{method} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    /// Calls a tool and returns (is_error, all text blocks joined).
    pub fn call(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let texts: Vec<&str> = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect();
        (
            result["isError"].as_bool().unwrap_or(false),
            texts.join("\n"),
        )
    }

    pub fn ok(&mut self, name: &str, arguments: Value) -> String {
        let (err, out) = self.call(name, arguments.clone());
        assert!(!err, "{name} {arguments} failed: {out}");
        out
    }
}

/// The ref that follows `needle` in the outline. A form's signature holds several fields
/// on one line, so the ref is the first one after the needle, not the line's first.
/// The server binary these tests drive: aibrowse's own, or the one
/// `AIBROWSE_TEST_SERVER` names (a crate wrapping aibrowse, sharing these helpers).
fn server_binary() -> String {
    std::env::var("AIBROWSE_TEST_SERVER")
        .ok()
        .or(option_env!("CARGO_BIN_EXE_aibrowse").map(str::to_owned))
        .expect("AIBROWSE_TEST_SERVER, or a test of the aibrowse crate")
}

pub fn find_ref(outline: &str, needle: &str) -> String {
    let at = outline
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in:\n{outline}"));
    let rest = &outline[at..];
    let start = rest
        .match_indices('[')
        .map(|(i, _)| i + 1)
        .find(|&i| rest[i..].starts_with(['e', 'f']))
        .unwrap();
    rest[start..start + rest[start..].find(']').unwrap()].to_owned()
}

pub fn set_policy(profile: &str, policy: &str) {
    let dir = format!(
        "{}/.aibrowse/profiles/{profile}",
        dirs::home_dir().unwrap().display()
    );
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/policy.json"), policy).unwrap();
}

pub fn has_jev_key() -> bool {
    std::env::var("JEV_API_KEY").is_ok()
        || std::fs::read_to_string(format!(
            "{}/.aibrowse/env",
            dirs::home_dir().unwrap().display()
        ))
        .is_ok_and(|env| env.contains("JEV_API_KEY="))
}

pub fn http_get(url: &str) -> String {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = std::net::TcpStream::connect(host).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).unwrap();
    body
}

/// Serves `pages` on localhost, each at its path (the longest matching prefix wins; none
/// is a 404, and JSON bodies are served as JSON), and returns the port. One thread per
/// connection: Chrome opens speculative preconnect
/// sockets that may never send a request, and a serial server would stall behind them.
pub fn serve_pages(pages: &'static [(&'static str, &'static str)]) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body = pages
                    .iter()
                    .filter(|(prefix, _)| path.starts_with(prefix))
                    .max_by_key(|(prefix, _)| prefix.len())
                    .map_or("", |(_, body)| body);
                // JSON documents (API responses, descriptions) are served as JSON, YAML
                // files as YAML.
                let content_type = if body.starts_with(['{', '[']) {
                    "application/json"
                } else if path.ends_with(".yaml") {
                    "application/yaml"
                } else if path.ends_with(".pdf") {
                    "application/pdf"
                } else {
                    "text/html"
                };
                let status = if body.is_empty() {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    port
}
