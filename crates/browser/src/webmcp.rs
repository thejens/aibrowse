//! WebMCP: tools a page registers for agents, with `document.modelContext.registerTool()`
//! or declaratively on a `<form toolname=…>`.
//!
//! Chrome reports them over the CDP `WebMCP` domain (Chrome for Testing 154 has it
//! without flags). Enabling the domain is browser-side bookkeeping: nothing is injected
//! into the page and `Runtime` stays disabled, so pages can't tell we are listening.
//! Browsers without the domain simply report no tools.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aibrowse_cdp::{Session, SessionEvents};
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{BrowserError, Result};
use crate::tab::{ActionResult, Tab};
use crate::webmcp_polyfill::BRIDGED_PREFIX;

/// How long a page tool may take. Tools can do real work (search, checkout steps), but an
/// agent shouldn't hang on one that never answers.
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// A tool the page offers. Its name, description and schema are written by the site.
#[derive(Debug, Clone, Serialize)]
pub struct WebMcpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Option<Value>,
    pub frame_id: String,
    /// Declared on a form rather than registered from script.
    pub declarative: bool,
    /// The page's own hints: it changes nothing / it commits something.
    pub read_only: bool,
    pub consequential: bool,
    /// Its output may carry third-party or user-generated content.
    pub untrusted_content: bool,
    /// Registered through a `navigator.modelContext` polyfill and bridged into the
    /// native registry (webmcp_polyfill.rs).
    pub bridged: bool,
    /// The name it has in Chrome's registry, which invoking it takes: the bridged
    /// tools' carry the bridge's prefix, which `name` leaves out.
    #[serde(skip)]
    pub(crate) registered: String,
}

pub(crate) type SharedTools = Arc<Mutex<Vec<WebMcpTool>>>;

pub(crate) fn lock_tools(tools: &SharedTools) -> MutexGuard<'_, Vec<WebMcpTool>> {
    tools
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts tracking the page's tools. `events` must be subscribed before this is called,
/// so the burst Chrome sends on enable (every tool already registered) isn't missed.
pub(crate) async fn watch_webmcp(
    session: &Session,
    mut events: SessionEvents,
    main_frame_id: String,
) -> SharedTools {
    let tools = SharedTools::default();
    let shared = Arc::clone(&tools);
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let params = &event.params;
            let mut tools = lock_tools(&shared);
            match event.method.as_str() {
                "WebMCP.toolsAdded" => {
                    for tool in params["tools"].as_array().into_iter().flatten() {
                        let Some(tool) = parse(tool) else { continue };
                        tools.retain(|t| !(t.name == tool.name && t.frame_id == tool.frame_id));
                        tools.push(tool);
                    }
                }
                "WebMCP.toolsRemoved" => {
                    for gone in params["tools"].as_array().into_iter().flatten() {
                        tools.retain(|t| {
                            gone["name"] != t.name.as_str()
                                || gone["frameId"] != t.frame_id.as_str()
                        });
                    }
                }
                // A new document in the main frame takes every frame's tools with it.
                "Page.frameNavigated"
                    if params["frame"]["parentId"].is_null()
                        && params["frame"]["id"] == main_frame_id.as_str() =>
                {
                    tools.clear();
                }
                _ => {}
            }
        }
    });
    if let Err(err) = session.call("WebMCP.enable", json!({})).await {
        tracing::debug!(%err, "WebMCP domain unavailable; page tools won't be listed");
    }
    tools
}

fn parse(tool: &Value) -> Option<WebMcpTool> {
    let annotations = &tool["annotations"];
    let registered = tool["name"].as_str()?.to_owned();
    let bridged = registered.starts_with(BRIDGED_PREFIX);
    Some(WebMcpTool {
        name: registered
            .strip_prefix(BRIDGED_PREFIX)
            .unwrap_or(&registered)
            .to_owned(),
        bridged,
        registered,
        description: tool["description"].as_str().unwrap_or_default().to_owned(),
        input_schema: tool.get("inputSchema").cloned(),
        frame_id: tool["frameId"].as_str().unwrap_or_default().to_owned(),
        declarative: tool.get("backendNodeId").is_some(),
        read_only: annotations["readOnly"].as_bool().unwrap_or(false),
        consequential: annotations["consequential"].as_bool().unwrap_or(false),
        untrusted_content: annotations["untrustedContent"].as_bool().unwrap_or(false),
    })
}

impl Tab {
    /// The page tool called `name`. Tools are per frame; a name registered in several
    /// frames is ambiguous and must be resolved by the page, not guessed.
    pub fn webmcp_tool(&self, name: &str) -> Result<WebMcpTool> {
        let tools = self.webmcp_tools();
        let mut matching = tools.iter().filter(|t| t.name == name);
        match (matching.next(), matching.next()) {
            (Some(tool), None) => Ok(tool.clone()),
            (Some(_), Some(_)) => Err(BrowserError::Element(format!(
                "the page registers {name:?} in several frames; this isn't supported yet"
            ))),
            (None, _) => {
                let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                Err(BrowserError::Element(if names.is_empty() {
                    format!("the page offers no tool {name:?} (it registers no WebMCP tools)")
                } else {
                    format!(
                        "the page offers no tool {name:?}; it has: {}",
                        names.join(", ")
                    )
                }))
            }
        }
    }

    /// Runs the page's own tool `name` with `input`, as the page's agent interface
    /// intends, and returns what it answered in `ActionResult::output`.
    ///
    /// This calls the page's JavaScript with the user's session, so it is gated like the
    /// UI actions it replaces (by the caller). Chrome runs it; nothing is injected.
    pub async fn call_webmcp_tool(&mut self, name: &str, input: Value) -> Result<ActionResult> {
        let tool = self.webmcp_tool(name)?;
        // Subscribed before invoking: the response event may arrive right after the call.
        let mut events = self.session.events();
        let start = self.begin();
        let invoked = self
            .session
            .call(
                "WebMCP.invokeTool",
                json!({ "frameId": tool.frame_id, "toolName": tool.registered, "input": input }),
            )
            .await?;
        let id = invoked["invocationId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let response = tokio::time::timeout(TOOL_TIMEOUT, async {
            while let Some(event) = events.recv().await {
                if event.method == "WebMCP.toolResponded"
                    && event.params["invocationId"] == id.as_str()
                {
                    return Some(event.params);
                }
            }
            None
        })
        .await;
        let response = match response {
            Ok(Some(response)) => response,
            Ok(None) => return Err(BrowserError::Element("the page went away".into())),
            Err(_) => {
                let _ = self
                    .session
                    .call("WebMCP.cancelInvocation", json!({ "invocationId": id }))
                    .await;
                return Err(BrowserError::Element(format!(
                    "the page's tool {name:?} didn't answer within {}s; it was cancelled",
                    TOOL_TIMEOUT.as_secs()
                )));
            }
        };
        let mut notes = Vec::new();
        let status = response["status"].as_str().unwrap_or("Error");
        if status != "Completed" {
            let detail = response["errorText"]
                .as_str()
                .or_else(|| response["exception"]["description"].as_str())
                .unwrap_or("no details");
            notes.push(format!("the tool ended with status {status}: {detail}"));
        }
        let label = format!("call tool {name:?}");
        let mut result = self
            .finish(&label, start, Self::settle_timeout(), notes)
            .await?;
        result.output = response.get("output").cloned();
        Ok(result)
    }
}
