//! `read` on the shapes of page that read badly before: content hidden from assistive
//! technology behind a modal, layout tables, an empty main region, and the parts of an
//! article (headings, code, collapsed tables, forms) whose text came out wrong. Ignored
//! by default; run with `make test-live`.

use serde_json::json;

mod common;
use common::{Client, serve_pages};

/// Tom's Hardware's pattern: while a consent modal is open, every paragraph of the
/// article is aria-hidden, yet renders.
const MODAL: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>GPU review</title>
<div role="dialog" aria-modal="true" style="position:fixed;inset:0;background:#fff8">
<p>We value your privacy and use cookies.</p><button>Accept</button></div>
<main><h1>The new GPU, reviewed</h1>
<p aria-hidden="true">The card draws 450 watts under load and stays quiet throughout.</p>
<p aria-hidden="true">At 4K it averages 140 frames per second across our test suite.</p>
<p>Rated <span aria-hidden="true">★</span> 4 of 5.</p></main>"#;

/// Hacker News' pattern: a thread is tables nested in table cells, with no headers.
const THREAD: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Thread</title>
<table id="hnmain"><tr><td>
<table class="fatitem"><tr><td class="title"><span class="titleline"><a href="/x">Show: a tiny database</a></span></td></tr></table>
<table class="comment-tree"><tr><td>
  <table><tr><td class="ind"><img src="s.gif" width="0" height="1"></td>
    <td class="default"><div><span class="comhead">alice 2 hours ago</span></div>
    <div class="commtext">The storage engine is clever.<p>But the docs are thin.</p></div></td></tr></table>
  <table><tr><td class="ind"><img src="s.gif" width="40" height="1"></td>
    <td class="default"><div><span class="comhead">bob 1 hour ago</span></div>
    <div class="commtext">Agreed on the docs.</div></td></tr></table>
</td></tr></table>
</td></tr></table>"#;

/// GSMArena's pattern: an empty element with role=main, the specs outside it.
const EMPTY_MAIN: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Phone specs</title>
<div id="nav" role="main"></div>
<div id="specs"><h1>Phone X specifications</h1>
<p>Display: 6.8 inches, 120 Hz. Battery: 5000 mAh. Weight: 218 grams.</p></div>"#;

/// An article with the parts that read wrong: paragraphs (one after an aria-hidden svg
/// icon, whose lower-case tagName crashed the reader), a heading with a responsive
/// twin and a noscript fallback, a code block with a copy button, a fee table in a
/// closed <details>, a data table whose cells carry aria-labels, a converter whose
/// result sits inside its form, and a sign-in form.
const ARTICLE: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Guide</title>
<style>.short { display: none }</style>
<main>
<h1><span class="long">Fees and tools guide</span><span class="short">Fees guide</span><noscript><img src="hero.png" class="hero"></noscript></h1>
<p>First paragraph of the guide.</p><p>Second paragraph of the guide.</p>
<p><svg aria-hidden="true" width="12" height="12"><path d="M0 0h12v12z"/></svg> Third paragraph, after an icon.</p>
<pre><button>Copy</button><code>print("hi")
print("bye")</code></pre>
<details><summary>Fees</summary><div><table><tr><th>Applicant</th><th>Fee</th></tr>
<tr><td>Adult</td><td>653.00</td></tr></table></div></details>
<table><tr><th>Pos</th><th>Club</th></tr>
<tr><td>1</td><td aria-label="View Team profile"><a href="/city"><span>Manchester City</span></a></td></tr></table>
<form><label>Amount <input value="100"></label><button>Convert</button>
<div class="result">100 USD = 92.50 EUR</div></form>
<form><input type="email"><input type="password"><a href="/forgot">Forgot password?</a><button>Sign in</button></form>
</main>"#;

/// Values that read wrong: a spec table with cells spanning rows and columns, result
/// times drawn as buttons among control buttons, a link card in a closed <details>, and
/// counters in boxes of their own with no whitespace between them.
const VALUES: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Values</title>
<main><h1>Values</h1>
<table><tr><th rowspan="2">Battery</th><td>Type</td><td>Li-Ion 3582 mAh</td></tr>
<tr><td>Charging</td><td>Wired, PD2.0</td></tr>
<tr><th colspan="2">Price</th><td>$999</td></tr></table>
<ul class="locations"><li><div>New York</div><div><button aria-haspopup="dialog">11:00</button></div></li>
<li><div>Tokyo</div><div><button>00:00</button></div></li>
<li><button>Copy</button></li><li><button>2</button></li></ul>
<p>Pick a day: <button><span>Mon</span><span>17</span></button> <button>Submit</button> <button>Save 20%</button></p>
<details><summary>Restore</summary><a href="/restore"><div>Restore your status as a student</div><div>Restore your status ($246.25)</div></a></details>
<div><a href="/u">Trenton</a></div>
<div><span>63.5k</span><span style="margin-left:6px">41 gold badges</span></div>
<div><span>Hel</span><span>lo</span></div>
<div>PD2.0<i style="display:inline-block;width:16px;height:16px"></i>25W, 3582<span>mAh</span>, 15W<svg width="16" height="16"></svg>MagSafe</div>
</main>"#;

/// A package page (npm, crates.io): the README is the article, and the facts about the
/// package (version, license) are in a header and a sidebar beside it. A file list of
/// links and a long "related" column are beside it too, and aren't facts.
const PACKAGE: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>widget - npm</title>
<main><div class="top"><div class="head"><h1>widget</h1><span>4.6.7 • Public • Published 3 days ago</span></div>
<div class="cols"><div class="readme"><article><h2>Widget</h2>
<p>Widget validates schemas. ...</p></article></div>
<aside aria-label="Package sidebar"><h3>Version</h3><p>4.6.7</p><h3>License</h3><p>MIT</p>
<h3>Last publish</h3><p>3 days ago</p></aside></div>
<div class="files"><a href="/a">src/index.ts</a> <a href="/b">src/parse.ts</a> <a href="/c">package.json</a></div>
<div class="related"><p>RELATED ...</p></div></div></main>"#;

/// A plain-text document in <pre> (an RFC on rfc-editor.org), its sections anchored.
const RFC: &str = r##"<!doctype html><html lang="en"><meta charset="utf-8"><title>RFC 6749</title>
<pre>INTRO
<span class="h3"><a class="selflink" id="section-4.1.2" href="#section-4.1.2">4.1.2</a>.  Authorization Response</span>

   code
         REQUIRED.  A maximum authorization code lifetime of 10
         minutes is RECOMMENDED.

LATER</pre>"##;

/// Tables whose labels aren't text: a caption, header cells that are icons (titled, or
/// over a hidden name), an icon font's glyph names; and an accepted answer.
const LABELS: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Labels</title>
<style>.label { display: none } .ms { font-family: 'Material Symbols Outlined' }</style>
<main><h1>Labels</h1>
<table><caption>Oct - 2026</caption><tr><th>Mon</th><th>Tue</th></tr><tr><td>5 11.25</td><td>6 11.24</td></tr></table>
<table><tr><th></th><th><span class="label">Chrome</span><i class="icon"></i></th>
<th title="Firefox"><svg width="9" height="9"><path d="M0 0h9v9z"/></svg></th></tr>
<tr><td>findLast</td><td>97</td><td>104</td></tr></table>
<table><tr><th>Party <span class="ms">swap_vert</span></th><th>Seats</th></tr>
<tr><td>S <span class="material-symbols-outlined">arrow_upward_alt</span></td><td>99</td></tr></table>
<p>Sort <i class="ms">expand_more</i> by size.</p>
<button aria-expanded="false" aria-controls="all"><span class="material-symbols-outlined" aria-hidden="true">add</span>Show all parties</button>
<div id="all" hidden><p>Other parties: 1.58%</p></div>
<div class="answer" itemprop="suggestedAnswer"><p>Use git revert.</p></div>
<div class="answer accepted-answer" itemprop="acceptedAnswer"><div>30172</div><p>Use git reset HEAD~.</p></div>
</main>"#;

/// A page whose content a script draws a moment after it loads.
const LATE: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Late</title>
<div>An official website of the United States government</div><main id="m"></main>
<script>setTimeout(() => { document.getElementById('m').innerHTML =
'<h1>CVE-2024-3094 Detail</h1><p>' + 'Malicious code was discovered in xz 5.6.0 and 5.6.1. '.repeat(40) + '</p>'; }, 1200);</script>"#;

const PAGES: &[(&str, &str)] = &[
    ("/modal", MODAL),
    ("/thread", THREAD),
    ("/empty-main", EMPTY_MAIN),
    ("/article", ARTICLE),
    ("/values", VALUES),
    ("/package", PACKAGE),
    ("/rfc", RFC),
    ("/labels", LABELS),
    ("/late", LATE),
];

/// `PAGES` with the long pages' text made long here, which keeps the fixtures above
/// readable; leaked, as the server serves from statics.
fn long_pages() -> &'static [(&'static str, &'static str)] {
    let readme = "Widget validates schemas and infers static types from them. ".repeat(60);
    let related = "Another package you may like, with a long description. ".repeat(40);
    let paragraph = |n: usize| {
        format!(
            "   The authorization server answers the request of client {} within the\n   \
             time the client allows, and logs what it answered for later audits.\n",
            [
                "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel"
            ][n % 8]
        )
    };
    let text = |n: usize| (0..n).map(paragraph).collect::<Vec<_>>().join("\n");
    let long = |path: &str, body: &str| -> String {
        match path {
            "/package" => body
                .replace("Widget validates schemas. ...", &readme)
                .replace("RELATED ...", &related),
            "/rfc" => body
                .replace("INTRO", &format!("1.  Introduction\n\n{}", text(30)))
                .replace(
                    "LATER",
                    &format!("4.1.3.  Access Token Request\n\n{}", text(30)),
                ),
            _ => body.to_owned(),
        }
    };
    let pages: Vec<(&'static str, &'static str)> = PAGES
        .iter()
        .map(|&(path, body)| (path, &*Box::leak(long(path, body).into_boxed_str())))
        .collect();
    Box::leak(pages.into_boxed_slice())
}

fn read(c: &mut Client, port: u16, path: &str) -> String {
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}{path}"), "observe": false }),
    );
    c.ok("read", json!({}))
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn aria_hidden_text_behind_a_modal_is_read() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-read-modal", true);
    let text = read(&mut c, port, "/modal");
    assert!(text.contains("draws 450 watts"), "{text}");
    assert!(text.contains("140 frames per second"), "{text}");
    assert!(!text.contains("value your privacy"), "the modal: {text}");
    assert!(!text.contains('★'), "an aria-hidden icon: {text}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn layout_tables_read_as_blocks_and_an_empty_main_is_passed_over() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-read-layout", true);
    let text = read(&mut c, port, "/thread");
    assert!(!text.contains('|'), "read as a data table: {text}");
    assert!(text.contains("Show: a tiny database"), "{text}");
    assert!(
        text.contains("alice 2 hours ago\nThe storage engine is clever."),
        "{text}"
    );
    assert!(text.contains("But the docs are thin."), "{text}");
    assert!(
        text.contains("bob 1 hour ago\nAgreed on the docs."),
        "{text}"
    );

    let text = read(&mut c, port, "/empty-main");
    assert!(text.contains("# Phone X specifications"), "{text}");
    assert!(text.contains("Battery: 5000 mAh"), "{text}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn an_articles_parts_read_as_they_show() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-read-article", true);
    let text = read(&mut c, port, "/article");
    // A heading's rendered words: one title, no noscript markup.
    assert!(text.contains("# Fees and tools guide\n"), "{text}");
    assert!(text.contains("Third paragraph, after an icon."), "{text}");
    assert!(
        !text.contains("Fees guide") && !text.contains("<img"),
        "{text}"
    );
    // Paragraphs stand apart.
    assert!(
        text.contains("First paragraph of the guide.\n\nSecond paragraph of the guide."),
        "{text}"
    );
    // Code without its copy button.
    assert!(
        text.contains("```\nprint(\"hi\")\nprint(\"bye\")\n```"),
        "{text}"
    );
    assert!(!text.contains("Copy"), "{text}");
    // A table in a closed <details> keeps its cells.
    assert!(text.contains("| Adult | 653.00 |"), "{text}");
    // A cell's visible text over its aria-label.
    assert!(text.contains("| 1 | Manchester City |"), "{text}");
    assert!(!text.contains("View Team profile"), "{text}");
    // A converter's result in its form, without its controls and labels.
    assert!(text.contains("100 USD = 92.50 EUR"), "{text}");
    assert!(
        !text.contains("Convert") && !text.contains("Amount"),
        "{text}"
    );
    // A sign-in form is all controls.
    assert!(!text.contains("Forgot password?"), "{text}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn values_read_in_their_place_and_apart() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-read-values", true);
    let text = read(&mut c, port, "/values");
    // Spanned cells keep the columns aligned.
    assert!(
        text.contains("| Battery | Type | Li-Ion 3582 mAh |"),
        "{text}"
    );
    assert!(
        text.contains("| Battery | Charging | Wired, PD2.0 |"),
        "{text}"
    );
    assert!(text.contains("| Price |  | $999 |"), "{text}");
    // Result values drawn as buttons are read; control labels aren't.
    assert!(text.contains("- New York\n  11:00"), "{text}");
    assert!(text.contains("- Tokyo\n  00:00"), "{text}");
    assert!(text.contains("Mon 17"), "{text}");
    for control in ["Copy", "Submit", "- 2", "Save 20%"] {
        assert!(!text.contains(control), "{control}: {text}");
    }
    // A closed <details>' blocks stay apart.
    assert!(
        text.contains("Restore your status as a student Restore your status ($246.25)"),
        "{text}"
    );
    // Boxes apart on screen read apart; touching ones stay one word.
    assert!(text.contains("63.5k 41 gold badges"), "{text}");
    assert!(text.contains("\nHello"), "{text}");
    assert!(text.contains("PD2.0 25W, 3582mAh, 15W MagSafe"), "{text}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_package_pages_facts_beside_its_readme_are_read() {
    let port = serve_pages(long_pages());
    let mut c = Client::spawn("test-read-package", true);
    let text = read(&mut c, port, "/package");
    assert!(text.contains("Widget validates schemas"), "{text}");
    assert!(
        text.contains("4.6.7 • Public • Published 3 days ago"),
        "{text}"
    );
    assert!(text.contains("### License\n\nMIT"), "{text}");
    assert!(text.contains("### Last publish\n\n3 days ago"), "{text}");
    // A list of links and a long column beside it are not facts about it.
    assert!(!text.contains("src/parse.ts"), "{text}");
    assert!(!text.contains("Another package you may like"), "{text}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_plain_text_document_is_digested_where_the_address_points() {
    let port = serve_pages(long_pages());
    let mut c = Client::spawn("test-read-rfc", true);
    let text = read(&mut c, port, "/rfc#section-4.1.2");
    // `read` shows the document as it is, without the fragment's mark.
    assert!(text.contains("minutes is RECOMMENDED"), "{text}");
    assert!(!text.contains("aibrowse:"), "{text}");
    let digest = c.ok("read", json!({ "view": "digest" }));
    assert!(digest.contains("selected passages"), "{digest}");
    assert!(
        digest.contains("4.1.2.  Authorization Response"),
        "{digest}"
    );
    assert!(digest.contains("minutes is RECOMMENDED"), "{digest}");
    assert!(!digest.contains("aibrowse:"), "{digest}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn labels_that_are_not_text_are_read() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-read-labels", true);
    let text = read(&mut c, port, "/labels");
    // A caption on a line of its own before its table.
    assert!(text.contains("Oct - 2026\n\n| Mon | Tue |"), "{text}");
    // Icon headers by their hidden name or title.
    assert!(text.contains("|  | Chrome | Firefox |"), "{text}");
    // No glyph names of an icon font.
    for glyph in ["swap_vert", "arrow_upward_alt", "expand_more"] {
        assert!(!text.contains(glyph), "{glyph}: {text}");
    }
    assert!(text.contains("| Party | Seats |"), "{text}");
    assert!(text.contains("| S | 99 |"), "{text}");
    assert!(text.contains("Sort by size."), "{text}");
    assert!(text.contains("\nShow all parties\n"), "{text}");
    // Answers are headed by what they are, the accepted one once.
    assert!(
        text.contains(
            "# Answer\n\nUse git revert.\n\n# Accepted answer\n\n30172\n\nUse git reset HEAD~."
        ),
        "{text}"
    );
    assert_eq!(text.matches("Accepted answer").count(), 1, "{text}");
    c.ok("session_close", json!({}));
}

#[test]
#[ignore = "needs Chrome; run with `make test-live`"]
fn a_digest_waits_for_a_page_still_rendering() {
    let port = serve_pages(PAGES);
    let mut c = Client::spawn("test-read-late", true);
    c.ok(
        "navigate",
        json!({ "url": format!("http://localhost:{port}/late"), "observe": false }),
    );
    let digest = c.ok("read", json!({ "view": "digest" }));
    assert!(digest.contains("Malicious code was discovered"), "{digest}");
    c.ok("session_close", json!({}));
}
