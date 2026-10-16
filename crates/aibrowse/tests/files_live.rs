//! End-to-end file uploads (behind approval) and downloads. Ignored by default; run with
//! `make test-live`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;

mod common;
use common::{Client, find_ref, http_get, set_policy};

/// A form with a plain file input, a styled "Choose photos" button that opens a hidden
/// multi-file input, and a download link. The page shows what each input holds and
/// which events the selection fired.
const PAGE: &str = r#"<!doctype html><title>Files</title><h1>Files</h1>
<form method="post" action="/submit" enctype="multipart/form-data">
<label>Attachment <input type="file" id="direct" name="doc"></label>
<button type="submit">Submit</button></form>
<p id="direct-out"></p>
<button type="button" id="chooser">Choose photos</button>
<input type="file" id="photos" multiple hidden>
<p id="photos-out"></p>
<p id="events"></p>
<a href="/download">Download report</a>
<script>
for (const [input, out] of [[direct, 'direct-out'], [photos, 'photos-out']]) {
  for (const type of ['input', 'change']) {
    input.addEventListener(type, () => { events.textContent += input.id + ':' + type + ' '; });
  }
  input.addEventListener('change', () => {
    document.getElementById(out).textContent = [...input.files].map((f) => f.name + '=' + f.size).join(',');
  });
}
chooser.onclick = () => photos.click();
</script>"#;

/// What the download link serves: a hostile suggested name that tries to escape the
/// downloads folder.
const REPORT: &str = "quarter,revenue\nQ3,1200\n";

fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        data.extend_from_slice(&buf[..n]);
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        data.extend_from_slice(&buf[..n]);
    }
    let line = head.lines().next().unwrap_or_default().to_owned();
    Some((line, data[head_end..head_end + length].to_vec()))
}

/// Serves the page; returns the port and the bodies of form submissions received.
fn serve() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::clone(&received);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let received = Arc::clone(&state);
            std::thread::spawn(move || {
                let Some((line, body)) = read_request(&mut stream) else {
                    return;
                };
                let (headers, body) = if line.starts_with("POST /submit") {
                    received
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&body).to_string());
                    (
                        "content-type: text/html\r\n".to_owned(),
                        "<!doctype html><title>Received</title><h1>Received</h1>".to_owned(),
                    )
                } else if line.starts_with("GET /download") {
                    (
                        "content-type: application/octet-stream\r\ncontent-disposition: attachment; filename=\"../../.hidden report.csv\"\r\n"
                            .to_owned(),
                        REPORT.to_owned(),
                    )
                } else {
                    ("content-type: text/html\r\n".to_owned(), PAGE.to_owned())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (port, received)
}

/// Files to upload, in a fresh temporary folder.
fn fixtures(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aibrowse-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "hello upload").unwrap();
    std::fs::write(dir.join("a.png"), [0u8; 300]).unwrap();
    std::fs::write(dir.join("b.png"), [1u8; 2048]).unwrap();
    dir
}

fn path(dir: &std::path::Path, name: &str) -> String {
    dir.join(name).display().to_string()
}

fn eval(c: &mut Client, expression: &str) -> String {
    c.ok("evaluate", json!({ "expression": expression }))
}

fn headless() -> bool {
    std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1")
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn uploads_need_confirmation_and_downloads_are_reported() {
    let profile = "test-files";
    // Uploads are gated even when the profile lets the agent act alone.
    set_policy(profile, r#"{"confirm": "off"}"#);
    let downloads = dirs::home_dir()
        .unwrap()
        .join(".aibrowse/downloads")
        .join(profile);
    let _ = std::fs::remove_dir_all(&downloads);
    let files = fixtures("files");
    let (port, received) = serve();
    let mut c = Client::spawn(profile, headless());
    let outline = c.ok(
        "session_open",
        json!({ "url": format!("http://localhost:{port}/"), "attended": false }),
    );
    let input = find_ref(&outline, "Attachment");
    let chooser = find_ref(&outline, "Choose photos");

    // Unattended: the agent must take responsibility explicitly.
    let upload = json!({ "action": "upload", "ref": input, "files": [path(&files, "notes.txt")] });
    let out = c.ok("act", upload.clone());
    assert!(out.contains("no user is present"), "{out}");
    assert!(eval(&mut c, "document.getElementById('direct-out').textContent").is_empty());

    // Obvious secrets are refused, confirmed or not; so are relative paths.
    let home = dirs::home_dir().unwrap().display().to_string();
    for (secret, why) in [
        (format!("{home}/.ssh/id_ed25519"), "refusing"),
        (format!("{home}/.aibrowse/env"), "refusing"),
        ("notes.txt".to_owned(), "absolute"),
    ] {
        let (err, out) = c.call(
            "act",
            json!({ "action": "upload", "ref": input, "files": [secret], "confirm": true }),
        );
        assert!(err && out.contains(why), "{secret}: {out}");
    }

    // A file input: the server receives the file, and the page saw a real selection.
    let mut confirmed = upload.clone();
    confirmed["confirm"] = json!(true);
    let out = c.ok("act", confirmed);
    assert!(out.contains("chose \"notes.txt\" (12 B)"), "{out}");
    assert_eq!(
        eval(&mut c, "document.getElementById('direct-out').textContent"),
        "notes.txt=12"
    );
    assert!(
        eval(&mut c, "document.getElementById('events').textContent")
            .contains("direct:input direct:change"),
        "a selection fires input and change"
    );

    // A button that opens a chooser (for a hidden, multi-file input).
    let out = c.ok(
        "act",
        json!({ "action": "upload", "ref": chooser, "confirm": true,
                "files": [path(&files, "a.png"), path(&files, "b.png")] }),
    );
    assert!(
        out.contains("\"a.png\" (300 B), \"b.png\" (2.0 KB)"),
        "{out}"
    );
    assert_eq!(
        eval(&mut c, "document.getElementById('photos-out').textContent"),
        "a.png=300,b.png=2048"
    );
    // A plain click on it tells the agent how to answer the chooser.
    let out = c.ok("act", json!({ "action": "click", "ref": chooser }));
    assert!(out.contains("opened a file chooser"), "{out}");

    // A download: saved under a safe name in the profile's downloads folder.
    let outline = c.ok("observe", json!({}));
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Download report") }),
    );
    // Chrome cleans up the hostile name: the file lands directly in the folder, and
    // isn't hidden.
    let saved_to = |out: &str| {
        PathBuf::from(
            out.lines()
                .find_map(|l| l.split_once("report.csv\" (24 B) to "))
                .unwrap_or_else(|| panic!("the action reports the finished download: {out}"))
                .1,
        )
    };
    let saved = saved_to(&out);
    assert_eq!(saved.parent(), Some(downloads.as_path()));
    let name = saved.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        !name.starts_with('.') && name.ends_with("report.csv"),
        "{name}"
    );
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), REPORT);
    // Downloading it again never overwrites the first file.
    let out = c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Download report") }),
    );
    let numbered = saved.with_file_name(name.replace("report.csv", "report (1).csv"));
    assert!(
        out.contains(&format!("(24 B) to {}", numbered.display())),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), REPORT);
    let listing = c.ok("downloads", json!({}));
    assert!(
        listing.contains(&name) && listing.lines().count() == 2,
        "{listing}"
    );

    // Submitting the form sends the uploaded file to the site.
    let outline = c.ok("observe", json!({}));
    c.ok(
        "act",
        json!({ "action": "click", "ref": find_ref(&outline, "Submit") }),
    );
    let bodies = received.lock().unwrap().clone();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("filename=\"notes.txt\"") && b.contains("hello upload")),
        "the server received the file: {bodies:?}"
    );
    c.ok("session_close", json!({}));
    let _ = std::fs::remove_dir_all(files);
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn uploads_wait_for_the_users_approval() {
    let profile = "test-files-attended";
    set_policy(profile, r#"{"confirm": "off"}"#);
    let files = fixtures("files-attended");
    let (port, _) = serve();
    let mut c = Client::spawn(profile, headless());
    let outline = c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let upload = json!({ "action": "upload", "ref": find_ref(&outline, "Attachment"),
                         "files": [path(&files, "notes.txt")] });

    // The user sees which files would go where, and the upload waits for them.
    let out = c.ok("act", upload.clone());
    assert!(out.contains("needs their approval"), "{out}");
    let url = out
        .split_whitespace()
        .find(|w| w.starts_with("http"))
        .unwrap()
        .to_owned();
    let page = http_get(&url);
    assert!(
        page.contains("notes.txt") && page.contains(&format!("localhost:{port}")),
        "the approval names the files and the site"
    );
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    ws.send(tungstenite::Message::Text(r#"{"t":"cancel"}"#.into()))
        .unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(out.contains("denied"), "{out}");
    assert!(eval(&mut c, "document.getElementById('direct-out').textContent").is_empty());

    // Approved: the server performs exactly the approved upload.
    let out = c.ok("act", upload);
    let url = out
        .split_whitespace()
        .find(|w| w.starts_with("http"))
        .unwrap()
        .to_owned();
    let (mut ws, _) =
        tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    ws.send(tungstenite::Message::Text(r#"{"t":"done"}"#.into()))
        .unwrap();
    let out = c.ok("handoff_status", json!({ "wait_seconds": 10 }));
    assert!(out.contains("approved") && out.contains("chose"), "{out}");
    assert_eq!(
        eval(&mut c, "document.getElementById('direct-out').textContent"),
        "notes.txt=12"
    );
    c.ok("session_close", json!({}));
    let _ = std::fs::remove_dir_all(files);
}
