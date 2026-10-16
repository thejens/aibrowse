//! Tools for the site's own API (docs/design.md §5.4): what the page called while the
//! agent browsed, and calling those endpoints directly from inside the page.

use aibrowse_browser::Tab;
use aibrowse_navigator::Jev;
use aibrowse_network::render::{self, LogFilter};
use aibrowse_network::{
    ApiRequest, ApiResponse, Overrides, ReplayHeader, fetch_script, plan, replay_for_endpoint,
    replay_headers, resolve_headers,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde_json::{Value, json};

use crate::confirm::{Gate, Parked, gate_api};
use crate::respond::{dialog_block, failure, text};
use crate::server::Aibrowse;
use crate::session::Live;
use crate::tools::{ApiCallParams, ApiListParams, NetworkLogParams};
use crate::trace::notes;

/// Response characters returned to the agent; `pick` narrows larger responses.
const MAX_RESPONSE_CHARS: usize = 20_000;

#[tool_router(router = network_router, vis = "pub(crate)")]
impl Aibrowse {
    #[tool(
        description = "Requests the active tab's page made (XHR/fetch calls and page loads), newest last, each with its API endpoint id and the action that caused it. id=N shows one request in full, credentials redacted."
    )]
    async fn network_log(
        &self,
        Parameters(p): Parameters<NetworkLogParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let log = live.tab().network();
        Ok(match p.id {
            Some(id) => match log.entry(id) {
                Some(entry) => text(render::entry(log, &entry)),
                None => failure(format!(
                    "no request #{id} in this tab's log (it keeps the most recent 400)"
                )),
            },
            None => text(render::log(
                log,
                &LogFilter {
                    text: p.filter,
                    all: p.all.unwrap_or(false),
                    limit: p.limit.unwrap_or(30),
                },
            )),
        })
    }

    #[tool(
        description = "The site's own API, learned from the calls its pages made while you browsed and from its OpenAPI/Swagger descriptions: each endpoint's method and URL template, the UI action that triggers it, its documented summary, and its response shape. Calling one with api_call is much faster than repeating the clicks."
    )]
    async fn api_list(
        &self,
        Parameters(p): Parameters<ApiListParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let catalog = live.browser.catalog().clone();
        catalog.ready().await;
        if p.endpoint.is_none() {
            crate::api_docs::add_documented(live, &catalog).await;
        }
        Ok(match &p.endpoint {
            Some(id) => match catalog.get(id) {
                Some(endpoint) => {
                    let replayed = match (&endpoint.sample, &endpoint.learned) {
                        (Some(s), _) => replay_headers(&s.headers, &s.cookies),
                        (None, Some(learned)) => learned.headers.clone(),
                        (None, None) => Vec::new(),
                    };
                    text(render::endpoint(&endpoint, &replayed))
                }
                None => failure(unknown_endpoint(id)),
            },
            None => {
                let site = url::Url::parse(&live.tab().url())
                    .ok()
                    .and_then(|u| u.host_str().map(aibrowse_network::site_of));
                text(render::endpoints(
                    &catalog.endpoints(),
                    p.filter.as_deref(),
                    p.all.unwrap_or(false),
                    site.as_deref(),
                ))
            }
        })
    }

    #[tool(
        description = "Call the site's own API from inside the page, as its own code does (the user's login applies): an endpoint from api_list, varying its path, query or body, or a URL. Returns the response, JSON narrowed with pick. Requests that change something need the user's approval, like the clicks they replace."
    )]
    async fn api_call(
        &self,
        Parameters(p): Parameters<ApiCallParams>,
    ) -> Result<CallToolResult, McpError> {
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
        if let Some(blocked) = dialog_block(live) {
            return Ok(failure(blocked));
        }
        let call = match build(live, &p).await {
            Ok(call) => call,
            Err(message) => return Ok(failure(message)),
        };
        if call.mutating {
            match gate_api(
                live,
                jev.as_ref(),
                &call.request,
                call.trigger.as_deref(),
                p.confirm,
            )
            .await
            {
                Gate::Proceed => {}
                Gate::Refuse(reason) => return Ok(failure(reason)),
                Gate::AgentDecides(message) => return Ok(text(message)),
                Gate::Ask { reason, summary } => {
                    let parked = Parked::Api {
                        request: call.request,
                        pick: p.pick,
                    };
                    return Ok(self.ask_approval(live, &reason, summary, parked).await);
                }
            }
        }
        Ok(execute(live, &call.request, p.pick.as_deref()).await)
    }
}

/// A request ready to run, and what the gate needs to know about it.
struct Call {
    request: ApiRequest,
    mutating: bool,
    /// The UI action the page makes this request for, if seen.
    trigger: Option<String>,
}

async fn build(live: &mut Live, p: &ApiCallParams) -> Result<Call, String> {
    let (method, url, body, replay, mutating, trigger) = match (&p.endpoint, &p.url) {
        (Some(id), _) => {
            let catalog = live.browser.catalog();
            catalog.ready().await;
            let endpoint = catalog.get(id).ok_or_else(|| unknown_endpoint(id))?;
            if p.method
                .as_ref()
                .is_some_and(|m| !m.eq_ignore_ascii_case(&endpoint.method))
            {
                return Err(format!(
                    "endpoint {id} is {}; to send another method, use url",
                    endpoint.method
                ));
            }
            let overrides = Overrides {
                path: p.path.clone().unwrap_or_default(),
                query: p.query.clone().unwrap_or_default(),
                body: p.body.clone(),
            };
            let (url, body) = plan(&endpoint, endpoint.sample.as_ref(), &overrides)?;
            let replay = replay_for_endpoint(&endpoint, live.tab().network(), &url);
            let trigger = endpoint.triggers.last().cloned();
            (
                endpoint.method,
                url,
                body,
                replay,
                endpoint.mutating,
                trigger,
            )
        }
        (None, Some(url)) => {
            if p.path.is_some() || p.query.is_some() {
                return Err(
                    "path and query apply to endpoints; with url, write them into the URL".into(),
                );
            }
            let method = match &p.method {
                Some(method) => method.to_ascii_uppercase(),
                None if p.body.is_some() => "POST".to_owned(),
                None => "GET".to_owned(),
            };
            let body = p.body.as_ref().map(|b| match b {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            });
            let mutating = !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS");
            let replay = live.tab().network().replay_for(url);
            (method, url.clone(), body, replay, mutating, None)
        }
        (None, None) => return Err("api_call needs an endpoint (from api_list) or a url".into()),
    };
    if body.is_some() && matches!(method.as_str(), "GET" | "HEAD") {
        return Err(format!("a {method} request can't have a body"));
    }
    let (replay, with_cookies) = replay;
    let jar = cookies(live, &url).await;
    let mut headers = resolve_headers(&replay, &jar);
    let sends_json = p.body.as_ref().is_some_and(|b| !b.is_string());
    if sends_json && !has_header(&replay, "content-type") {
        headers.push(("content-type".into(), "application/json".into()));
    }
    Ok(Call {
        request: ApiRequest {
            method,
            url,
            headers,
            body,
            with_cookies,
        },
        mutating,
        trigger,
    })
}

/// Runs `request` in the active page and renders the response for the agent. A request
/// to another origin that the browser refuses to send from the page (no CORS for it,
/// common for a documented API whose docs live elsewhere) is sent again from a
/// background tab on that origin.
pub async fn execute(live: &mut Live, request: &ApiRequest, pick: Option<&str>) -> CallToolResult {
    let mut response = match run(live.tab(), request).await {
        Ok(response) => response,
        Err(message) => {
            notes::api(request, Err(&message));
            return failure(message);
        }
    };
    let mut note = None;
    if response.error.is_some()
        && let Some(origin) = other_origin(&live.tab().url(), &request.url)
        && let Some(retried) = from_its_origin(live, &origin, request).await
        && retried.error.is_none()
    {
        response = retried;
        note = Some(format!(
            "The page's origin may not call {origin}, so the request was sent from a tab on {origin} (its cookies apply).\n"
        ));
    }
    notes::api(request, Ok(&response));
    text(format!(
        "{}{}",
        note.unwrap_or_default(),
        render::response(&response, pick, MAX_RESPONSE_CHARS)
    ))
}

/// Runs `request` as a fetch in `tab`'s page.
async fn run(tab: &mut Tab, request: &ApiRequest) -> Result<ApiResponse, String> {
    let value = tab
        .evaluate_isolated_raw(&fetch_script(request))
        .await
        .map_err(|err| format!("could not run the request in the page: {err}"))?;
    serde_json::from_value::<ApiResponse>(value)
        .map_err(|err| format!("unexpected result from the page: {err}"))
}

/// `request_url`'s origin, when it isn't `page_url`'s.
fn other_origin(page_url: &str, request_url: &str) -> Option<String> {
    let origin = |u: &str| {
        url::Url::parse(u)
            .ok()
            .map(|u| u.origin().ascii_serialization())
    };
    let target = origin(request_url)?;
    (origin(page_url).as_deref() != Some(target.as_str())).then_some(target)
}

/// Sends `request` from a background tab on `origin`, closed afterwards. The tab loads a
/// page of that origin that runs none of the site's scripts: its `/robots.txt`, or its
/// root when that fails to load.
async fn from_its_origin(
    live: &mut Live,
    origin: &str,
    request: &ApiRequest,
) -> Option<ApiResponse> {
    let mut tab = live.browser.new_background_tab("about:blank").await.ok()?;
    let mut response = None;
    for path in ["/robots.txt", "/"] {
        let _ = tab.navigate(&format!("{origin}{path}")).await;
        // A failed load ends on Chrome's own error page, which isn't the origin.
        if other_origin(&tab.url(), origin).is_none() {
            response = run(&mut tab, request)
                .await
                .inspect_err(|err| tracing::debug!(%origin, %err, "request from its origin failed"))
                .ok();
            break;
        }
    }
    let _ = live.browser.close_tab(tab.target_id()).await;
    response
}

/// The cookies the browser would send to `url`: current values for headers that copy a
/// cookie (CSRF tokens).
async fn cookies(live: &mut Live, url: &str) -> Vec<(String, String)> {
    let response = live
        .tab()
        .session()
        .call("Network.getCookies", json!({ "urls": [url] }))
        .await
        .unwrap_or_default();
    response["cookies"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let field = |k: &str| c[k].as_str().unwrap_or_default().to_owned();
            (field("name"), field("value"))
        })
        .collect()
}

fn has_header(headers: &[ReplayHeader], name: &str) -> bool {
    headers.iter().any(|h| h.name.eq_ignore_ascii_case(name))
}

fn unknown_endpoint(id: &str) -> String {
    format!("unknown endpoint {id:?}; api_list shows the endpoints known for the sites visited")
}
