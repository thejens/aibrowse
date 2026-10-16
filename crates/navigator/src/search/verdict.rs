//! The search's own verdict on the pages its results led to, all in one request, by
//! the passages each page has about the criterion.
//!
//! A branch's reached check asks whether a page is its goal by the first few thousand
//! characters about the goal's words. On a long page that cut misses what decides it:
//! Stack Overflow's canonical "undo the most recent local commits" (44k characters)
//! scored 0.49 and ran out of steps, while a short duplicate question passed. And it
//! doesn't ask where a page comes from, so for "the accepted Stack Overflow answer" a
//! DigitalOcean page passed. Here every page left open is judged again on the passages
//! picked for the criterion: a page a criterion's source rules out is dropped, and one
//! that ran out of steps or time is judged rather than left to the caller.

use serde_json::{Map, Value, json};

use crate::jev::Jev;

/// How sure Jev must be that a page ran out of steps or time is what the criterion
/// asks for, to count it, as sure as a branch's own reached check must be.
const FULFILS: f64 = 0.6;
/// How sure Jev must be that the criterion names a source the page isn't from, to
/// drop it: a clear yes, as a page that merely quotes the source is from elsewhere too.
const FROM_ELSEWHERE: f64 = 0.6;

/// A page a result led to, as the verdict sees it.
pub(super) struct Page<'a> {
    pub title: &'a str,
    pub url: &'a str,
    /// The passages about the criterion (`passages_for`).
    pub excerpt: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Verdict {
    /// How likely the page is what the criterion asks for.
    pub fulfils: f64,
    /// How likely the criterion names a site, publisher or kind of source the page
    /// isn't from.
    pub elsewhere: f64,
}

impl Verdict {
    /// The criterion names a source and the page is from another one.
    pub fn is_from_elsewhere(self) -> bool {
        self.elsewhere >= FROM_ELSEWHERE
    }

    /// The page's text shows it is what the criterion asks for, from the source it
    /// names if it names one.
    pub fn fulfils(self) -> bool {
        self.fulfils >= FULFILS && !self.is_from_elsewhere()
    }
}

/// Jev's verdict on each of `pages`, in order. `None` when Jev can't be reached: the
/// pages then stand as their branches left them.
pub(super) async fn judge(jev: &Jev, criterion: &str, pages: &[Page<'_>]) -> Option<Vec<Verdict>> {
    if pages.is_empty() {
        return Some(Vec::new());
    }
    let mut state = json!({ "criterion": criterion, "today": crate::today::today() });
    for (i, page) in pages.iter().enumerate() {
        state[format!("page_{i}")] =
            json!({ "title": page.title, "url": page.url, "text": page.excerpt });
    }
    match jev.ask(&state, &questions(pages.len())).await {
        Ok(answers) => Some(verdicts(&answers, pages.len())),
        Err(err) => {
            tracing::warn!(error = %err, "could not judge the search's pages: kept as found");
            None
        }
    }
}

fn questions(pages: usize) -> Value {
    let mut questions = Map::new();
    for i in 0..pages {
        questions.insert(
            format!("fulfils_{i}"),
            json!({
                "type": "noul",
                "instructions": format!("Judging by its title, address and text, is `page_{i}` what `criterion` asks for?"),
                "criteria": {
                    "true": "Yes: it is that page, or holds exactly what `criterion` asks for",
                    "false": "No: it is a different or more general page, or what `criterion` requires of it (its kind of answer, its date, its subject) isn't there"
                }
            }),
        );
        // Asked on its own: folded into the question above, a page holding the right
        // answer from another site still read as what was asked for.
        questions.insert(
            format!("elsewhere_{i}"),
            json!({
                "type": "noul",
                "instructions": format!("Does `criterion` say where the page must come from, and is `page_{i}` from somewhere else? Judge by its address and title."),
                "criteria": {
                    "true": "Yes: `criterion` names a site (\"on Stack Overflow\", \"on GitHub\"), a publisher or an authority, or a kind of source (\"the official documentation\", \"the manufacturer's\"), and this page is from none of the sources it allows (a tutorial site, a blog, a forum, a third party's copy)",
                    "false": "No: `criterion` names no source, or this page is from one of those it allows (with \"the agency or a retailer\", a retailer's page is)"
                }
            }),
        );
    }
    Value::Object(questions)
}

/// The verdicts in `answers`, one per page. An answer missing from the reply neither
/// counts a page nor drops it.
fn verdicts(answers: &Value, pages: usize) -> Vec<Verdict> {
    let p = |name: String| answers[name]["noul"].as_f64();
    (0..pages)
        .map(|i| Verdict {
            fulfils: p(format!("fulfils_{i}")).unwrap_or(0.0),
            elsewhere: p(format!("elsewhere_{i}")).unwrap_or(0.0),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_gets_both_questions() {
        let questions = questions(2);
        for name in ["fulfils_0", "elsewhere_0", "fulfils_1", "elsewhere_1"] {
            assert_eq!(questions[name]["type"], "noul", "{name}");
        }
        assert!(
            questions["elsewhere_1"]["instructions"]
                .as_str()
                .unwrap()
                .contains("`page_1`")
        );
    }

    #[test]
    fn verdicts_read_each_page_and_default_missing_answers() {
        let answers = json!({
            "fulfils_0": { "noul": 0.82 },
            "elsewhere_0": { "noul": 0.9 },
            "fulfils_1": { "noul": 0.7 },
        });
        assert_eq!(
            verdicts(&answers, 2),
            [
                Verdict {
                    fulfils: 0.82,
                    elsewhere: 0.9
                },
                Verdict {
                    fulfils: 0.7,
                    elsewhere: 0.0
                },
            ]
        );
    }

    #[test]
    fn a_page_from_another_source_never_fulfils() {
        let digitalocean = Verdict {
            fulfils: 0.9,
            elsewhere: 0.85,
        };
        assert!(digitalocean.is_from_elsewhere());
        assert!(!digitalocean.fulfils());
        let canonical = Verdict {
            fulfils: 0.75,
            elsewhere: 0.05,
        };
        assert!(canonical.fulfils());
        let unsure = Verdict {
            fulfils: 0.5,
            elsewhere: 0.05,
        };
        assert!(!unsure.fulfils() && !unsure.is_from_elsewhere());
    }
}
