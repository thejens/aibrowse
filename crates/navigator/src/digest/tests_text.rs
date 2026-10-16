//! Digest tests on text shapes: plain-text documents in `<pre>`, the address's
//! fragment, labels before tables, and passages alike but for their numbers.

use aibrowse_perception::FRAGMENT_MARK;

use super::blocks::blocks;
use super::choose;
use super::render::render;
use super::select::{about, select};

/// An RFC as rfc-editor.org sets it: each printed page a `<pre>`, sections numbered,
/// paragraphs apart at blank lines. The fragment mark stands before §4.1.2.
fn rfc(mark: bool) -> String {
    // Paragraphs without numbers, so none are alike (see `alike`).
    let filler = |n: usize| {
        let word = [
            "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth",
        ][n];
        format!(
            "   The authorization server handles the {word} request as the client asks\n   and \
             answers it within the time the client allows for the response.\n"
        )
    };
    let section = |title: &str, n: usize| {
        format!(
            "{title}\n\n{}",
            (0..n).map(filler).collect::<Vec<_>>().join("\n")
        )
    };
    format!(
        "# RFC 6749\n\n```\n{}\n{}\n{}{}\n   code\n         REQUIRED.  A maximum authorization code lifetime of 10\n         \
         minutes is RECOMMENDED.  The client MUST NOT use the code twice.\n\n{}```",
        section("1.  Introduction", 6),
        section("4.1.1.  Authorization Request", 6),
        if mark {
            format!("{FRAGMENT_MARK}\n")
        } else {
            String::new()
        },
        section("4.1.2.  Authorization Response", 1),
        section("4.1.3.  Access Token Request", 8),
    )
}

#[test]
fn a_plain_text_document_splits_into_sections_and_paragraphs() {
    let (b, _) = blocks(&rfc(false));
    let headings: Vec<(usize, &str)> = b
        .iter()
        .filter(|x| x.level > 0)
        .map(|x| (x.level, x.text.as_str()))
        .collect();
    assert_eq!(
        headings,
        [
            (1, "RFC 6749"),
            (2, "1.  Introduction"),
            (4, "4.1.1.  Authorization Request"),
            (4, "4.1.2.  Authorization Response"),
            (4, "4.1.3.  Access Token Request"),
        ]
    );
    // Paragraphs, dedented, each a passage.
    assert!(b.iter().any(|x| {
        x.level == 0
            && x.text
                .starts_with("code\n      REQUIRED.  A maximum authorization code lifetime")
    }));
    assert!(b.iter().all(|x| !x.text.contains("```")));
    // Real code stays whole however long, blank lines and all.
    let code = format!(
        "```\n{}\n\n{}\n```",
        "fn main() { let x = vec![1, 2, 3]; println!(\"{x:?}\"); }\n".repeat(8),
        "if a == b { return Err(e); }\n".repeat(8)
    );
    let (b, _) = blocks(&code);
    assert_eq!(b.len(), 1);
    assert!(b[0].text.starts_with("```\nfn main()"));
}

#[test]
fn what_the_fragment_points_at_comes_first() {
    let (b, _) = blocks(&rfc(true));
    let target: Vec<&str> = b
        .iter()
        .filter(|x| x.target)
        .map(|x| x.text.as_str())
        .collect();
    assert_eq!(target, ["4.1.2.  Authorization Response"]);
    assert!(b.iter().all(|x| !x.text.contains(FRAGMENT_MARK)));
    // The check reads the section the address points at, though no word of the goal
    // is in it and the opening alone fills the budget.
    let text = about(&rfc(true), &[], 1200);
    assert!(text.contains("minutes is RECOMMENDED"), "{text}");
    assert!(text.starts_with("# RFC 6749"), "{text}");
    // Without the fragment, the opening.
    assert!(!about(&rfc(false), &[], 1200).contains("minutes is RECOMMENDED"));
    // A short page is read whole, without the mark.
    let short = format!("Intro.\n\n{FRAGMENT_MARK}\n\n## Part\n\nBody.");
    assert_eq!(about(&short, &[], 1200), "Intro.\n\n## Part\n\nBody.");
    // The mark before a heading outside code marks the heading.
    let (b, _) = blocks(&short);
    assert!(b[1].target && b[1].level == 2, "{b:?}");
}

#[tokio::test]
async fn a_digest_keeps_what_the_fragment_points_at() {
    let page = rfc(true);
    let (b, tables) = blocks(&page);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    let choice = choose(&b, &tables, &passages, 2000, None, None, 300).await;
    let text = render(&b, &tables, &choice.kept, Some(300));
    assert!(text.contains("minutes is RECOMMENDED"), "{text}");
    assert!(
        text.contains("#### 4.1.2.  Authorization Response\nThe authorization server"),
        "{text}"
    );
}

#[test]
fn a_short_line_before_a_table_heads_it() {
    let page = "# Rates\n\nOct - 2026\n\n| Mon | Tue |\n| --- | --- |\n| 5 11.25 | 6 11.24 |\n\n\
                Sep - 2026\n\n| Mon | Tue |\n| --- | --- |\n| 7 11.16 | 8 11.15 |\n\n\
                A longer paragraph that is not a label for the table that follows it at all.\n\n\
                | 1 | 2 |";
    let (b, tables) = blocks(page);
    assert_eq!(tables[0], "Oct - 2026\n| Mon | Tue |\n| --- | --- |");
    assert_eq!(tables[1], "Sep - 2026\n| Mon | Tue |\n| --- | --- |");
    assert_eq!(tables[2], "");
    assert!(b.iter().all(|x| x.text != "Oct - 2026"));
    // A row kept alone still says what it is of.
    let row = b.iter().position(|x| x.text.contains("7 11.16")).unwrap();
    let text = render(&b, &tables, &[row], Some(500));
    assert!(text.contains("Sep - 2026\n| Mon | Tue |"), "{text}");
}

#[test]
fn passages_alike_but_for_numbers_are_counted_past_a_few() {
    let names = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Today",
        "Tomorrow",
        "Later",
    ];
    let days = (10..20)
        .zip(names)
        .map(|(d, name)| {
            format!("{name} forecast with its own words, rain {d} mm\n\nSunrise at 07:{d} and sunset at 18:{d}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let page = format!("# Forecast\n\n{days}");
    let (b, tables) = blocks(&page);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    let kept = select(&b, &passages, &vec![0.9; passages.len()], 2000);
    let sunrises = kept
        .iter()
        .filter(|&&i| b[i].text.starts_with("Sunrise"))
        .count();
    assert_eq!(sunrises, 3);
    let text = render(&b, &tables, &kept, Some(2000));
    assert!(
        text.contains("Sunrise at 07:12 and sunset at 18:12\n(7 more like it)"),
        "{text}"
    );
    // The day lines differ in words, and all stay.
    assert_eq!(text.matches("forecast with its own words").count(), 10);
    // Kept whole, nothing is counted.
    let all: Vec<usize> = passages.clone();
    assert!(!render(&b, &tables, &all, None).contains("more like it"));
}

#[test]
fn a_printed_pages_running_head_and_foot_are_left_out() {
    let page = |n: usize| {
        format!(
            "```\nRFC 6749                        OAuth 2.0                   October 2012\n\n{}\n\
             Hardt                        Standards Track                   [Page {n}]\n```",
            "   The client sends its request, and the server answers it within the time\n   \
             that the client allows for its response, as the earlier sections say.\n\n"
                .repeat(5)
        )
    };
    let doc = (1..=4).map(page).collect::<Vec<_>>().join("\n\n");
    let (b, _) = blocks(&doc);
    assert!(b.iter().any(|x| x.text.starts_with("The client sends")));
    assert!(
        b.iter()
            .all(|x| !x.text.contains("Standards Track") && !x.text.contains("October 2012")),
        "{b:?}"
    );
}
