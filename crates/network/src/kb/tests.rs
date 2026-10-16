use serde_json::json;

use super::Record;
use super::store::Store;
use crate::catalog::{Catalog, Sample};
use crate::shape::Shape;

const PAGE: &str = "https://www.example.com/app";

/// Records one successful JSON call to `url` in `catalog`, as the network tap would.
fn call(catalog: &Catalog, method: &str, url: &str, body: Option<&str>, response: &str) -> String {
    let id = catalog.classify(method, url, body, &[], PAGE).unwrap();
    catalog.record_response(&id, 200, Some("application/json; charset=utf-8".into()));
    catalog.record_body(&id, Shape::of(&serde_json::from_str(response).unwrap()));
    catalog.record_sample(
        &id,
        Sample {
            request_id: "r1".into(),
            url: url.into(),
            headers: vec![
                ("csrf-token".into(), "ajax:SECRETCOOKIE".into()),
                ("Authorization".into(), "Bearer SECRETBEARER".into()),
                ("x-client".into(), "SECRETHEADER".into()),
            ],
            body: body.map(str::to_owned),
            cookies: vec![("JSESSIONID".into(), "\"ajax:SECRETCOOKIE\"".into())],
        },
    );
    id
}

#[test]
fn records_round_trip_and_accumulate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kb.sqlite");
    let catalog = Catalog::default();
    let id = call(
        &catalog,
        "POST",
        "https://api.example.com/v2/invitations/7123?action=accept&start=0",
        Some(r#"{"invitationId":"7123"}"#),
        r#"{"value":{"accepted":true}}"#,
    );
    catalog.add_trigger(&id, "click button \"Accept\"");

    let mut store = Store::open(&path).unwrap();
    let records = catalog.take_changes();
    assert_eq!(records.len(), 1);
    store.save(records, 1_000).unwrap();
    // Nothing changed since: nothing to write.
    assert!(catalog.take_changes().is_empty());

    let loaded = store.load("example.com").unwrap();
    assert_eq!(loaded.len(), 1);
    let record = &loaded[0];
    assert_eq!(record.method, "POST");
    assert_eq!(
        format!("{}{}", record.template.origin, record.template),
        "https://api.example.com/v2/invitations/{invitations_id}?action=accept"
    );
    assert!(record.mutating && !record.third_party);
    assert_eq!(record.query_keys.iter().collect::<Vec<_>>(), ["start"]);
    assert_eq!(record.content_type.as_deref(), Some("application/json"));
    assert_eq!(
        record.request_shape.as_ref().unwrap().render(3, 10),
        "{invitationId: str}"
    );
    assert_eq!(record.triggers, ["click button"]);
    assert_eq!(record.headers.len(), 1);
    assert_eq!(
        (
            record.headers[0].name.as_str(),
            record.headers[0].cookie.as_str()
        ),
        ("csrf-token", "JSESSIONID")
    );
    assert!(record.with_cookies);
    assert_eq!((record.calls, record.last_seen), (1, 1_000));

    // A later session adds calls and fields; the store merges rather than overwrites.
    let later = Catalog::default();
    call(
        &later,
        "POST",
        "https://api.example.com/v2/invitations/8000?action=accept",
        Some(r#"{"invitationId":"8000","note":"x"}"#),
        r#"{"value":{"accepted":true,"at":1}}"#,
    );
    store.save(later.take_changes(), 2_000).unwrap();
    let record = &store.load("example.com").unwrap()[0];
    assert_eq!((record.calls, record.last_seen), (2, 2_000));
    assert_eq!(
        record.response_shape.as_ref().unwrap().render(3, 10),
        "{value: {accepted: bool, at: num}}"
    );
    assert_eq!(record.triggers, ["click button"]);
    assert!(store.load("other.com").unwrap().is_empty());
}

#[test]
fn nothing_private_reaches_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kb.sqlite");
    let catalog = Catalog::default();
    let id = call(
        &catalog,
        "POST",
        "https://www.example.com/api/messages?token=SECRETQUERY&to=ada%40example.com",
        Some(r#"{"text":"SECRETBODY","recipient":{"name":"Ada Lovelace"}}"#),
        &json!({
            "byId": { "ada@example.com": { "name": "Ada" }, "urn:li:member:123": {} },
            "items": [{ "secretValue": "SECRETRESPONSE" }],
        })
        .to_string(),
    );
    catalog.add_trigger(&id, "click button \"Message Ada Lovelace\"");
    // Text in the path or a selector is data, not structure: such endpoints aren't kept.
    call(
        &catalog,
        "GET",
        "https://www.example.com/search/ada%20lovelace",
        None,
        "{}",
    );
    call(
        &catalog,
        "GET",
        "https://www.example.com/find?q=ada+lovelace",
        None,
        "{}",
    );

    // Search text is a parameter value, never part of the endpoint: `/find` is kept as
    // structure (a `q` key, no value), while the path carrying text is dropped.
    let records = catalog.take_changes();
    assert_eq!(records.len(), 2, "{records:?}");
    assert!(
        records.iter().all(|r| r.template.selectors.is_empty()),
        "{records:?}"
    );
    let mut store = Store::open(&path).unwrap();
    store.save(records, 1_000).unwrap();
    drop(store);

    let mut stored = Vec::new();
    for file in std::fs::read_dir(dir.path()).unwrap() {
        stored.extend(std::fs::read(file.unwrap().path()).unwrap());
    }
    let stored = String::from_utf8_lossy(&stored);
    for secret in [
        "SECRET",
        "Ada",
        "ada",
        "Lovelace",
        "example.com\":",
        "member:123",
        "Bearer",
        "x-client",
    ] {
        assert!(!stored.contains(secret), "{secret} was persisted");
    }
    // The structure itself is there.
    for kept in [
        "messages",
        "token",
        "csrf-token",
        "JSESSIONID",
        "click button",
        "secretValue",
    ] {
        assert!(stored.contains(kept), "{kept} is missing");
    }
}

#[tokio::test]
async fn a_new_session_starts_with_what_earlier_ones_learned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kb.sqlite");
    let first = Catalog::persistent(path.clone());
    first.visit(PAGE);
    call(
        &first,
        "GET",
        "https://www.example.com/api/users/42/repos?page=2",
        None,
        r#"[{"name":"x"}]"#,
    );
    first.flush().await;
    drop(first);

    let second = Catalog::persistent(path);
    assert!(second.endpoints().is_empty());
    second.visit("https://example.com/somewhere");
    second.ready().await;
    let known = second.endpoints();
    assert_eq!(known.len(), 1);
    let endpoint = &known[0];
    assert_eq!(endpoint.template.to_string(), "/api/users/{users_id}/repos");
    assert_eq!(endpoint.calls, 0);
    assert!(endpoint.sample.is_none());
    let learned = endpoint.learned.as_ref().unwrap();
    assert_eq!(learned.calls, 1);
    assert_eq!(learned.headers[0].cookie.as_deref(), Some("JSESSIONID"));

    // Seen live again, it is the same endpoint.
    let id = second
        .classify(
            "GET",
            "https://www.example.com/api/users/7/repos",
            None,
            &[],
            PAGE,
        )
        .unwrap();
    assert_eq!(id, endpoint.id);
    assert_eq!(second.endpoints().len(), 1);
    assert_eq!(second.get(&id).unwrap().calls, 1);
}

#[test]
fn endpoints_without_a_response_wait() {
    let catalog = Catalog::default();
    catalog
        .classify("GET", "https://www.example.com/api/items", None, &[], PAGE)
        .unwrap();
    assert!(catalog.take_changes().is_empty());
    let id = catalog
        .classify("GET", "https://www.example.com/api/items", None, &[], PAGE)
        .unwrap();
    catalog.record_response(&id, 200, Some("application/json".into()));
    let records: Vec<Record> = catalog.take_changes();
    assert_eq!(records[0].calls, 2);
}

#[test]
fn search_text_stored_by_earlier_builds_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kb.sqlite");
    {
        // A database at the previous schema, holding what the old rule let through.
        let conn = rusqlite::Connection::open(&path).unwrap();
        for sql in &super::store::MIGRATIONS[..2] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 2).unwrap();
        for (site, q) in [
            ("a.com", "ada lovelace"),
            ("a.com", "E-easy"),
            ("li.com", "findAlerts"),
        ] {
            let template = serde_json::json!({
                "origin": format!("https://{site}"), "segments": [{"Literal": "find"}],
                "selectors": [["q", q]], "graphql": null,
            });
            conn.execute(
                "INSERT INTO endpoints VALUES (?1, 'GET', ?2, 0, 0, '[]', NULL, NULL, NULL, '[]', '[]', 0, 1, 0, ?3)",
                rusqlite::params![site, template.to_string(), super::unix_now()],
            )
            .unwrap();
        }
    }
    let store = Store::open(&path).unwrap();
    assert!(store.load("a.com").unwrap().is_empty());
    assert_eq!(store.load("li.com").unwrap().len(), 1);
}
