use std::sync::Arc;

use aibrowse_browser::World;
use aibrowse_handoff::{Config as HandoffConfig, HandoffServer, Kind};
use aibrowse_navigator::{DEFAULT_DIGEST_TOKENS, Jev, Limits, Outcome, digest, pursue_parts};
use aibrowse_perception::Detail;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, GetPromptResult, ListPromptsResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use tokio::sync::{Mutex, MutexGuard, OnceCell};

use crate::confirm::{Gate, Parked, Pending, gate, perform};
use crate::guide;
use crate::hibernate::Lifecycle;
use crate::instructions::INSTRUCTIONS;
use crate::respond::{
    after_action, describe_parts, dialog_block, failure, notify_user, outline_changes, parse_ref,
    text,
};
use crate::session::{Defaults, Live};
use crate::tools::{
    ActParams, Action, EvaluateParams, NavigateParams, SessionCloseParams, SessionOpenParams,
    TabsParams, WorldParam,
};
use crate::trace::{Tracer, notes};

/// A session as session_open asked for it: profile, headless, attended.
pub(crate) type Asked = (String, Option<bool>, Option<bool>);

#[derive(Clone)]
pub struct Aibrowse {
    pub(crate) live: Arc<Mutex<Option<Live>>>,
    pub(crate) defaults: Defaults,
    /// The session the agent last asked for with session_open: (profile, headless,
    /// attended). A browser started by itself later starts as asked, never with the
    /// default profile instead: a session_open that failed (the profile was in use)
    /// must not turn into work in another profile, the user's own.
    pub(crate) asked: Arc<std::sync::Mutex<Option<Asked>>>,
    /// An action waiting for the user's approval.
    pub(crate) pending: Arc<Mutex<Option<Pending>>>,
    /// The navigator's decision model, created on first goal.
    pub(crate) jev: Arc<OnceCell<Jev>>,
    /// Started on first use: most sessions never need a human.
    pub(crate) handoff: Arc<OnceCell<HandoffServer>>,
    /// Records every tool call (docs/design.md §5.7).
    pub(crate) tracer: Tracer,
    /// Idle hibernation and the session journal (docs/design.md §5.1).
    pub(crate) lifecycle: Arc<Lifecycle>,
    #[allow(dead_code, reason = "read by the tool_handler macro")]
    tool_router: ToolRouter<Self>,
}

impl Aibrowse {
    pub fn new() -> Self {
        let defaults = Defaults::from_env();
        Self {
            live: Arc::new(Mutex::new(None)),
            lifecycle: Arc::new(Lifecycle::from_env(&defaults.profile)),
            defaults,
            asked: Arc::default(),
            handoff: Arc::new(OnceCell::new()),
            jev: Arc::new(OnceCell::new()),
            pending: Arc::new(Mutex::new(None)),
            tracer: Tracer::from_env(),
            tool_router: Self::tool_router()
                + Self::handoff_router()
                + Self::network_router()
                + Self::site_router()
                + Self::downloads_router()
                + Self::view_router()
                + Self::search_router()
                + Self::survey_router(),
        }
    }

    /// Points the watch view at the tab the agent now works in, and tells the live view
    /// whether this profile records the user's kinematics during handoffs.
    pub(crate) fn publish(&self, live: &mut Live) {
        if let Some(server) = self.handoff.get() {
            server.set_tab(Some(live.tab().session().clone()));
            server.set_recording(crate::persona::recording(&live.browser.profile().root));
        }
    }

    pub(crate) async fn handoff_server(&self) -> Result<&HandoffServer, String> {
        self.handoff
            .get_or_try_init(|| async {
                let server = HandoffServer::start(HandoffConfig::from_env()).await?;
                follow_privacy(server.user_has_browser(), Arc::clone(&self.live));
                Ok(server)
            })
            .await
            .map_err(|err: std::io::Error| format!("could not start the live view server: {err}"))
    }

    /// The action parked for approval in handoff `id`, if any.
    pub(crate) async fn take_pending(&self, id: &str) -> Option<Pending> {
        let mut pending = self.pending.lock().await;
        pending.take_if(|p| p.handoff == id)
    }

    /// Parks `parked` until the user approves it in the live view, and tells the agent.
    pub(crate) async fn ask_approval(
        &self,
        live: &mut Live,
        reason: &str,
        summary: String,
        parked: Parked,
    ) -> CallToolResult {
        let server = match self.handoff_server().await {
            Ok(server) => server,
            Err(err) => return failure(err),
        };
        self.publish(live);
        let ticket = server.open(live.tab().session().clone(), Kind::Approval, reason, None);
        notes::human("approval", "requested", Some(&ticket.id), reason);
        let watched = server.watchers() > 0;
        let reached = notify_user(&ticket.url, reason, !watched);
        let how = if reached {
            "opened on this computer, with a notification"
        } else if watched {
            "it shows up in the user's open live view"
        } else {
            "give the user this link"
        };
        let message = format!(
            "Not done yet: \"{summary}\" would commit something on the user's behalf, so it needs their approval. They were asked at {} ({how}). Call handoff_status: it runs when they approve.",
            ticket.url
        );
        *self.pending.lock().await = Some(Pending {
            handoff: ticket.id.clone(),
            parked,
            summary,
            page: live.tab().url(),
        });
        text(message)
    }

    /// Ends the session cleanly, so Chrome flushes cookies and storage to the profile.
    pub async fn shutdown(&self) {
        if let Some(server) = self.handoff.get() {
            server.cancel();
            server.set_tab(None);
            server.set_recording(None);
        }
        self.shutdown_session().await;
        self.tracer.flush().await;
    }

    /// The live session, started with the defaults if there is none yet.
    pub(crate) async fn session(&self) -> Result<MutexGuard<'_, Option<Live>>, CallToolResult> {
        // While the user has the browser, the agent keeps its hands off: no input that
        // could collide with theirs, and no observing what they type.
        if let Some(server) = self.handoff.get()
            && let Some(id) = server.pending()
        {
            let by_user = server.report(&id).is_some_and(|(by_user, _)| by_user);
            return Err(failure(if by_user {
                format!(
                    "the user took over the browser (handoff {id}). Call handoff_status to wait until they hand it back; you'll get what they did."
                )
            } else {
                format!(
                    "handoff {id} is in progress: the user has the browser. Call handoff_status to wait for them."
                )
            }));
        }
        let mut live = self.live.lock().await;
        if live
            .as_ref()
            .is_some_and(|l| !l.has_tabs() || l.browser.connection().is_closed())
        {
            // The page closed its last tab or Chrome went away; start over cleanly.
            *live = None;
        }
        if live.is_none() {
            let asked = self
                .asked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let (profile, headless, attended) =
                asked.unwrap_or_else(|| (self.defaults.profile.clone(), None, None));
            *live = Some(
                self.start_session(&profile, headless, attended)
                    .await
                    .map_err(|err| failure(format!("starting profile {profile:?}: {err}")))?,
            );
        }
        Ok(live)
    }
}

#[tool_router]
impl Aibrowse {
    #[tool(
        description = "Start the browser with a specific profile (logins persist per profile) or pace. Optional: the browser starts by itself on first use with the default profile."
    )]
    async fn session_open(
        &self,
        Parameters(p): Parameters<SessionOpenParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut live = self.live.lock().await;
        let profile = p.profile.unwrap_or_else(|| self.defaults.profile.clone());
        *self
            .asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((profile.clone(), p.headless, p.attended));
        if let Some(existing) = live.as_ref()
            && existing.browser.profile().name != profile
        {
            return Ok(failure(format!(
                "a session with profile {:?} is already open; call session_close first",
                existing.browser.profile().name
            )));
        }
        if live.is_none() {
            match self.start_session(&profile, p.headless, p.attended).await {
                Ok(started) => *live = Some(started),
                Err(err) => return Ok(failure(err)),
            }
        }
        let live = live.as_mut().expect("session ensured above");
        if let Some(pace) = p.pace {
            live.set_pace(pace.into());
        }
        if let Some(attended) = p.attended {
            live.set_attended(attended);
        }
        self.publish(live);
        let mut content = vec![ContentBlock::text(format!(
            "Session open with profile {profile:?}."
        ))];
        if let Some(url) = p.url {
            let result = live.tab().navigate(&url).await;
            content.extend(after_action(live, result, true).await);
        }
        Ok(CallToolResult::success(content))
    }

    #[tool(
        description = "Close the browser, saving the profile (cookies, logins) to disk. hibernate=true keeps its tabs to reopen on the next call."
    )]
    async fn session_close(
        &self,
        Parameters(p): Parameters<SessionCloseParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(server) = self.handoff.get() {
            server.cancel();
            server.set_tab(None);
        }
        if p.hibernate == Some(true) {
            return Ok(text(self.hibernate_now().await));
        }
        Ok(match self.close_session().await {
            Ok(true) => text("Session closed."),
            Ok(false) => text("No session was open."),
            Err(err) => failure(err),
        })
    }

    #[tool(
        description = "Load a URL and return the page outline; and/or pursue a goal (e.g. \"take me to the terms and conditions\", \"open my unread messages\"): a fast navigator clicks its way there and hands back when the goal is reached, or when text is needed (your turn) or a login/2FA/CAPTCHA (the user's turn, via request_handoff)."
    )]
    async fn navigate(
        &self,
        Parameters(p): Parameters<NavigateParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.url.is_none() && p.goal.is_none() {
            return Ok(failure("navigate needs a url, a goal, or both"));
        }
        let jev = match &p.goal {
            Some(_) => match self.jev.get_or_try_init(|| async { Jev::from_env() }).await {
                Ok(jev) => Some(jev.clone()),
                Err(err) => return Ok(failure(err)),
            },
            None => None,
        };
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let mut content = Vec::new();
        if let Some(url) = &p.url {
            let result = live.tab().navigate(url).await;
            let loaded = result.is_ok();
            let outline = p.goal.is_none() && p.observe.unwrap_or(true);
            content.extend(after_action(live, result, outline).await);
            if !loaded {
                return Ok(CallToolResult::success(content));
            }
        }
        self.publish(live);
        if let (Some(goal), Some(jev)) = (&p.goal, jev) {
            let texts = p.texts.clone().unwrap_or_default();
            let goals: Vec<String> = std::iter::once(goal.clone())
                .chain(
                    p.then
                        .iter()
                        .flatten()
                        .filter(|part| !part.trim().is_empty())
                        .cloned(),
                )
                .collect();
            let limits = Limits {
                keys: p.keys.clone().unwrap_or_default(),
                agreements: live.agreements(),
                ..Limits::default()
            };
            let parts = pursue_parts(live, &jev, &goals, &texts, &limits).await;
            let mut tab_notes: Vec<String> = parts
                .as_ref()
                .map(|parts| {
                    parts
                        .iter()
                        .flat_map(|(_, report)| report.tab_notes.clone())
                        .collect()
                })
                .unwrap_or_default();
            tab_notes.extend(live.sync_tabs().await);
            self.publish(live);
            if let Ok(parts) = &parts {
                for (part, report) in parts {
                    notes::navigator(part, report);
                }
                live.doing.goal(
                    goals.clone(),
                    parts
                        .iter()
                        .map(|(_, report)| notes::outcome_name(&report.outcome))
                        .collect(),
                );
            }
            let reached = parts.as_ref().is_ok_and(|parts| {
                parts.len() == goals.len()
                    && parts
                        .last()
                        .is_some_and(|(_, r)| matches!(r.outcome, Outcome::Reached))
            });
            match parts {
                Ok(parts) => content.push(ContentBlock::text(describe_parts(
                    &goals,
                    &parts,
                    &tab_notes,
                    live.attended(),
                ))),
                Err(err) => content.push(ContentBlock::text(format!(
                    "Error: the navigator could not decide: {err}"
                ))),
            }
            // The digest is about where the goal ends: its last part.
            let goal = goals.last().unwrap_or(goal);
            if p.digest.unwrap_or(false) && reached && live.has_tabs() {
                // What the page says, for the goal, instead of what can be clicked.
                match digest(live.tab(), Some(&jev), Some(goal), DEFAULT_DIGEST_TOKENS).await {
                    Ok(digest) => content.push(ContentBlock::text(digest)),
                    Err(err) => content.push(ContentBlock::text(format!(
                        "Error: no digest of the page: {err}"
                    ))),
                }
            } else if p.observe.unwrap_or(true) && live.has_tabs() {
                // The navigator's steps may have left the page much as it was: send what
                // changed, like act does.
                content.push(ContentBlock::text(
                    outline_changes(live, Detail::Interactive).await,
                ));
            }
        }
        Ok(CallToolResult::success(content))
    }

    #[tool(description = "Go back one page in the active tab's history.")]
    async fn back(&self) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let result = live.tab().back().await;
        Ok(CallToolResult::success(
            after_action(live, result, true).await,
        ))
    }

    #[tool(
        description = "Interact with the active page: click, hover, fill, type, press, scroll, select, accept_dialog, dismiss_dialog, upload, call_tool (a tool the page offers agents). Element actions take a ref from observe."
    )]
    async fn act(&self, Parameters(p): Parameters<ActParams>) -> Result<CallToolResult, McpError> {
        let jev = self
            .jev
            .get_or_try_init(|| async { Jev::from_env() })
            .await
            .ok()
            .cloned();
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let reference = match p.reference.as_deref().map(parse_ref).transpose() {
            Ok(reference) => reference,
            Err(message) => return Ok(failure(message)),
        };
        let is_dialog_action = matches!(p.action, Action::AcceptDialog | Action::DismissDialog);
        if !is_dialog_action && let Some(blocked) = dialog_block(live) {
            return Ok(failure(blocked));
        }
        match gate(live, jev.as_ref(), &p, reference).await {
            Gate::Proceed => {}
            Gate::Refuse(reason) => return Ok(failure(reason)),
            Gate::AgentDecides(message) => return Ok(text(message)),
            Gate::Ask { reason, summary } => {
                let parked = Parked::Act(p.clone());
                return Ok(self.ask_approval(live, &reason, summary, parked).await);
            }
        }
        let result = match perform(live, &p, reference).await {
            Ok(result) => result,
            Err(invalid) => return Ok(invalid),
        };
        let content = after_action(live, result, p.observe.unwrap_or(false)).await;
        self.publish(live);
        Ok(CallToolResult::success(content))
    }

    #[tool(
        description = "Run JavaScript in the active page and return the JSON result. Runs in an isolated world by default (shares the DOM, invisible to the page); world=main for page globals."
    )]
    async fn evaluate(
        &self,
        Parameters(p): Parameters<EvaluateParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        if let Some(blocked) = dialog_block(live) {
            return Ok(failure(blocked));
        }
        let world = match p.world {
            Some(WorldParam::Main) => World::Main,
            _ => World::Isolated,
        };
        Ok(match live.tab().evaluate(&p.expression, world).await {
            Ok(value) => text(match value {
                serde_json::Value::String(s) => s,
                serde_json::Value::Null => {
                    "null (no value; return plain data, not DOM nodes)".into()
                }
                other => serde_json::to_string_pretty(&other).unwrap_or_default(),
            }),
            Err(err) => failure(err),
        })
    }

    #[tool(description = "List open tabs, or switch to / close one (tabs are numbered from 1).")]
    async fn tabs(
        &self,
        Parameters(p): Parameters<TabsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let outcome = match (p.switch, p.close) {
            (Some(n), _) => live.switch(n.saturating_sub(1)).await,
            (None, Some(n)) => live.close(n.saturating_sub(1)).await,
            (None, None) => Ok(()),
        };
        if let Err(message) = outcome {
            return Ok(failure(message));
        }
        self.publish(live);
        let listing: Vec<String> = live
            .tab_list()
            .await
            .into_iter()
            .enumerate()
            .map(|(i, (active, info))| {
                let marker = if active { " (active)" } else { "" };
                format!("{}. {} — {}{marker}", i + 1, info.title, info.url)
            })
            .collect();
        Ok(text(listing.join("\n")))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Aibrowse {
    /// Routes the call to its tool, tracing it on the way (the one hook for traces).
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if let Some(refused) = self.out_of_scope(&request, &context).await {
            return Ok(CallToolResponse::Complete(refused));
        }
        self.call_started();
        let name = request.name.clone();
        let call = self.trace_begin(&request).await;
        let tool = ToolCallContext::new(self, request, context);
        let (mut response, notes) = notes::collect(self.tool_router.call(tool)).await;
        if let Some(notice) = self.call_finished(&name)
            && let Ok(CallToolResponse::Complete(result)) = &mut response
        {
            result.content.insert(0, ContentBlock::text(notice));
        }
        self.trace_end(call, &response, notes);
        response
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(guide::resources()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri;
        let text = if uri == guide::SESSION_URI {
            self.session_summary().await
        } else {
            guide::guide(&uri)
                .ok_or_else(|| McpError::resource_not_found(format!("no resource {uri:?}"), None))?
                .to_owned()
        };
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::text(text, uri).with_mime_type("text/markdown")],
        )))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(guide::prompts()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        let args: std::collections::HashMap<String, String> = request
            .arguments
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
            .collect();
        let messages = guide::prompt(&request.name, &args)
            .map_err(|err| McpError::invalid_params(err, None))?;
        Ok(GetPromptResponse::Complete(GetPromptResult::new(messages)))
    }
}

impl Aibrowse {
    /// The `aibrowse://session` resource: the profile, its settings and the open tabs,
    /// or which profile the browser would start with.
    async fn session_summary(&self) -> String {
        let live = self.live.lock().await;
        let Some(live) = live.as_ref() else {
            return format!(
                "# Session\n\nNo browser is running. It starts on the first tool call, with the profile {:?} (or the one session_open names).",
                self.defaults.profile
            );
        };
        let mut text = format!(
            "# Session\n\nProfile: {}\nAttended: {}\nApproval for committing actions: {}\nAgreements (terms, cookie prompts): {}\n\n## Tabs\n",
            live.browser.profile().name,
            if live.attended() {
                "yes, a user may be present"
            } else {
                "no, unattended"
            },
            if live.confirm_consequential() {
                "on"
            } else {
                "off"
            },
            match live.agreements() {
                aibrowse_navigator::Agreements::Accept => "accepted to go on",
                aibrowse_navigator::Agreements::Ask => "asked about",
            },
        );
        for (n, (active, tab)) in live.tab_list().await.into_iter().enumerate() {
            text.push_str(&format!(
                "{}. {}{} — {}\n",
                n + 1,
                tab.title,
                if active { " (active)" } else { "" },
                tab.url
            ));
        }
        text
    }
}

/// Privacy mode: while the user has the browser, the tabs' network logs keep no bodies
/// (what the user sends, such as passwords and codes, is theirs), and resume afterwards.
fn follow_privacy(
    mut user_has_browser: tokio::sync::watch::Receiver<bool>,
    live: Arc<Mutex<Option<Live>>>,
) {
    tokio::spawn(async move {
        while user_has_browser.changed().await.is_ok() {
            let on = *user_has_browser.borrow_and_update();
            if let Some(live) = live.lock().await.as_mut() {
                live.set_private(on);
            }
        }
    });
}
