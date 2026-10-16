use serde::{Deserialize, Serialize};

/// How much of the page to include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Detail {
    /// What is in view or near it: landmarks, headings and everything actionable, with
    /// repeats removed and the rest of the page summarized. The default, and what the
    /// navigator decides from.
    #[default]
    Interactive,
    /// Everything actionable on the whole page.
    All,
    /// Interactive plus readable content: paragraphs, list items, images, cells.
    Full,
}

/// Where a node is relative to the viewport. Agents need this to know whether something
/// is visible now or would require scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Visible,
    Above,
    Below,
    Left,
    Right,
}

/// Why a node is in the outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Something to act on; carries a ref.
    Actionable,
    /// A landmark or region that groups other nodes.
    Structure,
    /// Headings and, at full detail, text.
    Content,
    /// A line standing in for elements left out of a focused outline.
    Summary,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    /// Nesting depth among *included* nodes (skipped wrappers don't add depth).
    pub depth: usize,
    pub role: String,
    pub name: String,
    /// Present only on actionable nodes; rendered as `e45` or `f2e45` (inside iframe 2).
    /// Backed by the DOM node's backend id, which is stable for the node's lifetime, so
    /// refs stay valid across observations until the page re-creates the element.
    pub reference: Option<aibrowse_browser::ElementRef>,
    pub value: Option<String>,
    /// What the page's author says an element does, beyond its name
    /// (`aria-description`, `aria-describedby`): Wikipedia's "294 languages" button is
    /// "Go to an article in another language". Only on actionable elements, and only
    /// when it adds to the name.
    pub description: Option<String>,
    pub states: Vec<String>,
    /// What acting on it does beyond the obvious, when that changes what an agent would
    /// do: "opens-menu", "new-tab", "download" (see effects.rs).
    pub effects: Vec<&'static str>,
    /// Link target, shortened relative to the page origin.
    pub href: Option<String>,
    /// `None` when the node has no box of its own to measure (e.g. bare text).
    pub position: Option<Position>,
    /// How far outside the visible area, in screen heights (0 when visible).
    pub distance: f64,
    /// The visible part of the node's box in main-viewport CSS pixels ([x, y, width,
    /// height], the space screenshots are in); `None` when none of it is on screen.
    pub rect: Option<[f64; 4]>,
    /// The node's part in a form, when it is a field or a submit button.
    pub form_part: Option<FormPart>,
}

/// What a control contributes to a form (see forms.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormPart {
    Field(Field),
    /// A native submit button (`<button>` without another type, `<input type=submit>`).
    Submit,
}

/// A form field, typed by what it asks for rather than by its widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// "email", "password", "otp", "select", "checkbox", "text", ...
    pub kind: &'static str,
    /// A dropdown's options, as many as `MAX_KEPT_OPTIONS`: the navigator offers each
    /// of a native `<select>`'s as a choice of its own. Signatures list the first few.
    pub options: Vec<String>,
    /// How many options the dropdown has in all.
    pub option_count: usize,
    /// The radio group a radio button belongs to (its `name` attribute).
    pub group: Option<String>,
    /// It shows no choice of anyone's: a dropdown on its prompt option, a number or
    /// range at one of its own bounds.
    pub unset: bool,
    /// It pages through a list (a page number in a pagination bar).
    pub paging: bool,
}

#[derive(Debug, Clone)]
pub struct PageModel {
    pub detail: Detail,
    pub url: String,
    pub title: String,
    pub viewport: aibrowse_browser::Viewport,
    pub nodes: Vec<Node>,
    /// Actionable nodes omitted because of the budget.
    pub omitted: usize,
    /// Something the agent must know to read the outline right, e.g. that a modal dialog
    /// hides the rest of the page.
    pub notice: Option<String>,
    /// What kind of page this is, when the heuristics are confident (see page.rs).
    pub page: Option<PageType>,
    /// Whether the session looks signed in here, or whether the page needs the user.
    pub auth: Option<String>,
    /// The page's top-level landmarks, named the way `observe(region=…)` accepts them.
    pub regions: Vec<String>,
    /// The region the outline was narrowed to, if any.
    pub region: Option<String>,
    /// Every visible actionable element, including those the outline merges (repeated
    /// links) or summarizes, for set-of-marks screenshots.
    pub marks: Vec<Mark>,
}

/// A cheap, deterministic classification of the page, with the evidence for it so the
/// agent can judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageType {
    /// "login", "signup", "verification", "captcha", "cookie-consent", "error",
    /// "checkout", "search-results", "product", "article", "listing" or "pdf".
    pub kind: &'static str,
    pub evidence: Vec<String>,
}

impl PageType {
    /// Pages where only the account owner can go on: credentials, codes, challenges.
    pub fn needs_user(&self) -> bool {
        matches!(self.kind, "login" | "signup" | "verification" | "captcha")
    }
}

/// One element to label on a set-of-marks screenshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub reference: aibrowse_browser::ElementRef,
    /// Visible box in viewport CSS pixels: [x, y, width, height].
    pub rect: [f64; 4],
}

impl PageModel {
    /// The visible actionable elements and their on-screen boxes, for set-of-marks
    /// screenshots. This is more than the outline lists: a screenshot shows everything
    /// on screen, so a duplicate link or a summarized element the agent sees there must
    /// carry a label too. Every label is a ref `act` accepts.
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }
}
