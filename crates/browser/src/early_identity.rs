//! Targets that start outside any session of ours must present the same identity as
//! the pages we drive, from their first script on.
//!
//! Pages (and the dedicated workers and frames under them) get the headless identity's
//! client hints from `UserAgentIdentity::apply` on their own sessions. Two kinds of
//! target start before such a session exists:
//!
//! - Service workers are browser-level targets, not under any page session. Without
//!   this they report the bare `--user-agent` identity (no platform version, no full
//!   version), which fingerprinters that probe from a service worker (CreepJS) see as a
//!   mismatch with the page, and as a difference from headful.
//! - Popups (target=_blank, window.open) begin loading before the agent follows them.
//!
//! Pages must be in the auto-attach filter regardless: while any browser-level
//! auto-attach is on, Chrome (154) holds a new popup's first navigation until an
//! auto-attached session resumes it, even with `waitForDebuggerOnStart` off. With pages
//! filtered out nobody does, and the popup sits at an empty URL forever while its
//! opener (same renderer) stops answering, so every action there times out.

use aibrowse_cdp::Connection;
use serde_json::json;

use crate::calibrate::UserAgentIdentity;
use crate::error::Result;

/// Attaches to every service worker and page as it starts, paused before its script
/// runs, applies `identity` there, and resumes it. The sessions stay attached: an
/// override lasts only as long as the session that set it.
pub(crate) async fn present_identity(conn: &Connection, identity: UserAgentIdentity) -> Result<()> {
    let browser = conn.browser();
    let mut events = browser.events();
    browser
        .call(
            "Target.setAutoAttach",
            json!({
                "autoAttach": true,
                "waitForDebuggerOnStart": true,
                "flatten": true,
                // Frames and dedicated workers come with their page's own session.
                "filter": [
                    { "type": "service_worker", "exclude": false },
                    { "type": "page", "exclude": false },
                    { "exclude": true },
                ],
            }),
        )
        .await?;
    let conn = conn.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if event.method != "Target.attachedToTarget" {
                continue;
            }
            let Some(session_id) = event.params["sessionId"].as_str() else {
                continue;
            };
            let target = conn.session(session_id);
            let kind = event.params["targetInfo"]["type"]
                .as_str()
                .unwrap_or_default();
            let waiting = event.params["waitingForDebugger"]
                .as_bool()
                .unwrap_or(false);
            let identity = identity.clone();
            let kind = kind.to_owned();
            tokio::spawn(async move {
                let applied = if kind == "service_worker" {
                    // Workers have no Emulation domain; Network carries the override.
                    target
                        .call(
                            "Network.setUserAgentOverride",
                            json!({
                                "userAgent": identity.user_agent,
                                "platform": identity.platform,
                                "userAgentMetadata": identity.metadata,
                            }),
                        )
                        .await
                        .map(drop)
                        .map_err(|e| e.to_string())
                } else {
                    // A paused page's renderer answers nothing until resumed, so
                    // awaiting the override first would wait out the CDP timeout. Both
                    // go out at once, resume last: a session's commands run in order,
                    // so the override is in place before the page's first script.
                    let resume = async {
                        if waiting {
                            let _ = target
                                .call("Runtime.runIfWaitingForDebugger", json!({}))
                                .await;
                        }
                    };
                    let (applied, ()) = tokio::join!(identity.apply(&target), resume);
                    if let Err(err) = applied {
                        tracing::debug!(%err, kind, "early identity override failed");
                    }
                    return;
                };
                if let Err(err) = applied {
                    tracing::debug!(%err, kind, "early identity override failed");
                }
                if waiting {
                    let _ = target
                        .call("Runtime.runIfWaitingForDebugger", json!({}))
                        .await;
                }
            });
        }
    });
    Ok(())
}
