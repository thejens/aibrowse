//! Durable sessions end to end: idle hibernation closes Chrome and the next call brings
//! the tabs back; a hibernated session survives a server restart. Ignored by default;
//! run with `make test-live`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

mod common;
use common::{Client, find_ref, has_jev_key, set_policy};

const PROFILE: &str = "test-hibernate";

/// A tall page (so a scroll offset is worth restoring) linking to a second one in a new tab.
fn page(title: &str) -> String {
    format!(
        "<!doctype html><title>{title}</title><h1>{title}</h1>\
         <a href=\"/second\" target=\"_blank\">Open second</a>\
         <div style=\"height: 6000px\"></div><p>End</p>"
    )
}

/// Serves the first page at / and the second at /second, one thread per connection
/// (Chrome's speculative preconnects may never send a request).
fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let body = if request.starts_with("GET /second") {
                    page("Second")
                } else {
                    page("First")
                };
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

fn headless() -> bool {
    std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1")
}

fn profile_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join(format!(".aibrowse/profiles/{PROFILE}"))
}

/// The pid of the Chrome running the profile, from Chrome's lock ("<host>-<pid>").
fn chrome_pid() -> u32 {
    let lock = std::fs::read_link(profile_dir().join("chrome/SingletonLock")).unwrap();
    lock.to_string_lossy()
        .rsplit('-')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn journal() -> Option<Value> {
    let bytes = std::fs::read(profile_dir().join("session.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Opens two tabs, scrolls the first, and makes it the active one again.
fn two_tabs(client: &mut Client, port: u16) {
    let outline = client.ok(
        "navigate",
        json!({ "url": format!("http://127.0.0.1:{port}/") }),
    );
    let link = find_ref(&outline, "Open second");
    let opened = client.ok("act", json!({ "action": "click", "ref": link }));
    assert!(opened.contains("new tab"), "{opened}");
    client.ok("tabs", json!({ "switch": 1 }));
    client.ok(
        "evaluate",
        json!({ "expression": "window.scrollTo(0, 1500)" }),
    );
    // A call that moves the session journals it; tabs is one.
    client.ok("tabs", json!({}));
}

fn assert_restored(listing: &str, port: u16) {
    let tabs: Vec<&str> = listing.lines().filter(|l| l.contains(" — ")).collect();
    assert_eq!(tabs.len(), 2, "{listing}");
    assert!(
        tabs[0].ends_with(&format!("127.0.0.1:{port}/ (active)")),
        "{listing}"
    );
    assert!(tabs[1].contains("/second"), "{listing}");
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn idle_session_hibernates_and_resumes_its_tabs() {
    set_policy(PROFILE, r#"{"confirm": "off"}"#);
    let _ = std::fs::remove_file(profile_dir().join("session.json"));
    let port = serve();
    let mut client = Client::spawn_with(PROFILE, headless(), &[("AIBROWSE_IDLE_SECONDS", "3")]);
    two_tabs(&mut client, port);
    let pid = chrome_pid();

    // Idle: Chrome goes away, the server stays.
    let deadline = Instant::now() + Duration::from_secs(60);
    while alive(pid) {
        assert!(Instant::now() < deadline, "Chrome {pid} still running");
        std::thread::sleep(Duration::from_millis(200));
    }
    let record = journal().expect("journal written");
    assert_eq!(record["tabs"].as_array().unwrap().len(), 2, "{record}");
    assert_eq!(record["active"], 0, "{record}");
    assert!(
        record["tabs"][0]["scroll_y"].as_f64().unwrap() >= 1400.0,
        "{record}"
    );

    // The next call resumes: both tabs, the same one active, scrolled back.
    let listing = client.ok("tabs", json!({}));
    assert!(
        listing.starts_with("Note: the browser was hibernated after 3s idle and resumed: 2 tabs restored; refs are new, observe again."),
        "{listing}"
    );
    assert_restored(&listing, port);
    assert_ne!(chrome_pid(), pid);
    let scrolled = client.ok("evaluate", json!({ "expression": "window.scrollY" }));
    assert!(scrolled.parse::<f64>().unwrap() >= 1400.0, "{scrolled}");
    // Told once only.
    let again = client.ok("tabs", json!({}));
    assert!(!again.contains("Note:"), "{again}");

    // Closing for good forgets the tabs.
    client.ok("session_close", json!({}));
    assert!(journal().is_none());
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn hibernated_session_survives_a_server_restart() {
    let profile = "test-hibernate-restart";
    set_policy(profile, r#"{"confirm": "off"}"#);
    let port = serve();
    let mut client = Client::spawn(profile, headless());
    client.ok(
        "navigate",
        json!({ "url": format!("http://127.0.0.1:{port}/second") }),
    );
    let hibernated = client.ok("session_close", json!({ "hibernate": true }));
    assert!(hibernated.starts_with("Session hibernated"), "{hibernated}");
    drop(client);

    // What the reload front does for the worker that replaces a retired one.
    let mut client = Client::spawn_with(profile, headless(), &[("AIBROWSE_RESUME", "1")]);
    let listing = client.ok("tabs", json!({}));
    assert!(
        listing.starts_with(
            "Note: the browser was restarted with the server and resumed: 1 tab restored"
        ),
        "{listing}"
    );
    assert!(
        listing.contains(&format!("127.0.0.1:{port}/second (active)")),
        "{listing}"
    );
    client.ok("session_close", json!({}));
}

/// A resumed session says what the agent was doing: its last goal and how it ended.
#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn a_resumed_session_says_what_the_agent_was_doing() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    let profile = "test-hibernate-doing";
    set_policy(profile, r#"{"confirm": "off"}"#);
    let port = serve();
    let mut client = Client::spawn(profile, headless());
    client.ok(
        "navigate",
        json!({ "url": format!("http://127.0.0.1:{port}/"), "observe": false }),
    );
    client.ok(
        "navigate",
        json!({ "goal": "the page titled Second", "observe": false }),
    );
    client.ok("session_close", json!({ "hibernate": true }));
    drop(client);

    let mut client = Client::spawn_with(profile, headless(), &[("AIBROWSE_RESUME", "1")]);
    let listing = client.ok("tabs", json!({}));
    assert!(
        listing.contains("Your last navigator goal: \"the page titled Second\" (reached)."),
        "{listing}"
    );
    client.ok("session_close", json!({}));
}

/// The reload front retires its worker on a rebuild by hibernating the session; the
/// new worker resumes it, in the profile the agent opened rather than its default.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_reload_resumes_the_session_in_the_new_worker() {
    let profile = "test-hibernate-reload";
    set_policy(profile, r#"{"confirm": "off"}"#);
    let port = serve();
    let bin = tempfile::tempdir().unwrap();
    let exe = bin.path().join("aibrowse");
    std::fs::copy(env!("CARGO_BIN_EXE_aibrowse"), &exe).unwrap();
    let mut client = Client::spawn_reloading(&exe, "test-hibernate-reload-unused", headless());
    client.ok("session_open", json!({ "profile": profile }));
    two_tabs(&mut client, port);

    // What `make install` does: a new file renamed over the old one. (Writing to the
    // running binary in place would get the next process killed by code signing.)
    let staged = bin.path().join("aibrowse.new");
    std::fs::copy(env!("CARGO_BIN_EXE_aibrowse"), &staged).unwrap();
    let rebuilt = std::time::SystemTime::now() + Duration::from_secs(2);
    std::fs::File::options()
        .write(true)
        .open(&staged)
        .unwrap()
        .set_modified(rebuilt)
        .unwrap();
    std::fs::rename(&staged, &exe).unwrap();
    let listing = client.ok("tabs", json!({}));
    assert!(
        listing.starts_with(
            "aibrowse was rebuilt and has reloaded.\nNote: the browser was restarted with the server and resumed: 2 tabs restored"
        ),
        "{listing}"
    );
    assert_restored(&listing, port);
    client.ok("session_close", json!({}));
}

/// Headless Chrome often outlives `Browser.close` by ~10s and is killed after 5s at
/// `session_close`; what a page stored just before must still be there next time.
#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn storage_written_just_before_close_survives() {
    let profile = "test-hibernate-flush";
    set_policy(profile, r#"{"confirm": "off"}"#);
    let port = serve();
    let url = format!("http://127.0.0.1:{port}/");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    let mut client = Client::spawn(profile, headless());
    client.ok("navigate", json!({ "url": url }));
    let store = format!(
        "document.cookie = 'stamp={stamp}; max-age=86400; path=/'; localStorage.setItem('stamp', '{stamp}'); 'stored'"
    );
    client.ok("evaluate", json!({ "expression": store, "world": "main" }));
    client.ok("session_close", json!({}));
    drop(client);

    let mut client = Client::spawn(profile, headless());
    client.ok("navigate", json!({ "url": url }));
    let read = "document.cookie + ' ' + localStorage.getItem('stamp')";
    let stored = client.ok("evaluate", json!({ "expression": read, "world": "main" }));
    // Other cookies may be left from earlier runs; this run's must be among them.
    assert!(stored.contains(&format!("stamp={stamp}")), "{stored}");
    assert!(stored.ends_with(&format!(" {stamp}")), "{stored}");
    client.ok("session_close", json!({}));
}
