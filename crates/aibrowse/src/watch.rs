//! Noticing that the user has finished a handoff, so they needn't press Done.
//!
//! While a handoff is pending, the page is checked every few seconds and Jev is asked
//! whether the user is through the task (e.g. signed in and on the site's normal pages).
//! The page outline goes only to Jev; the agent still sees nothing until the handoff ends.

use std::sync::Arc;
use std::time::Duration;

use aibrowse_handoff::HandoffServer;
use aibrowse_navigator::Jev;
use aibrowse_perception::{Detail, observe};
use serde_json::json;
use tokio::sync::Mutex;

use crate::session::Live;

const CHECK_EVERY: Duration = Duration::from_secs(3);
const MAX_OUTLINE_CHARS: usize = 6000;
/// Probability of "through" that counts as a yes.
const CONFIDENT: f64 = 0.6;
/// How many of the pages visited during the handoff Jev sees.
const TRAIL: usize = 5;
/// Consecutive yeses needed: a single read can catch a page mid-redirect.
const REQUIRED: u32 = 2;

pub async fn complete_when_done(
    live: Arc<Mutex<Option<Live>>>,
    server: HandoffServer,
    jev: Jev,
    id: String,
    task: String,
) {
    // Jev sees what a person watching would: the task, where the user started, the
    // pages they went through, and where they are now. "Done" judged from one page
    // alone is ambiguous; against that history it is not.
    let Some(start) = snapshot(&live).await else {
        return;
    };
    let mut trail: Vec<String> = Vec::new();
    let mut confident = 0;
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        if server.pending().as_deref() != Some(id.as_str()) {
            return; // resolved some other way
        }
        let Some(now) = snapshot(&live).await else {
            continue; // mid-navigation; try again shortly
        };
        if now == start {
            confident = 0;
            continue; // nothing has happened yet; no need to ask
        }
        let heading = page_heading(&now);
        if trail.last() != Some(&heading) {
            trail.push(heading);
            if trail.len() > TRAIL {
                trail.remove(0);
            }
        }
        let state = json!({
            "task": task,
            "page_when_handed_over": start,
            "pages_since": trail,
            "page_now": now,
        });
        let questions = json!({
            "progress": {
                "type": "choice",
                "instructions": "A human was handed this browser to do `task`, starting at `page_when_handed_over`, and has since visited `pages_since`. Where are they now, judging by `page_now`?",
                "criteria": {
                    "same_step": "Still on the step they were handed, or a retry of it (e.g. the sign-in form again, perhaps with an error)",
                    "follow_up": "On a further step of the same task that still needs them: a verification or 2FA code, a CAPTCHA, a confirmation, choosing an account",
                    "through": "Through the task: on the site's normal pages, e.g. a home page, feed or dashboard after signing in"
                }
            }
        });
        match jev.ask(&state, &questions).await {
            Ok(answers) => {
                let through = answers["progress"]["probabilities"]["through"]
                    .as_f64()
                    .unwrap_or(0.0);
                let top = answers["progress"]["choice"].as_str().unwrap_or_default();
                tracing::debug!(through, top, "handoff progress check");
                confident = if top == "through" && through >= CONFIDENT {
                    confident + 1
                } else {
                    0
                };
                if confident >= REQUIRED {
                    server.complete(&id);
                    return;
                }
            }
            Err(err) => tracing::warn!(%err, "could not check handoff progress"),
        }
    }
}

/// The title and URL lines of an outline: enough to show the path through a flow.
fn page_heading(outline: &str) -> String {
    outline.lines().take(2).collect::<Vec<_>>().join(" — ")
}

/// The tab's URL and outline, or `None` if it can't be observed right now.
async fn snapshot(live: &Arc<Mutex<Option<Live>>>) -> Option<String> {
    let mut guard = live.lock().await;
    let live = guard.as_mut()?;
    let model = observe(live.tab(), Detail::Interactive, None).await.ok()?;
    Some(model.render().chars().take(MAX_OUTLINE_CHARS).collect())
}
