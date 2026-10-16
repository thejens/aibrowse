//! Tool parameter types. Field docs become the JSON Schema descriptions agents read.

use std::collections::BTreeMap;

use aibrowse_input::Pace;
use aibrowse_perception::Detail;
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionOpenParams {
    /// Named browser profile. Cookies, logins and storage persist per profile across
    /// sessions. Defaults to "default".
    pub profile: Option<String>,
    /// URL to open right away.
    pub url: Option<String>,
    /// Run without a visible window. Headless browsers are easy for sites to detect;
    /// only use this for sites that don't care.
    pub headless: Option<bool>,
    /// "human" (default) moves the cursor, scrolls and types like a person. "fast" skips
    /// the delays; only for sites without bot detection.
    pub pace: Option<PaceParam>,
    /// Whether a human is around to help: hand-offs for logins, approvals for
    /// consequential actions. Default true. Set false for unattended runs: then nothing
    /// is ever routed to a human, and every such decision comes back to you.
    pub attended: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SessionCloseParams {
    /// Close the browser but keep its place: the next call reopens the same tabs where
    /// they were. For long waits between steps. Default false (forget the tabs).
    pub hibernate: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PaceParam {
    Human,
    Fast,
}

impl From<PaceParam> for Pace {
    fn from(p: PaceParam) -> Self {
        match p {
            PaceParam::Human => Pace::Human,
            PaceParam::Fast => Pace::Fast,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NavigateParams {
    /// Absolute URL to load first. Optional when a goal is given (then the navigator
    /// starts from the current page).
    pub url: Option<String>,
    /// What to get to, in plain words, e.g. "the terms and conditions" or "my unread
    /// messages". A fast navigator clicks toward it and reports whose turn it is next.
    pub goal: Option<String>,
    /// With a goal: further parts to do after it, in order, each in plain words, e.g.
    /// goal "Enter the formula =A2+10 in cell C3", then ["Select cells C3 to C10",
    /// "Fill the formula down"]. Each part starts where the previous one left the page;
    /// a part that isn't done stops the rest.
    pub then: Option<Vec<String>>,
    /// Return the page outline after loading (default true).
    pub observe: Option<bool>,
    /// With a goal: texts the navigator may type where text is needed, e.g. search
    /// terms or a postcode ["refund digital game", "xbox refund"]. It types one only
    /// where it fits (and presses Enter); for anything else it hands back to you.
    /// Without texts it tries the goal's own words, so pass them when you know
    /// better ones.
    pub texts: Option<Vec<String>>,
    /// With a goal: keyboard shortcuts the navigator may press where they fit, each a
    /// chord as in act's press, then ": " and what it does, e.g. ["Meta+d: fill the
    /// selection down"] in a spreadsheet (Meta is Cmd on macOS; give the shortcut for
    /// this computer). Saying what it does lets the navigator tell when it's done. It
    /// presses one only where it fits, never one that would commit something (send,
    /// submit).
    pub keys: Option<Vec<String>>,
    /// With a goal: when it is reached, return a digest of the page (its own key
    /// passages for the goal, ~500 tokens; see read view=digest) instead of the outline.
    pub digest: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Character offset to continue from, as given at the end of the previous read.
    pub offset: Option<usize>,
    /// "full" (default): the main text, paginated. "digest": a short version made of
    /// the page's own passages (never rewritten), chosen for `focus`, plus the page
    /// type, sign-in state and forms; read again with view=full for everything.
    pub view: Option<ReadView>,
    /// For view=digest: what you care about on this page (e.g. "the price of the team
    /// plan"); passages about it are kept first.
    pub focus: Option<String>,
    /// For view=digest: roughly how many tokens it may take (default 500).
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReadView {
    Full,
    Digest,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EvaluateParams {
    /// A JavaScript expression. Promises are awaited; the result must be JSON-serializable
    /// (return plain data, not DOM nodes). Wrap statements in an IIFE: (() => { ... })().
    pub expression: String,
    /// "isolated" (default): shares the DOM but not the page's JavaScript globals, and
    /// the page cannot see it. "main": the page's own context, needed to read page
    /// globals or framework state; the page can observe what runs there.
    pub world: Option<WorldParam>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WorldParam {
    Isolated,
    Main,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TabsParams {
    /// Make tab N (numbered from 1, as listed) the active tab.
    pub switch: Option<usize>,
    /// Close tab N (numbered from 1, as listed).
    pub close: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ObserveParams {
    /// "interactive" (default): what is in view or near it, compact; the rest of the
    /// page is summarized ("… 40 more below"); scroll to bring it in.
    /// "all": everything actionable on the whole page, with link targets.
    /// "full": "all" plus readable text (paragraphs, list items, cells, images).
    pub detail: Option<DetailParam>,
    /// Only this part of the page, in full (nothing summarized away): a landmark role
    /// ("main", "navigation", "dialog", "form", "search"), its HTML name ("header",
    /// "footer", "aside", "nav"), a region as the outline's `regions:` line lists it
    /// (`navigation "Contents"`), or words from a landmark's name.
    pub region: Option<String>,
    /// Also return a JPEG screenshot of the viewport.
    pub screenshot: Option<bool>,
    /// With screenshot: outline each visible actionable element on it and label it with
    /// its ref (e.g. e123), to match what you see to refs.
    pub marks: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    /// Outline each visible actionable element and label it with its ref (e.g. e123),
    /// to match what you see to refs for act.
    pub marks: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DetailParam {
    Interactive,
    All,
    Full,
}

impl From<DetailParam> for Detail {
    fn from(d: DetailParam) -> Self {
        match d {
            DetailParam::Interactive => Detail::Interactive,
            DetailParam::All => Detail::All,
            DetailParam::Full => Detail::Full,
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Move the cursor to the element and click it. Needs `ref`.
    Click,
    /// Move the cursor over the element (opens hover menus, tooltips). Needs `ref`.
    Hover,
    /// Click a text field, replace its content with `text`. Needs `ref` and `text`.
    Fill,
    /// Type `text` into whatever has focus.
    Type,
    /// Press a key or chord in `key`, e.g. "Enter", "Escape", "Tab", "Shift+Tab", "Meta+a".
    Press,
    /// Scroll what is under the cursor by `dx`/`dy` pixels (positive dy scrolls down).
    Scroll,
    /// Choose an option of a dropdown (<select>) by its label or value in `text`. Needs `ref`.
    Select,
    /// Set a range slider (a price filter's minimum or maximum) to the number in `text`:
    /// by keyboard, or dragging its thumb when keys do nothing. Needs `ref`.
    Set,
    /// Accept the open confirm/prompt dialog (`text` answers a prompt).
    AcceptDialog,
    /// Dismiss (cancel) the open dialog.
    DismissDialog,
    /// Choose local `files` in a file input, or in the file chooser that the button or
    /// link at `ref` opens. Needs `ref` and `files`. Always needs approval: the user's,
    /// or confirm=true in unattended sessions.
    Upload,
    /// Call a tool the page itself offers to agents (WebMCP; the outline lists them,
    /// site_info shows their input schemas): `tool` names it, `args` is its input.
    /// Gated like the clicks it replaces; its output is written by the page.
    CallTool,
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Action::Click => "click",
            Action::Hover => "hover",
            Action::Fill => "fill",
            Action::Type => "type",
            Action::Press => "press",
            Action::Scroll => "scroll",
            Action::Select => "select",
            Action::Set => "set",
            Action::AcceptDialog => "accept_dialog",
            Action::DismissDialog => "dismiss_dialog",
            Action::Upload => "upload",
            Action::CallTool => "call_tool",
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ActParams {
    pub action: Action,
    /// Element reference from `observe`, e.g. "e123".
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Text for `fill`, `type` and `select`, or the answer for `accept_dialog` on a prompt.
    pub text: Option<String>,
    /// Key or chord for `press`.
    pub key: Option<String>,
    /// Horizontal scroll distance in pixels for `scroll`.
    pub dx: Option<f64>,
    /// Vertical scroll distance in pixels for `scroll`.
    pub dy: Option<f64>,
    /// Absolute paths of local files for `upload`. Hidden files and folders (like
    /// ~/.ssh) and key stores are refused.
    pub files: Option<Vec<String>>,
    /// Name of the page's tool for `call_tool`.
    pub tool: Option<String>,
    /// Input for `call_tool`, a JSON object matching the tool's input schema.
    pub args: Option<serde_json::Value>,
    /// Return a fresh `observe` (interactive detail) of the page after the action, saving
    /// a round trip.
    pub observe: Option<bool>,
    /// Unattended sessions only: perform an action that commits something on the
    /// user's behalf (send, post, accept, buy, delete, upload files) on your own
    /// authority. Only when your instructions explicitly allow it. Ignored when a user is present: they
    /// approve such actions themselves.
    pub confirm: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HandoffParams {
    /// What you ask of the user, shown at the top of their live view and in the
    /// notification: one clear request in plain words, saying what to do and what it
    /// is for, e.g. "Please log in to LinkedIn (and complete 2FA) so I can read your
    /// messages, then press Done" or "Please set the price filter to at most 5000 kr,
    /// then press Done".
    pub reason: String,
    /// Give up on the user after this many minutes (they may be away from the
    /// computer): handoff_status then says they didn't respond, and you carry on
    /// without it (skip that site, report what is missing). Leave it out when nothing
    /// can go on without them: the handoff then waits as long as it takes.
    pub give_up_after_minutes: Option<u64>,
    /// Open the live view in this computer's default browser and show a notification
    /// (default true). Set false when the user is elsewhere and you relay the link.
    pub open: Option<bool>,
    /// Finish the handoff automatically once the page URL contains this text, e.g.
    /// "linkedin.com/feed" after a login. Strongly recommended: users often forget Done.
    pub done_url: Option<String>,
    /// For a one-time code (2FA, SMS): the ref of the field it goes into, e.g. "e42".
    /// The user then gets a page with just a box for the code, easy on a phone, and the
    /// code is typed into that field and Enter pressed; you never see it.
    pub code_field: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HandoffStatusParams {
    /// Seconds to wait for the user to finish before returning "pending" (default 30,
    /// max 55). Call again to keep waiting.
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WatchParams {
    /// Open the live view in this computer's default browser (default true). Set false
    /// to just get the link, e.g. to pass to the user on another device.
    pub open: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NetworkLogParams {
    /// Show request #N (from the listing) in full: headers, bodies and timing, with
    /// credentials redacted.
    pub id: Option<u64>,
    /// Only requests whose method or URL contains this text.
    pub filter: Option<String>,
    /// Also list scripts, images, styles and fonts. By default only XHR/fetch calls and
    /// page loads are listed.
    pub all: Option<bool>,
    /// How many of the most recent requests to list (default 30).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApiListParams {
    /// Show endpoint `a3` in detail: its parameters, last request, and request and
    /// response shapes.
    pub endpoint: Option<String>,
    /// Only endpoints whose method, URL, documented summary or site contains this text,
    /// on any site visited. Without it, only the current site's endpoints are listed
    /// (the other sites are counted on one line).
    pub filter: Option<String>,
    /// Include third-party and non-JSON endpoints (analytics, assets, HTML fragments),
    /// on every site visited.
    pub all: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ApiCallParams {
    /// Endpoint id from api_list, e.g. "a3". The call repeats the last successful request
    /// the page made to it, with your path, query and body changes applied.
    pub endpoint: Option<String>,
    /// Instead of an endpoint: an absolute URL on a site the page talks to. It is sent
    /// with the headers of the page's latest successful API call to that origin.
    pub url: Option<String>,
    /// HTTP method for `url` (default GET, or POST when a body is given). An endpoint
    /// keeps its own method.
    pub method: Option<String>,
    /// Path parameters of the endpoint's template by name, e.g. {"invitations_id": "7123"}.
    pub path: Option<BTreeMap<String, String>>,
    /// Query parameters to set; null removes one. Others keep the last request's values.
    pub query: Option<BTreeMap<String, Option<String>>>,
    /// Request body: JSON (sent as JSON) or a string (sent as is). Default: the body of
    /// the last request to the endpoint.
    pub body: Option<serde_json::Value>,
    /// Return only part of a JSON response, by dot path: "data.items", "included.0";
    /// `*` for every item of a list and `{a,b.c}` for only some fields:
    /// "hits.*.{title,url,author.name}" (a field name on a list also means every item:
    /// "hits.title"). React Server Components responses are decoded into
    /// {"text", "value"} first; "$raw" returns their raw payload instead.
    pub pick: Option<String>,
    /// Unattended sessions only: send a state-changing request on your own authority.
    /// Only when your instructions explicitly allow it.
    pub confirm: Option<bool>,
}
