//! Digest tests: cutting markdown into passages, choosing them and showing them.

use super::blocks::{blocks, unlink};
use super::render::render;
use super::select::{about, candidates, omitted_sections, openers, select, stem, tokens};
use super::{Choice, excerpt, header_title, keep, page_text};

const PAGE: &str = "# Pricing\n\nIntro text.\n\n## Teams\n\nTeams cost $10 per seat.\n- SSO\n- Audit log\n\n## FAQ\n\nLong answer.\n\n### Refunds\n\nWithin 30 days.";

#[test]
fn markdown_splits_into_headings_and_passages() {
    let (b, _) = blocks(PAGE);
    assert_eq!(b[0].text, "Pricing");
    assert_eq!(b[0].level, 1);
    assert_eq!(b[3].text, "Teams cost $10 per seat.\n- SSO\n- Audit log");
    assert_eq!(b.iter().filter(|x| x.level == 0).count(), 4);
}

#[test]
fn kept_passages_come_with_their_headings_in_page_order() {
    let (b, _) = blocks(PAGE);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    // Keep the Teams passage and the Refunds one.
    let scores = vec![0.1, 0.9, 0.1, 0.8];
    let kept = select(&b, &passages, &scores, 500);
    let text = render(&b, &[], &kept, Some(500));
    assert_eq!(
        text,
        "# Pricing\n## Teams\nTeams cost $10 per seat.\n- SSO\n- Audit log\n\n## FAQ\n### Refunds\nWithin 30 days.\n"
    );
    assert_eq!(omitted_sections(&b, &kept), Vec::<String>::new());
}

#[test]
fn sections_with_nothing_kept_are_named() {
    let (b, _) = blocks(PAGE);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    let kept = select(&b, &passages, &[0.0, 0.9, 0.0, 0.0], 500);
    assert_eq!(omitted_sections(&b, &kept), ["FAQ", "Refunds"]);
}

#[test]
fn the_budget_holds_and_long_passages_are_cut() {
    let long = format!("# T\n\n{}\n\nshort", "word ".repeat(400));
    let (b, _) = blocks(&long);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    let kept = select(&b, &passages, &[0.9, 0.8], 120);
    assert_eq!(kept.len(), 2);
    let text = render(&b, &[], &kept, Some(120));
    assert!(tokens(&text) <= 120 + 10, "{}", tokens(&text));
    assert!(text.contains('…'));
}

#[test]
fn without_jev_section_openers_win() {
    let (b, _) = blocks(PAGE);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    assert_eq!(openers(&b, &passages), vec![1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn tables_split_into_rows_under_their_header_and_code_stays_whole() {
    let page = "# Ops\n\n| Op | Meaning |\n| --- | --- |\n| -> | field |\n| ->> | field as text |\n| #> | path |\n\n```\nfoo(a)\n\nfoo(a, b)\n```\n\nSee [the docs](https://x.org/d) ![x] [image: logo] now.";
    let (b, tables) = blocks(page);
    assert_eq!(tables, ["| Op | Meaning |\n| --- | --- |"]);
    let rows: Vec<&str> = b
        .iter()
        .filter(|x| x.table.is_some())
        .map(|x| x.text.as_str())
        .collect();
    assert_eq!(
        rows,
        ["| -> | field |", "| ->> | field as text |", "| #> | path |"]
    );
    assert!(b.iter().any(|x| x.text == "```\nfoo(a)\n\nfoo(a, b)\n```"));
    assert!(b.iter().any(|x| x.text == "See the docs ![x]  now."));
    assert_eq!(
        unlink("3. [image: Night: heavy rain] "),
        "3. Night: heavy rain"
    );
    assert_eq!(unlink("[image: cloudy]"), "cloudy");
    assert_eq!(
        unlink("[image: logo] [Home](https://x.org/)").trim(),
        "Home"
    );
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    // Keep the second and third rows only.
    let scores: Vec<f64> = passages
        .iter()
        .map(|&i| {
            if b[i].text.contains(">>") || b[i].text.contains("#>") {
                0.9
            } else {
                0.0
            }
        })
        .collect();
    let kept = select(&b, &passages, &scores, 500);
    let text = render(&b, &tables, &kept, Some(500));
    assert_eq!(
        text,
        "# Ops\n| Op | Meaning |\n| --- | --- |\n| ->> | field as text |\n| #> | path |\n"
    );
}

#[test]
fn a_list_after_prose_is_a_passage_of_its_own() {
    let page = "# Hybrid bikes\nOur hybrid bikes suit commuting.\nWe stock three brands.\n\
                - [image: Windfield - productimage]\nNakamura Windfield\n6999 kr\n\
                - [image: Alfa - productimage]\nNakamura Alfa\n4999 kr";
    let (blocks, _) = blocks(page);
    let passages: Vec<&str> = blocks
        .iter()
        .filter(|b| b.level == 0)
        .map(|b| b.text.as_str())
        .collect();
    assert_eq!(passages.len(), 2, "{passages:?}");
    assert!(passages[1].starts_with("- Windfield"), "{passages:?}");
    assert!(passages[1].contains("4999 kr"), "{passages:?}");
}

#[test]
fn a_check_reads_the_opening_and_the_passages_about_the_goal() {
    let mut page = String::from("# Serde JSON\n\nSerde is a framework for serializing data.\n\n");
    for n in 0..40 {
        page.push_str(&format!(
            "## Section {n}\n\nSome long text about parsing values number {n}.\n\n"
        ));
    }
    page.push_str(
        "## License\n\nLicensed under either of Apache License, Version 2.0 or MIT license.\n",
    );
    let words = vec![
        "license".to_owned(),
        "serde_json".to_owned(),
        "crate".to_owned(),
    ];
    let text = about(&page, &words, 600);
    assert!(
        text.starts_with("# Serde JSON\nSerde is a framework"),
        "{text}"
    );
    assert!(text.contains("## License\nLicensed under either"), "{text}");
    assert!(!text.contains("number 39"), "{text}");
    // A plural finds its singular in a heading.
    let mut jobs = String::from("# Careers\n\nWe build things.\n\n");
    for n in 0..40 {
        jobs.push_str(&format!(
            "## Story {n}\n\nA long story about open source number {n}.\n\n"
        ));
    }
    jobs.push_str("## Job Postings\n\n- Engineering Manager\n- Data Engineer\n");
    let words = vec!["careers".to_owned(), "open".to_owned(), "jobs".to_owned()];
    let text = about(&jobs, &words, 600);
    assert!(
        text.contains("## Job Postings\n- Engineering Manager"),
        "{text}"
    );
    // A table's rows are found by the words of its header and introduction.
    let mut releases = String::from("# Active kernel releases\n\nThere are several kinds.\n\n");
    for kind in ["Prepatch", "Mainline", "Stable"] {
        releases.push_str(&format!(
            "{kind}\n\nA long description of the {kind} kernels and how they are made.\n\n"
        ));
    }
    for n in 0..30 {
        releases.push_str(&format!("Background paragraph {n} on kernels.\n\n"));
    }
    releases.push_str(
        "There are usually several longterm maintenance kernel releases.\n\n\
         | Version | Released | Projected EOL |\n| --- | --- | --- |\n\
         | 6.12 | 2024-11-17 | Dec, 2028 |\n| 6.6 | 2023-10-29 | Dec, 2027 |\n",
    );
    let words = vec!["longterm".to_owned(), "lts".to_owned(), "life".to_owned()];
    let text = about(&releases, &words, 600);
    assert!(text.contains("| 6.12 | 2024-11-17 | Dec, 2028 |"), "{text}");
    assert!(text.contains("| 6.6 | 2023-10-29 | Dec, 2027 |"), "{text}");
    // A page that fits is read whole.
    assert_eq!(about("short page", &words, 600), "short page");
}

#[test]
fn a_check_reads_on_where_no_passage_uses_the_goals_words() {
    // The goal in English, the page in Swedish: a calendar opens it, the departures
    // follow, and none of them says "train" or "departure".
    let mut page = String::from("# Välj resa\n\n");
    for week in 0..12 {
        page.push_str(&format!(
            "Vecka {week}: måndag, tisdag, onsdag, torsdag, fredag, lördag, söndag.\n\n"
        ));
    }
    page.push_str("## Matchande resor imorgon\n\n");
    for hour in 5..20 {
        page.push_str(&format!(
            "Avgår {hour:02}:13, ankommer {:02}:47, pris från 245 kronor.\n\n",
            hour + 3
        ));
    }
    let words = vec![
        "trains".to_owned(),
        "departure".to_owned(),
        "prices".to_owned(),
    ];
    let text = about(&page, &words, 1200);
    assert!(text.starts_with("# Välj resa\nVecka 0"), "{text}");
    assert!(
        text.contains("## Matchande resor imorgon\nAvgår 05:13"),
        "{text}"
    );
    assert!(text.chars().count() <= 1300, "{}", text.chars().count());
}

#[test]
fn long_prose_splits_into_its_paragraphs() {
    // An article whose paragraphs are separated by single line breaks.
    let paragraph = |n: usize| format!("Paragraph {n} says {}.", "something ".repeat(20));
    let article = format!(
        "# Trial\n\n{}",
        (0..6).map(paragraph).collect::<Vec<_>>().join("\n")
    );
    let (b, _) = blocks(&article);
    let passages: Vec<&str> = b
        .iter()
        .filter(|x| x.level == 0)
        .map(|x| x.text.as_str())
        .collect();
    assert_eq!(passages.len(), 6, "{passages:?}");
    assert!(passages[5].starts_with("Paragraph 5"), "{passages:?}");
    // Short lines stay together: a card's name and price are one passage.
    let (b, _) = blocks("Nakamura Alfa\n4999 kr");
    assert_eq!(b.len(), 1);
}

#[test]
fn a_cut_or_partial_digest_says_so() {
    let page = format!("# Trial\n\n{}\n\nShort.", "word ".repeat(400));
    let (b, tables) = blocks(&page);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    // Both passages are kept, but the long one is cut.
    let choice = keep(&b, &passages, &[0.9, 0.9], 300);
    assert_eq!(choice.kept.len(), 2);
    let text = page_text(&b, &tables, &passages, 600, &choice, None, 300);
    assert!(text.contains("selected passages"), "{text}");
    assert!(text.contains("read returns the full text"), "{text}");
    // A page that fits is whole, however long its passages.
    let whole = Choice {
        kept: passages.clone(),
        unmatched: false,
        whole: true,
    };
    let text = page_text(&b, &tables, &passages, 600, &whole, None, 1000);
    assert!(!text.contains('…') && !text.contains("selected"), "{text}");
}

#[test]
fn when_nothing_matches_the_openings_are_kept_and_said_to_be() {
    let (b, tables) = blocks(PAGE);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    let choice = keep(&b, &passages, &[0.0; 4], 10);
    assert!(choice.unmatched);
    assert!(!choice.kept.is_empty());
    let text = page_text(&b, &tables, &passages, 50, &choice, Some("refund"), 10);
    assert!(
        text.contains("nothing on the page was judged to be about the focus"),
        "{text}"
    );
    assert!(text.contains("Intro text."), "{text}");
}

#[test]
fn a_long_pages_late_sections_are_judged_when_the_focus_names_them() {
    let mut page = String::from("# Tournament\n\n");
    for round in 0..40 {
        page.push_str(&format!("## Round {round}\n\n"));
        for game in 0..5 {
            page.push_str(&format!("| Team {game} | 2–1 | Team {} |\n", game + 5));
        }
        page.push('\n');
    }
    page.push_str("## Final\n\nSpain beat England 2–1 in Berlin.\n");
    let (b, tables) = blocks(&page);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    assert!(passages.len() > 200);
    let chosen = candidates(&b, &tables, &passages, &["final".to_owned()], 100);
    assert_eq!(chosen.len(), 100);
    let last = passages.len() - 1;
    assert!(chosen.contains(&last), "{chosen:?}");
    assert!(chosen.windows(2).all(|w| w[0] < w[1]), "page order");
    // Without a focus, every section's opening is judged before later rows.
    let chosen = candidates(&b, &tables, &passages, &[], 100);
    assert!(chosen.contains(&last), "{chosen:?}");
}

#[test]
fn a_long_passage_is_judged_by_what_it_says_about_the_focus() {
    let text = format!(
        "{} The fee is 653 dollars.",
        "Filler words here. ".repeat(60)
    );
    let shown = excerpt(&text, &[stem("fee")]);
    assert!(shown.contains("653 dollars"), "{shown}");
    assert!(shown.chars().count() < 450, "{}", shown.chars().count());
    assert_eq!(excerpt("short", &[stem("fee")]), "short");
}

#[test]
fn a_heading_that_is_the_focus_beats_headings_containing_it() {
    // Wikipedia's knockout stage: hundreds of line-up rows under "Quarter-finals" and
    // "Semi-finals" match the stem of "final", ahead of the "Final" itself.
    let mut page = String::from("# Knockout stage\n\nSixteen teams played.\n\n");
    for (round, games) in [("Quarter-finals", 4), ("Semi-finals", 2)] {
        page.push_str(&format!("## {round}\n\n"));
        for game in 0..games {
            page.push_str(&format!("### Game {round} {game}\n\nA close match.\n\n"));
            page.push_str("| Pos | Player |\n| --- | --- |\n");
            for player in 0..60 {
                page.push_str(&format!("| MF | Player {player} |\n"));
            }
            page.push('\n');
        }
    }
    page.push_str("## Final\n\nArgentina beat France on penalties.\n\n");
    page.push_str("| Pos | Player |\n| --- | --- |\n| GK | Martínez |\n| FW | Messi |\n\n");
    page.push_str("## Statistics\n\nThe final drew 88,966.\n");
    let (b, tables) = blocks(&page);
    let passages: Vec<usize> = (0..b.len()).filter(|&i| b[i].level == 0).collect();
    assert!(passages.len() > 300);
    let chosen = candidates(&b, &tables, &passages, &["final".to_owned()], 100);
    assert_eq!(chosen.len(), 100);
    let text = |p: usize| b[passages[p]].text.as_str();
    for wanted in ["Argentina beat France", "Messi", "Martínez", "88,966"] {
        assert!(
            chosen.iter().any(|&p| text(p).contains(wanted)),
            "{wanted} missing"
        );
    }
    // The stem's matches take turns by section: every game's rows are judged, not
    // just the first games' in page order.
    for round in ["Quarter-finals 3", "Semi-finals 1"] {
        let heading = b
            .iter()
            .position(|x| x.text == format!("Game {round}"))
            .unwrap();
        assert!(
            chosen.iter().any(|&p| passages[p] > heading
                && passages[p] < heading + 60
                && b[passages[p]].table.is_some()),
            "no rows of {round}"
        );
    }
}

#[test]
fn a_page_without_a_title_is_headed_by_its_documents_name() {
    let pdf = "# fw4.pdf\n\n(PDF, 5 pages)\n\n## Page 1\n\nStep 4(c)";
    let url = "https://www.irs.gov/pub/irs-pdf/fw4.pdf";
    assert_eq!(header_title("", pdf, url), "fw4.pdf");
    assert_eq!(header_title(url, pdf, url), "fw4.pdf");
    assert_eq!(header_title("Form W-4", pdf, url), "Form W-4");
    assert_eq!(header_title("", "Some text", "https://x.org/a/b?c=1"), "b");
    assert_eq!(header_title("", "", "about:blank"), "(untitled)");
}
