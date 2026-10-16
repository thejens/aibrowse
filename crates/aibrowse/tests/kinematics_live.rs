//! Kinematics recording and persona fitting, end to end: a handoff's live view receives
//! a synthetic user's input over its websocket, the profile's statistics file fills, and
//! `aibrowse persona fit` moves the persona toward that user. Ignored by default; run
//! with `make test-live`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use aibrowse_input::{Device, Pace, Persona, Point, plan_move};
use rand::SeedableRng;
use serde_json::{Value, json};

mod common;
use common::{Client, http_get, set_policy};

const PROFILE: &str = "test-kinematics";

/// Targets of different sizes at fixed positions, so the recorder's hit test finds a
/// known width under each click.
const PAGE: &str = r#"<!doctype html><title>Targets</title>
<body style="margin:0;height:3000px">
<button id="small" style="position:absolute;left:100px;top:100px;width:12px;height:12px"></button>
<button id="wide" style="position:absolute;left:480px;top:60px;width:200px;height:100px">wide</button>
<button id="mid" style="position:absolute;left:300px;top:420px;width:40px;height:30px">mid</button>
<button id="tiny" style="position:absolute;left:700px;top:450px;width:8px;height:8px"></button>
<input id="text" aria-label="Text" style="position:absolute;left:80px;top:300px;width:300px;height:30px">
</body>"#;

/// (center, min side) of each target, visited in turn: indexes of difficulty from 2.5
/// to 6 bits, so the Fitts slope can be told apart from the intercept.
const TARGETS: [((f64, f64), f64); 4] = [
    ((106.0, 106.0), 12.0),
    ((580.0, 110.0), 100.0),
    ((320.0, 435.0), 30.0),
    ((704.0, 454.0), 8.0),
];

fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{PAGE}",
                    PAGE.len()
                );
            });
        }
    });
    port
}

fn profile_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join(".aibrowse/profiles")
        .join(PROFILE)
}

fn read_persona() -> Persona {
    serde_json::from_slice(&std::fs::read(profile_dir().join("persona.json")).unwrap()).unwrap()
}

/// The synthetic user: slower, curvier pointing than any seeded persona, a notched
/// wheel with 120px steps, and a steady typist.
fn user() -> Persona {
    Persona {
        device: Device::Wheel,
        fitts_a_ms: 250.0,
        fitts_b_ms: 210.0,
        path_bow: 0.09,
        click_dwell_median_ms: 160.0,
        click_press_median_ms: 120.0,
        ..Persona::from_seed(1)
    }
}

struct User {
    ws: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    t: f64,
}

impl User {
    fn send(&mut self, message: Value) {
        self.ws
            .send(tungstenite::Message::Text(message.to_string().into()))
            .unwrap();
    }

    fn mouse(&mut self, kind: &str, p: Point, button: &str, buttons: u32) {
        let click_count = u32::from(kind != "mouseMoved");
        let message = json!({ "t": "mouse", "kind": kind, "x": p.x, "y": p.y, "button": button,
            "buttons": buttons, "clickCount": click_count, "modifiers": 0, "ts": self.t, "pt": "mouse" });
        self.send(message);
    }

    fn key(&mut self, kind: &str, ch: char) {
        let (key, code) = if ch == ' ' {
            (" ".to_owned(), "Space".to_owned())
        } else {
            (ch.to_string(), format!("Key{}", ch.to_ascii_uppercase()))
        };
        let message = json!({ "t": "key", "kind": kind, "key": key, "code": code,
            "keyCode": 0, "modifiers": 0, "ts": self.t });
        self.send(message);
    }
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn handoff_input_is_recorded_and_fitted() {
    let dir = profile_dir();
    // A fresh persona (a new seed) and no earlier recording, so the fit starts from a
    // seeded persona as on a profile's first recording.
    let _ = std::fs::remove_file(dir.join("kinematics.json"));
    let _ = std::fs::remove_file(dir.join("persona.json"));
    set_policy(PROFILE, r#"{"confirm": "off", "kinematics": "record"}"#);
    let port = serve();
    let headless = std::env::var("AIBROWSE_TEST_HEADLESS").is_ok_and(|v| v == "1");
    let mut c = Client::spawn(PROFILE, headless);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/") }),
    );
    let out = c.ok(
        "request_handoff",
        json!({ "reason": "Show me how you click", "open": false }),
    );
    let url = out
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("live view url")
        .to_owned();
    // The page tells the user that recording is on.
    let page = http_get(&url);
    assert!(
        page.contains(r#"class="recording " id="recording""#),
        "recording indicator missing"
    );
    let before = read_persona();

    let (ws, _) = tungstenite::connect(format!("{}/ws", url.replacen("http", "ws", 1))).unwrap();
    let mut u = User { ws, t: 1000.0 };
    let user = user();
    let mut rng = rand::rngs::StdRng::seed_from_u64(17);
    let mut at = Point::new(400.0, 300.0);
    u.mouse("mouseMoved", at, "none", 0);
    for i in 0..48 {
        let ((x, y), width) = TARGETS[i % TARGETS.len()];
        let to = Point::new(x, y);
        let start = u.t + 400.0;
        for step in plan_move(at, to, width, &user, Pace::Human, &mut rng) {
            u.t = start + step.at.as_secs_f64() * 1000.0;
            u.mouse("mouseMoved", step.point, "none", 0);
        }
        u.t += user.click_dwell_median_ms + (i % 5) as f64 * 10.0 - 20.0;
        u.mouse("mousePressed", to, "left", 1);
        u.t += user.click_press_median_ms + (i % 3) as f64 * 8.0 - 8.0;
        u.mouse("mouseReleased", to, "left", 0);
        at = to;
    }

    // Typing into the text field: a key every 210ms, held 85–105ms (median 95ms), so
    // 115ms from each release to the next press.
    let field = Point::new(230.0, 315.0);
    u.t += 500.0;
    u.mouse("mouseMoved", field, "none", 0);
    u.mouse("mousePressed", field, "left", 1);
    u.mouse("mouseReleased", field, "left", 0);
    let secret = "the quick brown fox jumps over the lazy dog ";
    for (i, ch) in secret.repeat(3).chars().enumerate() {
        let hold = [85.0, 95.0, 105.0][i % 3];
        u.t += 210.0;
        u.key("keyDown", ch);
        u.t += hold;
        u.key("keyUp", ch);
        u.t -= hold;
    }

    // Wheel spins: 120px notches, 40–80ms apart, gestures a second apart. Down then up,
    // so the page ends where it started.
    for spin in 0..16 {
        u.t += 1000.0;
        let dy = if spin % 2 == 0 { 120.0 } else { -120.0 };
        for gap in [0.0, 52.0, 71.0, 44.0] {
            u.t += gap;
            u.send(json!({ "t": "wheel", "x": 400.0, "y": 300.0, "dx": 0.0, "dy": dy, "ts": u.t }));
        }
    }
    u.send(json!({ "t": "done" }));

    // The statistics are saved once the handoff resolves, after every event before Done
    // has been forwarded: each move waits for Chrome to take it, and this synthetic user
    // sent ~1500 at once where a real one spreads them over minutes.
    let file = dir.join("kinematics.json");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = std::fs::read_to_string(&file).expect("kinematics.json written");
    let recorded: Value = serde_json::from_str(&text).unwrap();
    c.ok("session_close", json!({}));

    assert_eq!(recorded["handoffs"], 1);
    let fitts_n = recorded["pointer"]["fitts"]["n"].as_u64().unwrap();
    assert!(
        fitts_n >= 40,
        "aimed strokes with a known target: {fitts_n}"
    );
    assert_eq!(recorded["wheel"]["notch_px"]["120"], 64);
    assert_eq!(recorded["wheel"]["notched_gestures"], 16);
    let holds: u64 = recorded["keys"]["hold_ms"]
        .as_object()
        .unwrap()
        .values()
        .map(|n| n.as_u64().unwrap())
        .sum();
    assert_eq!(holds, secret.len() as u64 * 3);
    for word in ["quick", "brown", "lazy", "KeyQ", "Space"] {
        assert!(!text.contains(word), "{word} stored in {text}");
    }

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aibrowse"))
        .args(["persona", "fit", "--profile", PROFILE])
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{report}");
    eprintln!("{report}");
    assert!(report.contains("Saved persona.json"), "{report}");

    let after = read_persona();
    let closer = |name: &str, b: f64, a: f64, target: f64| {
        assert!(
            (a - target).abs() < (b - target).abs() || (a - target).abs() < 1.0,
            "{name}: {b} → {a} should approach {target}"
        );
    };
    assert_eq!(after.seed, before.seed);
    assert_eq!(after.device, Device::Wheel);
    assert_eq!(after.wheel_notch_px, 120.0);
    closer(
        "fitts_b_ms",
        before.fitts_b_ms,
        after.fitts_b_ms,
        user.fitts_b_ms,
    );
    closer("path_bow", before.path_bow, after.path_bow, user.path_bow);
    closer(
        "click_press_median_ms",
        before.click_press_median_ms,
        after.click_press_median_ms,
        user.click_press_median_ms,
    );
    closer(
        "key_hold_median_ms",
        before.key_hold_median_ms,
        after.key_hold_median_ms,
        95.0,
    );
    closer(
        "keystroke_median_ms",
        before.keystroke_median_ms,
        after.keystroke_median_ms,
        115.0,
    );
}
