//! What the navigator may do on a page: the Page Model's actionable elements, labelled
//! for Jev, plus scrolling for content further down.

use aibrowse_browser::ElementRef;
use aibrowse_perception::{FormPart, Node, NodeKind, PageModel, Position};

/// Jev accepts at most 255 options per choice. Fewer keeps decisions sharp, but link-heavy
/// home pages put the footer (terms, privacy, careers) past the first hundred or so.
const MAX_CANDIDATES: usize = 200;

/// Option key for "scroll down to see more".
pub const SCROLL: &str = "scroll_down";
/// Option key for "go back: the last step led the wrong way" (pursuit.rs).
pub const GO_BACK: &str = "go_back";
/// Option key standing in when nothing on the page can be acted on (pursuit.rs).
pub const NOTHING: &str = "nothing";
/// Prefix of option keys for pressing the agent's offered keys: `press:0`, `press:1`.
/// Element keys are refs (`e12`, `f2e45`), which never hold a colon.
pub const PRESS: &str = "press:";
/// Separates a native `<select>`'s ref from an option's index in its options' keys:
/// `e12~3`.
const OPTION: char = '~';
/// A native `<select>` with more options than this (a country list) stays one field,
/// filled from the texts; with this many or fewer, each option is a choice of its own.
const MAX_SELECT_CHOICES: usize = 30;

pub struct Candidate {
    pub key: String,
    pub reference: ElementRef,
    pub label: String,
    /// The element by what it is, without where it is or what it holds: role, name and
    /// link target. The same element on a later visit has the same identity, so it is
    /// what loop detection and learned paths go by.
    pub identity: String,
    pub is_field: bool,
    /// A range slider: a field set to a number (a price filter's maximum), not typed.
    pub is_range: bool,
    /// A link to a web page: it can be opened in a background tab (fanout.rs).
    pub is_link: bool,
    /// Where a link leads, as the page wrote it (relative or absolute).
    pub href: Option<String>,
    /// In the viewport now.
    pub visible: bool,
    /// For an option of a native `<select>`: the option to choose in it. Such a
    /// dropdown opens a menu of the operating system's, which clicks on the page can't
    /// reach, so the option is chosen directly (`Tab::select`) as one step.
    pub option: Option<String>,
    /// For such an option: the identity of the option that would put the dropdown back
    /// to what it shows now, which choosing this one makes an undo.
    pub undo: Option<String>,
}

/// Actionable elements, visible ones first, as Jev options keyed by ref.
pub fn candidates(model: &PageModel) -> Vec<Candidate> {
    let mut out: Vec<(bool, f64, Candidate)> = model
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Actionable)
        // Links to other apps (mailto:, tel:, ms-settings:...) leave the browser: from
        // here they lead nowhere.
        .filter(|n| n.href.as_deref().is_none_or(stays_in_browser))
        // An expanded toggle's panel is open already: clicking it again only hides what
        // it showed (a FAQ answer the page was being judged by).
        .filter(|n| !(n.role == "button" && n.states.iter().any(|s| s == "expanded")))
        .flat_map(|n| {
            let found: Vec<(bool, Candidate)> = match select_options(n) {
                Some(options) => options,
                None => element(n).into_iter().collect(),
            };
            found
                .into_iter()
                .map(|(visible, c)| (visible, n.distance, c))
        })
        .collect();
    // Visible first; then the nearest off-screen ones, which the cap keeps: in page
    // order, Hemnet's filter panel (at the end of the page) lost its "Show N results"
    // button to hundreds of listing links further down. Stable: page order otherwise.
    out.sort_by(|a, b| (!a.0).cmp(&!b.0).then(a.1.total_cmp(&b.1)));
    // What a form's own button does, which its words may not say ("Visa 2 träffar",
    // "show 2 hits", applies the filters just set in Hemnet's panel; ranked 0.07 there).
    let submits = model.form_submits();
    out.into_iter()
        .map(|(_, _, mut c)| {
            if c.option.is_none() && submits.contains(&c.reference) {
                c.label.push_str(" (submits its form)");
            }
            c
        })
        .take(MAX_CANDIDATES)
        .collect()
}

/// The page's form fields with what they hold: values, and whether a checkbox or
/// radio button is checked. A sort order or a filter that was set shows here, where the
/// page's text may not say it.
pub fn field_values(model: &PageModel) -> Vec<String> {
    model
        .nodes
        .iter()
        .filter(|n| matches!(n.form_part, Some(FormPart::Field(_))))
        .map(|n| {
            let mut field = format!("{} \"{}\"", n.role, n.name);
            if let Some(value) = n.value.as_ref().filter(|v| !v.is_empty()) {
                field.push_str(&format!(
                    " (value: {})",
                    value.chars().take(30).collect::<String>()
                ));
            }
            for state in n.states.iter().filter(|s| *s == "checked") {
                field.push_str(&format!(" {state}"));
            }
            field
        })
        .collect()
}

/// What a page is set to: the fields holding a value or checked (dates, places, the
/// trip type, a filter), at most `MAX_SETTINGS`, never a password. Told with a reached
/// goal so the agent can see at a glance whether the page is what was asked ("one-way"
/// unchecked, a car route for "walking directions"), and go on from there if it isn't.
///
/// Each setting is told once: docs.python.org repeats its language, version and theme
/// pickers in a bar above and below the page. Those in the page's chrome (navigation,
/// header and footer landmarks) are site preferences rather than what the goal set, and
/// are told only when the page has no other settings.
pub fn page_settings(model: &PageModel) -> Vec<String> {
    let mut content = Vec::new();
    let mut chrome = Vec::new();
    // The depth of the chrome landmark the walk is inside, if any: nodes nest by depth.
    let mut chrome_depth: Option<usize> = None;
    for n in &model.nodes {
        if chrome_depth.is_some_and(|d| n.depth <= d) {
            chrome_depth = None;
        }
        if chrome_depth.is_none()
            && n.kind == NodeKind::Structure
            && matches!(n.role.as_str(), "navigation" | "banner" | "contentinfo")
        {
            chrome_depth = Some(n.depth);
            continue;
        }
        let Some(setting) = setting(n) else { continue };
        let found = if chrome_depth.is_some() {
            &mut chrome
        } else {
            &mut content
        };
        if !found.contains(&setting) {
            found.push(setting);
        }
    }
    let mut settings = if content.is_empty() { chrome } else { content };
    settings.truncate(MAX_SETTINGS);
    settings
}

/// A field's setting as `page_settings` tells it, when it holds one that says something:
/// not a password; not a field without a name (`spinbutton "": "469"` says nothing of
/// what is set); not one showing no choice of anyone's (a dropdown on its prompt, a
/// price bound at the control's own end) or paging through a list.
fn setting(n: &Node) -> Option<String> {
    match &n.form_part {
        Some(FormPart::Field(field))
            if field.kind != "password" && !field.unset && !field.paging => {}
        _ => return None,
    }
    if n.name.trim().is_empty() {
        return None;
    }
    // A radio or checkbox is set when checked; its value is only what it sends.
    let held = if matches!(n.role.as_str(), "radio" | "checkbox" | "switch") {
        n.states
            .iter()
            .any(|s| s == "checked")
            .then(|| "checked".to_owned())?
    } else {
        let value = n.value.as_deref().filter(|v| !v.trim().is_empty())?;
        format!("{:?}", value.chars().take(40).collect::<String>())
    };
    Some(format!("{} \"{}\": {held}", n.role, n.name))
}

const MAX_SETTINGS: usize = 10;

/// A native `<select>`'s options other than the one it shows, each a candidate of its
/// own: "Order by → Date last updated" is one decision, where clicking the dropdown
/// would only open a menu the navigator can't see. `None` for any other element, and
/// for a dropdown with too many options to offer one by one.
fn select_options(n: &Node) -> Option<Vec<(bool, Candidate)>> {
    let Some(FormPart::Field(field)) = &n.form_part else {
        return None;
    };
    if field.kind != "select" || field.options.is_empty() || field.option_count > MAX_SELECT_CHOICES
    {
        return None;
    }
    let reference = n.reference?;
    let visible = n.position.is_none_or(|p| p == Position::Visible);
    let current = n.value.as_deref().unwrap_or("");
    let options = field
        .options
        .iter()
        .enumerate()
        .filter(|(_, option)| option.as_str() != current)
        .map(|(i, option)| {
            let identity = format!("{} \"{}\" -> option \"{option}\"", n.role, n.name);
            let mut label = identity.clone();
            if !current.is_empty() {
                label.push_str(&format!(
                    " (from: {})",
                    current.chars().take(30).collect::<String>()
                ));
            }
            if !visible {
                label.push_str(" (off-screen)");
            }
            (
                visible,
                Candidate {
                    key: format!("{reference}{OPTION}{i}"),
                    reference,
                    label,
                    identity,
                    is_field: false,
                    is_range: false,
                    is_link: false,
                    visible,
                    href: None,
                    option: Some(option.trim_end_matches('…').to_owned()),
                    undo: (!current.is_empty())
                        .then(|| format!("{} \"{}\" -> option \"{current}\"", n.role, n.name)),
                },
            )
        })
        .collect();
    Some(options)
}

/// An actionable element as a candidate, if it can be acted on by ref.
fn element(n: &Node) -> Option<(bool, Candidate)> {
    let reference = n.reference?;
    let mut label = format!("{} \"{}\"", n.role, n.name);
    if let Some(href) = &n.href {
        label.push_str(&format!(
            " -> {}",
            href.chars().take(60).collect::<String>()
        ));
    }
    let identity = label.clone();
    if let Some(value) = n.value.as_ref().filter(|v| !v.is_empty()) {
        label.push_str(&format!(
            " (value: {})",
            value.chars().take(30).collect::<String>()
        ));
    }
    if let Some(description) = &n.description {
        label.push_str(&format!(" ({description})"));
    }
    // What it opens, which a name like "14 Oct 2026" doesn't say: OpenTable's date
    // picker read as a bare date among "Book table…" buttons, and wasn't chosen.
    if let Some(opens) = opens(n) {
        label.push_str(&format!(" ({opens})"));
    }
    let visible = n.position.is_none_or(|p| p == Position::Visible);
    if !visible {
        label.push_str(" (off-screen)");
    }
    // A read-only field takes no input: the text shown in it is the page's (a range
    // slider's "$75 - $300" readout, a date picker's opener). It is still clickable,
    // which is how such an opener is used, but never a field to fill.
    let editable = !n.states.iter().any(|s| s == "readonly");
    let is_range = editable && n.role == "slider";
    let is_field = is_range
        || (editable
            && matches!(
                n.role.as_str(),
                "textbox" | "searchbox" | "combobox" | "spinbutton"
            ));
    // Only a link with an address of its own leads somewhere a background tab
    // could open; one with an empty `href` is its script's (a filter option).
    let is_link = n.role == "link"
        && n.href.as_deref().is_some_and(|h| {
            !h.trim().is_empty()
                && !h.starts_with('#')
                && !h.to_ascii_lowercase().starts_with("javascript:")
        });
    Some((
        visible,
        Candidate {
            key: reference.to_string(),
            reference,
            label,
            identity,
            is_field,
            is_range,
            is_link,
            visible,
            option: None,
            undo: None,
            href: n.href.clone().filter(|_| is_link),
        },
    ))
}

/// What acting on the element opens, in words, when the outline knows.
fn opens(n: &Node) -> Option<&'static str> {
    let effect = n.effects.iter().find_map(|e| match *e {
        "opens-menu" => Some("opens a menu"),
        "opens-dialog" => Some("opens a dialog"),
        "opens-listbox" => Some("opens a list"),
        "opens-grid" => Some("opens a grid, as date pickers do"),
        "opens-tree" => Some("opens a tree"),
        "new-tab" => Some("opens in a new tab"),
        "download" => Some("downloads a file"),
        _ => None,
    });
    effect.or_else(|| {
        n.states
            .iter()
            .any(|s| s == "collapsed")
            .then_some("expands")
    })
}

/// Whether a link's target is a web page (absolute http(s), or relative) or script run
/// in the page (`javascript:`, ImmoScout24's "decline all" link), rather than another
/// app's scheme.
fn stays_in_browser(href: &str) -> bool {
    match href.split_once(':') {
        // A colon before any slash, query or fragment is a scheme.
        Some((scheme, _)) if !scheme.contains(['/', '?', '#']) => matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "javascript"
        ),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibrowse_browser::Viewport;
    use aibrowse_perception::Detail;

    #[test]
    fn links_to_other_apps_are_not_options() {
        assert!(stays_in_browser("https://x.com/a"));
        assert!(stays_in_browser("/en-us/windows/"));
        assert!(stays_in_browser("../experience/tools#bkmk"));
        assert!(stays_in_browser("x.com/path?next=a:b"));
        assert!(!stays_in_browser("ms-settings:defaultapps"));
        assert!(!stays_in_browser("mailto:help@x.com"));
        assert!(!stays_in_browser("tel:+46123"));
        assert!(stays_in_browser("javascript:UC_UI.denyAllConsents()"));
    }

    fn button(backend: i64, name: &str, states: &[&str]) -> Node {
        Node {
            kind: NodeKind::Actionable,
            depth: 0,
            role: "button".into(),
            name: name.into(),
            reference: Some(ElementRef::new(0, backend)),
            value: None,
            description: None,
            states: states.iter().map(|s| (*s).to_owned()).collect(),
            effects: Vec::new(),
            href: None,
            position: None,
            distance: 0.0,
            rect: None,
            form_part: None,
        }
    }

    fn page(nodes: Vec<Node>) -> PageModel {
        PageModel {
            detail: Detail::All,
            url: "https://x.test/faq".into(),
            title: "FAQ".into(),
            viewport: Viewport {
                width: 1440.0,
                height: 800.0,
                scroll_x: 0.0,
                scroll_y: 0.0,
                content_width: 1440.0,
                content_height: 800.0,
                device_pixel_ratio: 1.0,
            },
            nodes,
            omitted: 0,
            notice: None,
            page: None,
            auth: None,
            regions: Vec::new(),
            region: None,
            marks: Vec::new(),
        }
    }

    #[test]
    fn an_open_toggle_is_not_an_option() {
        let model = page(vec![
            button(1, "What do I do if my card is stolen?", &["expanded"]),
            button(2, "How do I replace a card?", &["collapsed"]),
        ]);
        let labels: Vec<String> = candidates(&model).into_iter().map(|c| c.label).collect();
        assert_eq!(labels, ["button \"How do I replace a card?\" (expands)"]);
    }

    fn select(backend: i64, name: &str, options: &[&str], count: usize) -> Node {
        Node {
            role: "combobox".into(),
            value: Some(options[0].to_owned()),
            form_part: Some(FormPart::Field(aibrowse_perception::Field {
                kind: "select",
                options: options.iter().map(|o| (*o).to_owned()).collect(),
                option_count: count,
                group: None,
                unset: false,
                paging: false,
            })),
            ..button(backend, name, &[])
        }
    }

    #[test]
    fn a_dropdowns_options_are_choices() {
        let sort = select(
            3,
            "Order by",
            &["Relevance", "Date last updated", "Name"],
            3,
        );
        let options = select_options(&sort).expect("a short native dropdown");
        let chosen: Vec<(&str, Option<&str>)> = options
            .iter()
            .map(|(_, c)| (c.key.as_str(), c.option.as_deref()))
            .collect();
        // The option it shows already is no step.
        assert_eq!(
            chosen,
            [("e3~1", Some("Date last updated")), ("e3~2", Some("Name"))]
        );
        assert!(options.iter().all(|(_, c)| !c.is_field));
        // A long list (countries) stays a field, filled from the texts.
        let countries = select(4, "Country", &["Afghanistan", "Albania"], 195);
        assert!(select_options(&countries).is_none());
    }

    #[test]
    fn a_read_only_field_is_clickable_but_not_filled() {
        let field = |backend: i64, name: &str, states: &[&str]| Node {
            role: "textbox".into(),
            ..button(backend, name, states)
        };
        let model = page(vec![
            field(1, "Price range", &["readonly"]),
            field(2, "Search", &[]),
        ]);
        let fields: Vec<(String, bool)> = candidates(&model)
            .into_iter()
            .map(|c| (c.label, c.is_field))
            .collect();
        assert_eq!(
            fields,
            [
                ("textbox \"Price range\"".to_owned(), false),
                ("textbox \"Search\"".to_owned(), true)
            ]
        );
    }

    #[test]
    fn the_nearest_off_screen_elements_come_first() {
        let at = |backend: i64, name: &str, distance: f64| Node {
            position: Some(if distance == 0.0 {
                Position::Visible
            } else {
                Position::Below
            }),
            distance,
            ..button(backend, name, &[])
        };
        let model = page(vec![
            at(1, "Shown", 0.0),
            at(2, "Far down the page", 9.0),
            at(3, "Just below", 0.4),
        ]);
        let names: Vec<String> = candidates(&model)
            .into_iter()
            .map(|c| c.label.split('"').nth(1).unwrap_or_default().to_owned())
            .collect();
        assert_eq!(names, ["Shown", "Just below", "Far down the page"]);
    }

    #[test]
    fn settings_are_told_once_and_chrome_only_without_others() {
        let at = |depth: usize, n: Node| Node { depth, ..n };
        let landmark = |depth: usize, role: &str| Node {
            kind: NodeKind::Structure,
            role: role.into(),
            reference: None,
            ..at(depth, button(0, "", &[]))
        };
        let picker = |depth: usize, backend: i64, name: &str| {
            at(depth, select(backend, name, &["English", "Deutsch"], 2))
        };
        let search = |depth: usize| Node {
            role: "textbox".into(),
            value: Some("asyncio.gather".into()),
            form_part: Some(FormPart::Field(aibrowse_perception::Field {
                kind: "text",
                options: Vec::new(),
                option_count: 0,
                group: None,
                unset: false,
                paging: false,
            })),
            ..at(depth, button(9, "Search", &[]))
        };
        // The same pickers in the bars above and below the page, as docs.python.org has.
        let chrome = || {
            vec![
                landmark(1, "navigation"),
                picker(2, 1, "Language"),
                picker(2, 2, "Theme"),
                landmark(1, "main"),
                landmark(1, "contentinfo"),
                picker(2, 3, "Language"),
                picker(2, 4, "Theme"),
            ]
        };
        assert_eq!(
            page_settings(&page(chrome())),
            [
                "combobox \"Language\": \"English\"",
                "combobox \"Theme\": \"English\""
            ]
        );
        // A setting of the page's own outranks them: the chrome's are left out.
        let mut nodes = chrome();
        nodes.insert(4, search(2));
        assert_eq!(
            page_settings(&page(nodes)),
            ["textbox \"Search\": \"asyncio.gather\""]
        );
    }

    #[test]
    fn fields_that_set_nothing_are_no_settings() {
        let field = |backend: i64, role: &str, name: &str, value: &str, kind: &'static str| Node {
            role: role.into(),
            value: Some(value.into()),
            form_part: Some(FormPart::Field(aibrowse_perception::Field {
                kind,
                options: Vec::new(),
                option_count: 0,
                group: None,
                unset: false,
                paging: false,
            })),
            ..button(backend, name, &[])
        };
        let flagged = |n: Node, unset: bool, paging: bool| {
            let mut n = n;
            if let Some(FormPart::Field(f)) = &mut n.form_part {
                f.unset = unset;
                f.paging = paging;
            }
            n
        };
        let model = page(vec![
            // Unnamed: what it sets is anyone's guess.
            field(1, "spinbutton", "", "469", "number"),
            // A dropdown still on its prompt option.
            flagged(
                field(2, "combobox", "Kommune", "Vælg kommune", "select"),
                true,
                false,
            ),
            // A price bound at the slider's own minimum.
            flagged(field(3, "slider", "Min price", "0", "slider"), true, false),
            // The page number of a pagination bar.
            flagged(field(4, "spinbutton", "Page", "2", "number"), false, true),
            field(5, "slider", "Max price", "1200", "slider"),
        ]);
        assert_eq!(page_settings(&model), ["slider \"Max price\": \"1200\""]);
    }
}
