//! Keeps a token scoped to some profiles away from the others (docs/design.md §5.8).
//!
//! Enforced per tool call, not per HTTP request: which profile a call touches depends on
//! the call (`session_open(profile)`) and on the session already open.

use axum::http::request::Parts;
use rmcp::RoleServer;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RequestContext;

use super::auth::Grant;
use crate::respond::failure;
use crate::server::Aibrowse;

/// The grant of the HTTP request behind a call; `None` over stdio, where whoever runs
/// the process is trusted with every profile.
fn grant(context: &RequestContext<RoleServer>) -> Option<&Grant> {
    context.extensions.get::<Parts>()?.extensions.get::<Grant>()
}

impl Aibrowse {
    /// Refuses the call when its grant doesn't cover the profile it would use: the one
    /// `session_open` asks for, else the open session's, else the default one a call
    /// starts implicitly.
    pub(crate) async fn out_of_scope(
        &self,
        request: &CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Option<CallToolResult> {
        let grant = grant(context)?;
        // Ending a session only ever gives access up.
        if request.name == "session_close" {
            return None;
        }
        let requested = (request.name == "session_open")
            .then(|| request.arguments.as_ref()?.get("profile")?.as_str())
            .flatten()
            .map(str::to_owned);
        let profile = match requested {
            Some(profile) => profile,
            None => match self.live.lock().await.as_ref() {
                Some(live) => live.browser.profile().name.clone(),
                None => self.defaults.profile.clone(),
            },
        };
        if grant.allows(&profile) {
            return None;
        }
        let allowed = grant.profiles().unwrap_or_default().join(", ");
        Some(failure(format!(
            "this token may not use profile {profile:?}; it is limited to: {allowed}. Call session_open with one of those (close the current session first if one is open)."
        )))
    }
}
