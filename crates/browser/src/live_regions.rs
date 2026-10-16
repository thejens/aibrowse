//! Live regions heard during an action: the "Saved" and "Error: …" toasts SPAs show for
//! a few seconds, reported with the action's result (see `MUTATION_COUNTER`).

use std::time::Duration;

use crate::isolated::TAKE_ANNOUNCEMENTS;
use crate::tab::Tab;

/// Messages per note; a chatty region (a progress counter) shouldn't flood the result.
const MAX_SHOWN: usize = 3;
/// The messages are a bonus; a renderer that stopped answering mustn't stall the action.
const TAKE_TIMEOUT: Duration = Duration::from_secs(1);

impl Tab {
    /// What the page's live regions said since the last call (isolated.rs).
    pub(crate) async fn take_announcements(&mut self) -> Vec<String> {
        let session = self.session.clone();
        let take = async {
            let world = self.world().await.ok()?;
            let value = world.evaluate(&session, TAKE_ANNOUNCEMENTS).await.ok()?;
            serde_json::from_value(value).ok()
        };
        tokio::time::timeout(TAKE_TIMEOUT, take)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}

/// Live-region messages as one note. They are how SPAs say "Saved" or "Error: …", often
/// in a toast that is gone by the next observe. The page wrote them, so they are quoted
/// and labelled; repeats are said once, a message the next one revises gives way to it,
/// and the latest messages win.
pub(crate) fn announcement_note(announced: &[String]) -> String {
    let mut unique: Vec<&String> = Vec::new();
    for text in announced {
        if unique.contains(&text) {
            continue;
        }
        if unique.last().is_some_and(|last| revises(text, last)) {
            unique.pop();
        }
        unique.push(text);
    }
    let skip = unique.len().saturating_sub(MAX_SHOWN);
    let shown: Vec<String> = unique[skip..].iter().map(|t| format!("{t:?}")).collect();
    format!(
        "the page announced (live region, page text): {}",
        shown.join(" | ")
    )
}

/// Whether `next` is `earlier` said again with a few words changed: a combobox announcing
/// its results for each letter typed ("…for search term Mat." then "…Math."). Short
/// messages ("1 item", "2 items") are distinct news, so only long ones are compared.
fn revises(next: &str, earlier: &str) -> bool {
    const LONG: usize = 30;
    let (a, b): (Vec<char>, Vec<char>) = (earlier.chars().collect(), next.chars().collect());
    let longer = a.len().max(b.len());
    if a.len().min(b.len()) < LONG {
        return false;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (prefix + suffix) * 5 >= longer * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_are_said_once_and_the_latest_kept() {
        let said: Vec<String> = ["Saving…", "Saved", "Saved", "1 item", "2 items"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            announcement_note(&said),
            "the page announced (live region, page text): \"Saved\" | \"1 item\" | \"2 items\""
        );
    }

    #[test]
    fn a_message_revised_as_the_user_types_is_said_once() {
        let said: Vec<String> = ["Mat", "Math", "Maths"]
            .into_iter()
            .map(|term| format!("Maths, 1 of 1. 1 result available for search term {term}. Use Up and Down to choose options."))
            .collect();
        assert_eq!(
            announcement_note(&said),
            format!("the page announced (live region, page text): {:?}", said[2])
        );
    }
}
