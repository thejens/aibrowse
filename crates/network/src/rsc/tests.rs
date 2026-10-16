//! Hand-written flight payloads, after React 19's wire format and what Next.js sends.

use std::fmt::Write;
use std::time::Instant;

use serde_json::json;

use super::*;

/// A Next.js (15) server action response: row 0 is the action envelope, the returned
/// data is a promise outlined to row 1.
const SERVER_ACTION: &str = r#"0:{"a":"$@1","f":"","b":"Xb2kz9Qh","q":"","i":false}
1:{"ok":true,"invitations":[{"id":"inv_7101","name":"Grace Hopper","sent":"$D2026-10-01T09:30:00.000Z"},{"id":"inv_7102","name":"Alan Turing","sent":"$D2026-10-02T11:00:00.000Z"}],"next":"$@2"}
2:{"cursor":"c-2","hasMore":false,"total":"$n12"}
"#;

/// A server-rendered list: a client component from an import row, Suspense outlined to
/// its own row, a server action prop, long text in a `T` row, a deduplicated path
/// reference, a preload hint, and an icon.
fn page() -> String {
    let note = "A note with\ntwo lines, ünïcode included";
    let rows = [
        r#"1:I["(app-pages-browser)/./src/components/InvitationCard.tsx",["static/chunks/app/page-3f2.js"],"default"]"#.to_owned(),
        r#"2:"$Sreact.suspense""#.to_owned(),
        r#"4:HL["/_next/static/css/app.css","style"]"#.to_owned(),
        format!("3:T{:x},{note}", note.len()),
        concat!(
            r#"0:["$","main",null,{"className":"p-4","children":["#,
            r#"["$","h1",null,{"children":"Invitations"}],"#,
            r#"["$","$2",null,{"fallback":["$","p",null,{"children":"Loading"}],"children":"#,
            r#"["$","ul",null,{"children":["#,
            r#"["$","$L1","inv-1",{"invitation":{"id":"7101","name":"Grace Hopper"},"onAccept":"$h5","onClick":"$E","className":"card"}],"#,
            r#"["$","li",null,{"children":["Pending: ",3]}]]}]}],"#,
            r#"["$","a",null,{"href":"/mynetwork","className":"link","children":"See all"}],"#,
            r#"["$","p",null,{"children":"$3"}],"#,
            r#"["$","svg",null,{"viewBox":"0 0 1 1","children":["$","title",null,{"children":"icon"}]}],"#,
            r#""$L6"]}]"#
        )
        .to_owned(),
        r#"5:{"id":"7f3a9c0d","bound":null}"#.to_owned(),
        r#"6:["$","footer",null,{"children":["$7:props:children"," today"]}]"#.to_owned(),
        r#"7:["$","span",null,{"children":"$$5 off"}]"#.to_owned(),
    ];
    rows.join("\n") + "\n"
}

#[test]
fn decodes_a_server_action_response() {
    let flight = decode(SERVER_ACTION).unwrap();
    assert_eq!(flight.rows, 3);
    assert!(!flight.truncated);
    let doc = flight.document();
    assert!(doc.get("text").is_none(), "{doc}");
    let data = &doc["value"]["a"];
    assert_eq!(data["invitations"][1]["name"], "Alan Turing");
    assert_eq!(data["invitations"][0]["sent"], "2026-10-01T09:30:00.000Z");
    assert_eq!(
        data["next"],
        json!({"cursor": "c-2", "hasMore": false, "total": "12"})
    );
    assert_eq!(doc["value"]["b"], "Xb2kz9Qh");
}

#[test]
fn compacts_element_trees_to_text_and_data() {
    let flight = decode(&page()).unwrap();
    assert!(!flight.truncated);
    assert_eq!(
        flight.text,
        [
            "Invitations",
            "Pending: 3",
            "See all",
            "A note with two lines, ünïcode included",
            "$5 off today",
        ]
    );
    // Wrappers without data collapse into their children; the Suspense fallback, the
    // icon, class names and plain handlers are gone; a server action keeps its id.
    assert_eq!(
        flight.value,
        json!([
            "Invitations",
            {
                "$el": "InvitationCard",
                "invitation": {"id": "7101", "name": "Grace Hopper"},
                "onAccept": {"$action": "7f3a9c0d"},
            },
            "Pending: 3",
            {"$el": "a", "href": "/mynetwork", "children": "See all"},
            "A note with\ntwo lines, ünïcode included",
            "$5 off today",
        ])
    );
}

#[test]
fn older_envelopes_streams_and_special_values() {
    let body = concat!(
        "0:[\"$@1\",[\"build-7\",null]]\n",
        "1:{\"m\":\"$Q2\",\"s\":\"$W3\",\"events\":\"$4\",\"u\":\"$u\",\"inf\":\"$I\",\"neg\":\"$-0\",\"sym\":\"$Sreact.fragment\",\"esc\":\"$$100\"}\n",
        "2:[[\"lang\",\"en\"],[\"theme\",\"dark\"]]\n",
        "3:[\"a\",\"b\"]\n",
        "4:R\n",
        "4:{\"n\":1}\n",
        "4:{\"n\":2}\n",
        "4:C\n",
    );
    let flight = decode(body).unwrap();
    let data = &flight.value[0];
    assert_eq!(flight.value[1][0], "build-7");
    assert_eq!(data["m"], json!({"lang": "en", "theme": "dark"}));
    assert_eq!(data["s"], json!(["a", "b"]));
    assert_eq!(data["events"], json!([{"n": 1}, {"n": 2}]));
    assert_eq!(data["u"], json!(null));
    assert_eq!(data["inf"], "Infinity");
    assert_eq!(data["neg"], 0);
    assert_eq!(data["sym"], "[react.fragment]");
    assert_eq!(data["esc"], "$100");
}

#[test]
fn malformed_and_truncated_payloads_keep_what_they_can() {
    let body = concat!(
        "0:{\"items\":[\"$1\",\"$2\",\"$L9\",\"$e\"],\"self\":\"$0\",\"deep\":\"$1:a:b\"}\n",
        "1:{\"broken\":\n",
        "2:Zsomething new\n",
        "e:E{\"digest\":\"3141592\"}\n",
        "garbage line\n",
        "3:T40,short",
    );
    let flight = decode(body).unwrap();
    assert!(flight.truncated);
    let v = &flight.value;
    assert_eq!(v["items"][0], json!({"$raw": "{\"broken\":"}));
    assert_eq!(v["items"][1], json!({"$row": "Z", "raw": "something new"}));
    assert_eq!(v["items"][2], "[row 9 not in the response]");
    assert_eq!(v["items"][3], json!({"$error": "3141592"}));
    assert_eq!(v["self"], "[cycle: row 0]");
    assert_eq!(v["deep"], "[$1:a:b not in the response]");

    assert!(decode("").is_none());
    assert!(decode("<!doctype html><p>not flight</p>").is_none());
    // A declared length ending inside a character, and a payload cut mid-row.
    let flight = decode("1:T1,éx\n0:\"ok\"\n").unwrap();
    assert_eq!(flight.value, "ok");
    let flight = decode("0:{\"a\":[1,2").unwrap();
    assert!(flight.truncated);
    assert_eq!(flight.value, json!({"$raw": "{\"a\":[1,2"}));
    // Without a row 0, every row is shown by id.
    let flight = decode("a:{\"x\":1}\nb:\"$a\"\n").unwrap();
    assert_eq!(flight.value, json!({"a": {"x": 1}, "b": {"x": 1}}));
}

#[test]
fn output_stays_bounded_when_rows_fan_out() {
    // Each row references the next twice: 2^60 nodes if expanded naively.
    let mut body = String::new();
    for id in 0..60u32 {
        let _ = writeln!(body, "{id:x}:[\"${:x}\",\"${:x}\"]", id + 1, id + 1);
    }
    body.push_str("3c:\"leaf\"\n");
    let started = Instant::now();
    let flight = decode(&body).unwrap();
    assert!(flight.truncated);
    assert!(started.elapsed().as_secs() < 2, "{:?}", started.elapsed());
    let size = serde_json::to_string(&flight.value).unwrap().len();
    assert!(size < 2_000_000, "{size}");

    // A long chain of references is cut at the depth limit, not the stack.
    let mut body = String::new();
    for id in 0..5_000u32 {
        let _ = writeln!(body, "{id:x}:{{\"next\":\"$L{:x}\"}}", id + 1);
    }
    let flight = decode(&body).unwrap();
    assert!(
        serde_json::to_string(&flight.value)
            .unwrap()
            .contains("[too deep]")
    );
}

#[test]
fn recognizes_the_media_type() {
    assert!(is_flight("text/x-component"));
    assert!(is_flight("text/x-component; charset=utf-8"));
    assert!(!is_flight("application/json"));
    assert!(is_flight_response(
        "application/octet-stream",
        "0:{\"a\":1}\n"
    ));
    assert!(is_flight_response("", "1:I[\"chunk\",[]]\n0:\"$L1\"\n"));
    assert!(!is_flight_response("application/octet-stream", "\u{89}PNG"));
    assert!(!is_flight_response(
        "application/octet-stream",
        "key: value"
    ));
    assert!(!is_flight_response("application/json", "0:{}"));
}
