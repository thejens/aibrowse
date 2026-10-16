//! `aibrowse reload`: a stable MCP front for development.
//!
//! An agent host starts its MCP servers once per session. While aibrowse is being built,
//! that would pin the agent to whatever binary was installed when the session began.
//! This mode is the process the host talks to; the real server runs as a child process
//! (`aibrowse mcp`), and when the installed binary changes (`make install`), the next
//! tool call hibernates the old server's session (its tabs go to the profile's journal,
//! Chrome saves the profile), starts the new server resuming that session, and tells
//! the host that the tool list may have changed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    GetPromptRequestParams, GetPromptResponse, JsonObject, ListPromptsResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
    ResultType, ServerCapabilities, ServerConfig, SubscriptionFilter,
};
use rmcp::service::{RequestContext, RunningService, SubscriptionContext, SubscriptionSink};
use rmcp::transport::TokioChildProcess;
use rmcp::{ErrorData as McpError, RoleClient, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::hibernate::HIBERNATED;

struct Worker {
    service: RunningService<RoleClient, ()>,
    /// Modification time of the binary it was started from.
    built: Option<SystemTime>,
}

#[derive(Clone)]
pub struct Reloader {
    exe: PathBuf,
    /// Live view address and watch token shared by every worker, so the user's watch
    /// page keeps working across reloads.
    live_view: (String, String),
    worker: Arc<Mutex<Option<Worker>>>,
    instructions: Option<String>,
    /// The profile of the last successful `session_open`: a reloaded worker resumes it
    /// rather than its default profile. Everything else (mode, pace, tabs) comes from the
    /// profile's session journal.
    profile: Arc<std::sync::Mutex<Option<String>>>,
    /// Open `subscriptions/listen` streams (protocol 2026-07-28 clients receive
    /// list-changed notifications only through these).
    listeners: Arc<std::sync::Mutex<Vec<SubscriptionSink>>>,
}

impl Reloader {
    pub async fn start() -> anyhow::Result<Self> {
        let exe = std::env::current_exe()?;
        let bind = match std::env::var("AIBROWSE_HANDOFF_BIND") {
            Ok(bind) => bind,
            Err(_) => {
                // Reserve a free local port once; workers bind it in turn.
                let probe = std::net::TcpListener::bind("127.0.0.1:0")?;
                probe.local_addr()?.to_string()
            }
        };
        let live_view = (bind, aibrowse_handoff::token());
        let worker = spawn(&exe, &live_view, None).await?;
        let instructions = worker
            .service
            .peer_info()
            .and_then(|info| info.instructions.clone());
        Ok(Self {
            exe,
            live_view,
            worker: Arc::new(Mutex::new(Some(worker))),
            instructions,
            profile: Arc::default(),
            listeners: Arc::default(),
        })
    }

    /// Tells every client that the tools may have changed after a reload.
    async fn announce_tool_change(&self, context: &RequestContext<RoleServer>) {
        let sinks: Vec<SubscriptionSink> = lock(&self.listeners).clone();
        let mut alive = Vec::with_capacity(sinks.len());
        for sink in sinks {
            if sink.notify_tool_list_changed().await.is_ok() {
                alive.push(sink);
            }
        }
        *lock(&self.listeners) = alive;
        // Clients on older protocol versions get the plain notification instead.
        let _ = context.peer.notify_tool_list_changed().await;
    }

    fn installed_build(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.exe).and_then(|m| m.modified()).ok()
    }
}

/// Starts a worker. With `resume`, it resumes the session the previous worker
/// hibernated, in that profile (`Some(None)`: its default profile).
async fn spawn(
    exe: &PathBuf,
    (bind, token): &(String, String),
    resume: Option<Option<String>>,
) -> anyhow::Result<Worker> {
    let built = std::fs::metadata(exe).and_then(|m| m.modified()).ok();
    let mut command = Command::new(exe);
    command
        .arg("mcp")
        .env("AIBROWSE_HANDOFF_BIND", bind)
        .env("AIBROWSE_WATCH_TOKEN", token);
    if let Some(profile) = resume {
        command.env("AIBROWSE_RESUME", "1");
        if let Some(profile) = profile {
            command.env("AIBROWSE_PROFILE", profile);
        }
    }
    let service = ().serve(TokioChildProcess::new(command)?).await?;
    Ok(Worker { service, built })
}

/// Ends a worker politely: its session is hibernated (tabs journaled, Chrome closed so
/// it flushes cookies and storage) before the worker is let go. Cancelling alone would
/// not do: the transport kills a worker that takes more than 3s to exit, and Chrome can
/// take longer than that to close. Returns whether there is a session to resume.
async fn retire(worker: Worker) -> bool {
    let mut hibernate = JsonObject::new();
    hibernate.insert("hibernate".into(), Value::Bool(true));
    let close = CallToolRequestParams::new("session_close").with_arguments(hibernate);
    let hibernated = worker
        .service
        .call_tool(close)
        .await
        .is_ok_and(|r| r.is_error != Some(true) && says_hibernated(&r));
    let _ = worker.service.cancel().await;
    hibernated
}

fn says_hibernated(result: &CallToolResult) -> bool {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .any(|t| t.text.starts_with(HIBERNATED))
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ServerHandler for Reloader {
    fn get_info(&self) -> ServerConfig {
        let config = ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .enable_prompts()
                .build(),
        );
        match &self.instructions {
            Some(instructions) => config.with_instructions(instructions.clone()),
            None => config,
        }
    }

    fn accepted_subscription_filter(
        &self,
        _requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(SubscriptionFilter::builder().tools_list_changed().build())
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        lock(&self.listeners).push(context.sink().clone());
        context.cancelled().await;
        Ok(())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let guard = self.worker.lock().await;
        let worker = guard
            .as_ref()
            .ok_or_else(|| McpError::internal_error("aibrowse worker is not running", None))?;
        let mut result = worker
            .service
            .list_tools(request)
            .await
            .map_err(|e| McpError::internal_error(format!("aibrowse worker: {e}"), None))?;
        // The worker speaks the legacy protocol to us and leaves out the 2026-07-28
        // result fields; this front speaks the client's version, so it restores them.
        // The tool list may change on reload, so it must not be cached.
        result.result_type = Some(ResultType::COMPLETE);
        result.ttl_ms = Some(0);
        result.cache_scope = Some(CacheScope::Private);
        Ok(result)
    }

    // Guides, the session and prompts are the worker's, as tools are: forwarded as they
    // are, so a client launching the front gets what the worker serves.
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let guard = self.worker.lock().await;
        let mut result = running(&guard)?
            .service
            .list_resources(request)
            .await
            .map_err(worker_error)?;
        result.result_type = Some(ResultType::COMPLETE);
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let guard = self.worker.lock().await;
        let mut result = running(&guard)?
            .service
            .read_resource(request)
            .await
            .map_err(worker_error)?;
        result.result_type = Some(ResultType::COMPLETE);
        Ok(ReadResourceResponse::Complete(result))
    }

    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let guard = self.worker.lock().await;
        let mut result = running(&guard)?
            .service
            .list_prompts(request)
            .await
            .map_err(worker_error)?;
        result.result_type = Some(ResultType::COMPLETE);
        Ok(result)
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        let guard = self.worker.lock().await;
        let mut result = running(&guard)?
            .service
            .get_prompt(request)
            .await
            .map_err(worker_error)?;
        result.result_type = Some(ResultType::COMPLETE);
        Ok(GetPromptResponse::Complete(result))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let mut guard = self.worker.lock().await;
        let mut notice = None;
        let current = self.installed_build();
        let stale = guard.as_ref().is_none_or(|w| w.built != current);
        if stale {
            // The new worker resumes the hibernated session by itself, from the journal,
            // when a call first needs the browser; it says so in that call's result.
            let mut resume = None;
            if let Some(old) = guard.take() {
                notice = Some("aibrowse was rebuilt and has reloaded.".to_owned());
                if retire(old).await {
                    resume = Some(lock(&self.profile).clone());
                }
            }
            match spawn(&self.exe, &self.live_view, resume).await {
                Ok(worker) => *guard = Some(worker),
                Err(err) => {
                    return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "aibrowse failed to start: {err}"
                    ))])
                    .into());
                }
            }
            self.announce_tool_change(&context).await;
        }
        let opened = (request.name == "session_open").then(|| {
            request
                .arguments
                .as_ref()
                .and_then(|args| args.get("profile"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let closing = request.name == "session_close";
        let worker = guard.as_ref().expect("worker ensured above");
        let mut result = match worker.service.call_tool(request).await {
            Ok(result) => result,
            Err(err) => {
                // The worker died (crash, or Chrome took it down). Drop it so the next
                // call starts a fresh one; the call itself is not retried because tool
                // calls are not idempotent.
                *guard = None;
                CallToolResult::error(vec![ContentBlock::text(format!(
                    "aibrowse worker failed ({err}); it will restart on the next call"
                ))])
            }
        };
        if result.is_error != Some(true) {
            let mut profile = lock(&self.profile);
            if let Some(opened) = opened {
                *profile = opened;
            } else if closing && !says_hibernated(&result) {
                // Closed for good; a hibernated session keeps its profile to resume in.
                *profile = None;
            }
        }
        if let Some(notice) = notice {
            result.content.insert(0, ContentBlock::text(notice));
        }
        result.result_type = Some(ResultType::COMPLETE);
        Ok(result.into())
    }
}

/// The running worker, or the error a request to it gets while there is none.
fn running(worker: &Option<Worker>) -> Result<&Worker, McpError> {
    worker
        .as_ref()
        .ok_or_else(|| McpError::internal_error("aibrowse worker is not running", None))
}

fn worker_error(err: impl std::fmt::Display) -> McpError {
    McpError::internal_error(format!("aibrowse worker: {err}"), None)
}
