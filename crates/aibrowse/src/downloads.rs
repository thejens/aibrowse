//! The `downloads` tool: what the session's pages downloaded, for downloads too large to
//! finish within the action that started them.

use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;

use crate::respond::text;
use crate::server::Aibrowse;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadsParams {
    /// Seconds to wait for downloads in progress to finish before listing (default 0,
    /// max 55).
    pub wait_seconds: Option<u64>,
}

#[tool_router(router = downloads_router, vis = "pub(crate)")]
impl Aibrowse {
    #[tool(
        description = "Files this session downloaded: name, size, where it is saved, and whether it finished. Actions already report the downloads they start; use this to wait for a large one."
    )]
    async fn downloads(
        &self,
        Parameters(p): Parameters<DownloadsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let downloads = live.browser.downloads().clone();
        // Waiting holds the session, so nothing else runs meanwhile; that is the point
        // of asking to wait.
        let wait = Duration::from_secs(p.wait_seconds.unwrap_or(0).min(55));
        downloads.wait_idle(wait).await;
        let list = downloads.list();
        if list.is_empty() {
            return Ok(text(format!(
                "Nothing was downloaded in this session. Downloads are saved in {}.",
                downloads.dir().display()
            )));
        }
        let lines: Vec<String> = list
            .iter()
            .enumerate()
            .map(|(i, d)| format!("{}. {}", i + 1, d.describe()))
            .collect();
        Ok(text(lines.join("\n")))
    }
}
