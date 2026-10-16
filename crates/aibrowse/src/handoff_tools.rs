//! Tools for working with the user: handing them the browser, and waiting for them.

use std::sync::Arc;

use aibrowse_handoff::{Kind, Status as HandoffStatus};
use aibrowse_navigator::Jev;
use aibrowse_perception::Detail;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ErrorData as McpError, tool, tool_router};

use crate::confirm::{Parked, perform};
use crate::network_tools::execute;
use crate::respond::{
    after_action, failure, notify_user, open_in_browser, outline, parse_ref, text,
};
use crate::server::Aibrowse;
use crate::tools::{HandoffParams, HandoffStatusParams, WatchParams};
use crate::trace::notes;
use crate::watch::complete_when_done;

#[tool_router(router = handoff_router, vis = "pub(crate)")]
impl Aibrowse {
    #[tool(
        description = "A read-only live view of the browser for the user to keep open while you work. It follows your active tab and turns into the controls whenever you request a handoff or approval. Offer it when the user wants to watch."
    )]
    async fn watch(
        &self,
        Parameters(p): Parameters<WatchParams>,
    ) -> Result<CallToolResult, McpError> {
        let server = match self.handoff_server().await {
            Ok(server) => server,
            Err(err) => return Ok(failure(err)),
        };
        if let Some(live) = self.live.lock().await.as_mut() {
            self.publish(live);
        }
        let url = server.watch_url();
        let opened = p.open.unwrap_or(true) && open_in_browser(&url);
        Ok(text(if opened {
            format!("Live view opened on this computer: {url}\nIt stays valid for this session.")
        } else {
            format!("Live view: {url}\nGive the user this link; it stays valid for this session.")
        }))
    }

    #[tool(
        description = "Hand the browser to the user via a live view link, for things only they can do: logging in, 2FA, CAPTCHAs, confirming payments. Then wait with handoff_status."
    )]
    async fn request_handoff(
        &self,
        Parameters(p): Parameters<HandoffParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        if !live.attended() {
            return Ok(failure(
                "this session is unattended: no user is present to hand off to. Report what is needed instead.",
            ));
        }
        let server = match self.handoff_server().await {
            Ok(server) => server,
            Err(err) => return Ok(failure(err)),
        };
        self.publish(live);
        let code_field = match p.code_field.as_deref().map(parse_ref) {
            None => None,
            Some(Ok(field)) if field.target == 0 => Some(field.backend),
            Some(Ok(field)) => {
                return Ok(failure(format!(
                    "{field} is inside a frame; a code relay types into the page itself. Use a plain handoff instead."
                )));
            }
            Some(Err(err)) => return Ok(failure(err)),
        };
        // The person sees how long they have, when the handoff gives up on them.
        let ask = match p.give_up_after_minutes {
            Some(minutes) => format!(
                "{} (If you're away, I'll carry on without this after {} minutes.)",
                p.reason,
                minutes.max(1)
            ),
            None => p.reason.clone(),
        };
        let ticket = match code_field {
            Some(field) => server.open_code(live.tab().session().clone(), &ask, Some(field)),
            None => server.open(
                live.tab().session().clone(),
                Kind::Takeover,
                &ask,
                p.done_url.as_deref(),
            ),
        };
        server.give_up_after(
            &ticket.id,
            p.give_up_after_minutes
                .map(|minutes| std::time::Duration::from_secs(minutes.max(1) * 60)),
        );
        notes::human("handoff", "requested", Some(&ticket.id), &p.reason);
        // Without a goal URL, Jev watches for the user being done (when a key is set). A
        // code relay is done when the code is sent.
        if p.done_url.is_none()
            && code_field.is_none()
            && let Ok(jev) = self.jev.get_or_try_init(|| async { Jev::from_env() }).await
        {
            tokio::spawn(complete_when_done(
                Arc::clone(&self.live),
                server.clone(),
                jev.clone(),
                ticket.id.clone(),
                p.reason.clone(),
            ));
        }
        let watched = server.watchers() > 0;
        let opened = p.open.unwrap_or(true) && notify_user(&ticket.url, &ask, !watched);
        let mut message = format!(
            "Handoff {} started. The user controls the browser at:\n{}\n",
            ticket.id, ticket.url
        );
        if opened && watched {
            message.push_str("It shows up in the user's open live view, with a notification. ");
        } else if opened {
            message.push_str("It was opened on this computer, with a notification. ");
        } else {
            message.push_str("Give the user this link. ");
        }
        message.push_str(
            "Now call handoff_status to wait: it completes when the user presses Done or is detected to be through",
        );
        message.push_str(&match p.give_up_after_minutes {
            Some(minutes) => format!(", or after {minutes} minutes without them."),
            None => ", however long that takes.".to_owned(),
        });
        Ok(text(message))
    }

    #[tool(
        description = "Wait for the user to finish a handoff. Returns pending (call again), done (with the page outline), or cancelled."
    )]
    async fn handoff_status(
        &self,
        Parameters(p): Parameters<HandoffStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(server) = self.handoff.get() else {
            return Ok(failure("no handoff was requested"));
        };
        let Some(id) = server.pending().or_else(|| server.last_id()) else {
            return Ok(failure("no handoff was requested"));
        };
        let wait = std::time::Duration::from_secs(p.wait_seconds.unwrap_or(30).min(55));
        let status = server.wait(&id, wait).await;
        Ok(match status {
            Some(HandoffStatus::Pending) => text(format!(
                "Handoff {id} is still pending: the user hasn't pressed Done yet. Call handoff_status again to keep waiting."
            )),
            Some(HandoffStatus::Cancelled) => match self.take_pending(&id).await {
                Some(pending) => {
                    notes::human("approval", "denied", Some(&id), &pending.summary);
                    text(format!(
                        "The user denied \"{}\". It was not done. Don't retry it; ask the user what they want instead.",
                        pending.summary
                    ))
                }
                None if server.timed_out(&id) => {
                    notes::human("handoff", "timed out", Some(&id), "");
                    self.remember_handoff(&id, "timed out", server.report(&id))
                        .await;
                    text(format!(
                        "Handoff {id} was given up on: the user didn't respond in time (they may be away). Carry on without it: skip what needed them, and say what is still missing when you report.{}",
                        trail_text(server.report(&id))
                    ))
                }
                None => {
                    notes::human("handoff", "cancelled", Some(&id), "");
                    self.remember_handoff(&id, "cancelled", server.report(&id))
                        .await;
                    text(format!(
                        "Handoff {id} was cancelled by the user. Ask them how to proceed.{}",
                        trail_text(server.report(&id))
                    ))
                }
            },
            Some(HandoffStatus::Done) => {
                let pending = self.take_pending(&id).await;
                let mut live = self.live.lock().await;
                if let (Some(pending), Some(live)) = (pending, live.as_mut()) {
                    notes::human("approval", "approved", Some(&id), &pending.summary);
                    // Approved: run exactly what was approved, now.
                    let mut content = vec![ContentBlock::text(format!(
                        "The user approved \"{}\"; done.",
                        pending.summary
                    ))];
                    match &pending.parked {
                        // The approval page shows the browser and takes the user's input:
                        // they may have done it themselves, or the page moved on. Its refs
                        // belong to the page approval was asked on, so it isn't redone.
                        Parked::Act(act)
                            if act.reference.is_some() && live.tab().url() != pending.page =>
                        {
                            content = vec![
                                ContentBlock::text(format!(
                                    "The user approved \"{}\", but the page changed while they decided (now {}): perhaps they did it themselves in the approval view. It was not done again; check the page below.",
                                    pending.summary,
                                    live.tab().url()
                                )),
                                ContentBlock::text(outline(live, Detail::Interactive).await),
                            ];
                        }
                        Parked::Act(act) => {
                            let reference =
                                act.reference.as_deref().and_then(|r| parse_ref(r).ok());
                            match perform(live, act, reference).await {
                                Ok(result) => {
                                    content.extend(after_action(live, result, true).await)
                                }
                                Err(invalid) => return Ok(invalid),
                            }
                        }
                        Parked::Api { request, pick } => {
                            content.extend(execute(live, request, pick.as_deref()).await.content);
                        }
                    }
                    return Ok(CallToolResult::success(content));
                }
                let report = server.report(&id);
                if let Some(live) = live.as_mut() {
                    let trail = report.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
                    live.doing.handoff(&id, "done", trail);
                }
                let trail = trail_text(report);
                notes::human("handoff", "done", Some(&id), trail.trim());
                let mut content = vec![ContentBlock::text(format!(
                    "Handoff {id} is done: the user handed the browser back.{trail}"
                ))];
                if let Some(live) = live.as_mut() {
                    let notes = live.sync_tabs().await;
                    for note in notes {
                        content.push(ContentBlock::text(format!("Note: {note}")));
                    }
                    content.push(ContentBlock::text(outline(live, Detail::Interactive).await));
                }
                CallToolResult::success(content)
            }
            None => failure(format!("unknown handoff {id}")),
        })
    }
}

impl Aibrowse {
    /// Keeps how a handoff ended in the session's journal.
    async fn remember_handoff(&self, id: &str, outcome: &str, report: Option<(bool, Vec<String>)>) {
        if let Some(live) = self.live.lock().await.as_mut() {
            live.doing.handoff(
                id,
                outcome,
                report.map(|(_, trail)| trail).unwrap_or_default(),
            );
        }
    }
}

/// What the user did while they had the browser, for the agent to resume from.
fn trail_text(report: Option<(bool, Vec<String>)>) -> String {
    let Some((_, trail)) = report.filter(|(_, trail)| !trail.is_empty()) else {
        return String::new();
    };
    let mut text = String::from("\nWhile the user had the browser they:");
    for entry in trail {
        text.push_str("\n  - ");
        text.push_str(&entry);
    }
    text
}
