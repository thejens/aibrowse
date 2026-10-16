//! Goals in several parts against a real Chrome and Jev: a spreadsheet drawn on a
//! canvas, as Google Sheets is, where a formula is entered through the Name Box and the
//! formula bar and filled down with a shortcut the agent offers.

mod common;

use common::{Client, has_jev_key, serve_pages, set_policy};
use serde_json::json;

const GRID: &str = include_str!("../../../fixtures/html/grid.html");

#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn a_formula_is_entered_and_filled_down_in_one_goal() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-parts", r#"{"confirm": "consequential"}"#);
    let port = serve_pages(&[("/", GRID)]);
    let mut c = Client::spawn("test-parts", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/"), "observe": false }),
    );
    let out = c.ok(
        "navigate",
        json!({
            "goal": "Enter the formula =A2+10 in cell C3",
            "then": ["Select cells C3 to C10", "Fill the formula in C3 down to C10"],
            "texts": ["C3", "=A2+10", "C3:C10"],
            "keys": ["Meta+d: fill the selection down from its first cell"],
            "observe": false,
        }),
    );
    let values = c.ok(
        "evaluate",
        json!({ "expression": "[3,4,5,6,7,8,9,10].map(r => String(value('C' + r))).join(',')", "world": "main" }),
    );
    assert!(
        values.contains("11,12,13,14,15,16,17,18"),
        "C3:C10 hold =A2+10 filled down: {values}\n{out}"
    );
    assert!(
        out.contains("Part 3 of 3") && !out.contains("Not started"),
        "{out}"
    );
    c.ok("session_close", json!({}));
}

/// A count from a seed, as shown live on Google Sheets: the select part ranks the Name
/// Box as a step rather than as text needed, and must still type the range into it.
#[test]
#[ignore = "needs Chrome and a Jev key; run with `make test-live`"]
fn a_column_counts_from_a_seed_in_one_goal() {
    if !has_jev_key() {
        eprintln!("skipped: no Jev key");
        return;
    }
    set_policy("test-parts-count", r#"{"confirm": "consequential"}"#);
    let port = serve_pages(&[("/", GRID)]);
    let mut c = Client::spawn("test-parts-count", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/"), "observe": false }),
    );
    let out = c.ok(
        "navigate",
        json!({
            "goal": "Enter the number 1 in cell E1",
            "then": [
                "Enter the formula =E1+1 in cell E2",
                "Select cells E2 to E10",
                "Fill the formula in E2 down to E10"
            ],
            "texts": ["E1", "1", "E2", "=E1+1", "E2:E10"],
            "keys": ["Meta+d: fill the selection down from its first cell"],
            "observe": false,
        }),
    );
    let values = c.ok(
        "evaluate",
        json!({ "expression": "[1,2,3,4,5,6,7,8,9,10].map(r => String(value('E' + r))).join(',')", "world": "main" }),
    );
    assert!(
        values.contains("1,2,3,4,5,6,7,8,9,10"),
        "E1:E10 count from 1: {values}\n{out}"
    );
    assert!(
        out.contains("Part 4 of 4") && !out.contains("Not started"),
        "{out}"
    );
    c.ok("session_close", json!({}));
}
