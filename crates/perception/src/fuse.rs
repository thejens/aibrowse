use std::collections::HashMap;

use aibrowse_browser::ElementRef;

use crate::RawObservation;
use crate::ax::AxNode;
use crate::compact::{apply_budget, count_actionable, focus, tidy};
use crate::control::{has_value, is_ad_frame, is_custom_clickable, is_iframe, live_role, states};
use crate::effects::effects;
use crate::forms;
use crate::frame::FrameCtx;
use crate::geometry::{place, position_containers};
use crate::model::{Detail, FormPart, Mark, Node, NodeKind, PageModel, PageType, Position};
use crate::page::Signals;
use crate::regions;
use crate::roles::{is_content, is_interactive, is_landmark, needs_name};
use crate::snapshot::{ElementFacts, FrameFacts};
use crate::text::{MAX_NAME_CHARS, MAX_TEXT_CHARS, clean, origin, shorten_href};
use crate::widgets::{SLIDER_HANDLE, field_value, name_unnamed_sliders, slider_handle};

/// Fuses the accessibility tree (semantics: roles, names, states) with the DOM snapshot
/// (geometry, styles, click listeners) into a Page Model, descending into iframes where
/// the page embeds them. With a `region` ("main", "footer", `navigation "Contents"`, a
/// landmark's name), the outline is that part of the page only, in full.
/// Chrome's built-in PDF viewer extension.
const PDF_VIEWER: &str = "chrome-extension://mhjfbmdgcfjbbpaeojofohoefgiehjai/";

pub fn fuse(raw: &RawObservation, detail: Detail, region: Option<&str>) -> PageModel {
    let scale = raw
        .snapshots
        .first()
        .map_or(1.0, |s| s.snapshot.scale(raw.viewport.content_width));
    let facts: HashMap<(u32, String), FrameFacts> = raw
        .snapshots
        .iter()
        .flat_map(|s| {
            s.snapshot
                .facts_by_frame(scale)
                .into_iter()
                .map(move |(frame, facts)| ((s.target, frame), facts))
        })
        .collect();
    let empty = FrameFacts::default();
    let frames: Vec<FrameCtx> = raw
        .frames
        .iter()
        .map(|capture| {
            let facts = facts
                .get(&(capture.target, capture.frame_id.clone()))
                .unwrap_or(&empty);
            FrameCtx::new(capture, facts)
        })
        .collect();

    let walk = |roots: &[(usize, &AxNode)]| {
        let mut walker = Walker {
            frames: &frames,
            raw,
            detail,
            origin: origin(&raw.url),
            nodes: Vec::new(),
        };
        for &(f, root) in roots {
            walker.visit(f, root, 0, false);
        }
        walker.nodes
    };
    let title = raw
        .snapshots
        .first()
        .map(|s| s.snapshot.title())
        .unwrap_or_default();
    let mut model = PageModel {
        detail,
        url: raw.url.clone(),
        title,
        viewport: raw.viewport,
        nodes: Vec::new(),
        omitted: 0,
        notice: None,
        page: None,
        auth: None,
        regions: Vec::new(),
        region: region.map(str::to_owned),
        marks: Vec::new(),
    };
    let Some(main_root) = frames.first().and_then(FrameCtx::root) else {
        return model;
    };
    let mut nodes = walk(&[(0, main_root)]);
    // While a modal dialog is open, the page behind it is inert to a person; listing it
    // would bury the dialog (often last in DOM order) under hundreds of unusable elements.
    let mut modals = modal_dialogs(&frames);
    // A modal nothing of which is in view isn't open to a person: reCAPTCHA's challenge
    // iframe, loaded below the page for a subscribe form (githubstatus.com), hid the
    // whole status page, and the navigator took it for the user's CAPTCHA.
    // By what is in it: a dialog's own box is often a zero-size wrapper placed at the
    // top, whatever its contents' place.
    let in_view = |modal: &(usize, &AxNode)| {
        let nodes = walk(std::slice::from_ref(modal));
        let inside: Vec<&Node> = nodes
            .iter()
            .filter(|n| !matches!(n.role.as_str(), "dialog" | "alertdialog"))
            .collect();
        inside.is_empty() || inside.iter().any(|n| n.position == Some(Position::Visible))
    };
    modals.retain(in_view);
    if !modals.is_empty() {
        let dialog_nodes = walk(&modals);
        let behind = count_actionable(&nodes) - count_actionable(&dialog_nodes);
        model.notice = Some(format!(
            "a modal dialog is open; the page behind it ({behind} actionable elements) is hidden until it is closed"
        ));
        nodes = dialog_nodes;
    }
    name_unnamed_sliders(&mut nodes);
    // Taken before tidying and focusing: the screenshot shows every element on screen,
    // including the repeated links and summarized parts the outline leaves out.
    model.marks = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Actionable && n.position == Some(Position::Visible))
        .filter_map(|n| {
            Some(Mark {
                reference: n.reference?,
                rect: n.rect?,
            })
        })
        .collect();
    let signals = signals(&frames);
    model.page = signals.page_type(&raw.url, &model.title, !modals.is_empty());
    // Chrome shows a PDF in a page of its own PDF extension (its stylesheet, and the
    // document in a closed shadow root; older versions an `<embed>`). What the outline
    // lists is the viewer's toolbar; the document is what `read` returns (pdf.rs).
    if frames.first().is_some_and(|f| {
        f.facts.elements.values().any(|e| {
            (e.tag.eq_ignore_ascii_case("embed") && e.attribute("type") == Some("application/pdf"))
                || (e.tag.eq_ignore_ascii_case("link")
                    && e.attribute("href")
                        .is_some_and(|h| h.starts_with(PDF_VIEWER)))
        })
    }) {
        model.page = Some(PageType {
            kind: "pdf",
            evidence: vec![
                "a PDF document in Chrome's viewer".into(),
                "read returns its text".into(),
            ],
        });
    }
    model.auth = signals.auth(model.page.as_ref());
    model.regions = regions::list(&nodes);
    // A region is read whole: the agent asked for exactly that part of the page, so
    // nothing in it is summarized away for being off-screen.
    let focused = match region {
        Some(query) => {
            nodes = regions::select(&nodes, query).unwrap_or_else(|| {
                model.notice = Some(format!(
                    "no region matches {query:?}; regions here: {}",
                    model.regions.join(", ")
                ));
                Vec::new()
            });
            false
        }
        None => detail == Detail::Interactive,
    };
    tidy(&mut nodes);
    if focused {
        focus(&mut nodes);
    }
    model.omitted = apply_budget(&mut nodes);
    position_containers(&mut nodes);
    model.nodes = nodes;
    model
}

/// Page-type signals from every visible node of every frame.
fn signals(frames: &[FrameCtx]) -> Signals {
    let mut signals = Signals::default();
    for frame in frames {
        for ax in &frame.capture.ax {
            let fact = frame.fact(ax);
            if ax.ignored || fact.is_some_and(ElementFacts::is_hidden) {
                continue;
            }
            // Paragraph text sits in links, emphasis and spans as often as directly in
            // the paragraph; a few levels up covers those.
            let in_paragraph = ax.role() == "StaticText"
                && std::iter::successors(ax.parent_id.as_deref(), |id| {
                    frame.by_id.get(id).and_then(|p| p.parent_id.as_deref())
                })
                .take(4)
                .any(|id| frame.by_id.get(id).is_some_and(|p| p.role() == "paragraph"));
            signals.in_modal = std::iter::successors(Some(ax), |n| {
                n.parent_id
                    .as_deref()
                    .and_then(|id| frame.by_id.get(id).copied())
            })
            .any(|n| {
                matches!(n.role(), "dialog" | "alertdialog")
                    && n.property("modal").and_then(|v| v.as_bool()) == Some(true)
            });
            signals.add(ax, fact, in_paragraph);
        }
    }
    signals
}

/// Open, visible modal dialogs in any frame.
fn modal_dialogs<'a>(frames: &'a [FrameCtx<'a>]) -> Vec<(usize, &'a AxNode)> {
    frames
        .iter()
        .enumerate()
        .flat_map(|(f, frame)| {
            frame.capture.ax.iter().filter_map(move |ax| {
                let is_modal = matches!(ax.role(), "dialog" | "alertdialog")
                    && !ax.ignored
                    && ax.property("modal").and_then(|v| v.as_bool()) == Some(true)
                    && frame.fact(ax).is_some_and(|fact| !fact.is_hidden());
                is_modal.then_some((f, ax))
            })
        })
        .collect()
}

struct Walker<'a> {
    frames: &'a [FrameCtx<'a>],
    raw: &'a RawObservation,
    detail: Detail,
    origin: String,
    nodes: Vec<Node>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Include {
    Actionable,
    Structure,
    /// A form without a `<form>` element: a structure node named "form".
    Form,
    /// A live region with text: its message, then only the controls inside it.
    Live(&'static str),
    Content,
    Skip,
}

impl Walker<'_> {
    /// `inside_actionable`: an ancestor is already actionable, so descendants are only
    /// shown when they are actionable themselves (e.g. a button inside a clickable card).
    fn visit(&mut self, f: usize, ax: &AxNode, depth: usize, inside_actionable: bool) {
        let frame = &self.frames[f];
        let fact = frame.fact(ax);
        // Hidden subtrees are invisible to users, and so to us.
        if fact.is_some_and(|f| f.bounds.is_some() && f.visibility == "hidden") {
            return;
        }

        let include = self.classify(f, ax, fact, inside_actionable);
        let mut child_depth = depth;
        let mut child_inside = inside_actionable;
        match include {
            Include::Skip => {}
            kind => {
                let node = self.build(f, ax, fact, depth, kind);
                // Below a control, or below a live region's message (which already holds
                // all its text), only controls are listed.
                if matches!(kind, Include::Actionable | Include::Live(_)) {
                    child_inside = true;
                }
                // Text-only leaves (StaticText) never have meaningful children.
                let leaf = matches!(kind, Include::Content) && ax.role() == "StaticText";
                self.nodes.push(node);
                if leaf {
                    return;
                }
                child_depth = depth + 1;
            }
        }

        let checkpoint = self.nodes.len();
        for child_id in &ax.child_ids {
            if let Some(child) = frame.by_id.get(child_id.as_str()) {
                self.visit(f, child, child_depth, child_inside);
            }
        }
        // An <iframe> element: continue into the embedded document, unless the frame is
        // invisible (preload frames at opacity 0) or an advertisement.
        if is_iframe(ax.role())
            && !fact.is_some_and(ElementFacts::is_hidden)
            && !is_ad_frame(ax.name())
            && let Some(owner) = ax.backend_dom_node_id
            && let Some(child) = self.frames.iter().position(|c| {
                c.capture.owner.as_ref().is_some_and(|(parent, backend)| {
                    *parent == frame.capture.frame_id && *backend == owner
                })
            })
            && let Some(root) = self.frames[child].root()
        {
            self.visit(child, root, child_depth, child_inside);
        }
        // A landmark with nothing inside it is noise.
        if matches!(include, Include::Structure | Include::Form) && self.nodes.len() == checkpoint {
            self.nodes.pop();
        }
    }

    fn classify(
        &self,
        f: usize,
        ax: &AxNode,
        fact: Option<&ElementFacts>,
        inside_actionable: bool,
    ) -> Include {
        let role = ax.role();
        let visible = fact.is_some_and(|f| !f.is_hidden());
        // Form containers are often plain divs, which Chrome may leave out of the tree
        // as uninteresting; they are checked before that.
        if !inside_actionable && self.frames[f].forms.contains(ax.node_id.as_str()) {
            return Include::Form;
        }
        if ax.ignored || role == "none" || role == "presentation" {
            return Include::Skip;
        }
        if is_interactive(role) {
            return if visible && ax.property("hidden") != Some(&serde_json::Value::Bool(true)) {
                Include::Actionable
            } else {
                Include::Skip
            };
        }
        // A native control whose ARIA role says otherwise is still the control: Cookie
        // Information's reject button is `<button role="alert">`, and taken for a live
        // message it couldn't be pressed, so its consent wall hid the whole page.
        if !inside_actionable && visible && native_role(fact).is_some() {
            return Include::Actionable;
        }
        let viewport_area = self.raw.viewport.width * self.raw.viewport.height;
        if !inside_actionable
            && visible
            && is_custom_clickable(
                ax,
                fact,
                viewport_area,
                self.frames[f].has_inside(ax, |n| is_interactive(n.role())),
            )
        {
            return Include::Actionable;
        }
        if inside_actionable {
            return Include::Skip;
        }
        if visible
            && let Some(live) = live_role(ax)
            && !self.frames[f].subtree_text(ax, 1).trim().is_empty()
        {
            return Include::Live(live);
        }
        // Unnamed forms are usually layout wrappers around a lone button ("Continue with
        // Google"); one with fields is a real form, and gets a signature (forms.rs).
        let named_enough = !needs_name(role)
            || !ax.name().is_empty()
            || self.frames[f].grid_title(ax).is_some()
            || (role == "form"
                && self.frames[f].has_inside(ax, |n| forms::field(n.role(), "", None).is_some()));
        if is_landmark(role) && named_enough {
            return Include::Structure;
        }
        if role == "heading" {
            return Include::Content;
        }
        if self.detail == Detail::Full && is_content(role) {
            let text_only_child = role == "StaticText";
            if text_only_child || !ax.name().trim().is_empty() {
                return Include::Content;
            }
        }
        Include::Skip
    }

    fn build(
        &self,
        f: usize,
        ax: &AxNode,
        fact: Option<&ElementFacts>,
        depth: usize,
        include: Include,
    ) -> Node {
        let frame = &self.frames[f];
        let actionable = include == Include::Actionable;
        let placed = fact.and_then(|e| e.bounds).map(|b| {
            place(
                b,
                frame.capture.rect,
                frame.facts.scroll,
                &self.raw.viewport,
            )
        });
        let mut role = match include {
            Include::Form => "form".to_owned(),
            Include::Live(live) => live.to_owned(),
            _ => ax.role().to_owned(),
        };
        let mut name = match include {
            Include::Live(_) => clean(
                &self.frames[f].subtree_text(ax, MAX_TEXT_CHARS * 2),
                MAX_TEXT_CHARS,
            ),
            _ => clean(
                ax.name(),
                if role == "StaticText" {
                    MAX_TEXT_CHARS
                } else {
                    MAX_NAME_CHARS
                },
            ),
        };
        if !is_interactive(&role) && actionable {
            role = native_role(fact).unwrap_or("clickable").into();
        }
        // A field's own text is what it holds, not what it is: an unnamed field is named
        // by the label beside it instead.
        if name.is_empty() && actionable && has_value(&role) {
            name = self.frames[f].nearby_label(ax).unwrap_or_default();
        } else if name.is_empty() && actionable {
            name = clean(
                &self.frames[f].subtree_text(ax, MAX_NAME_CHARS * 2),
                MAX_NAME_CHARS,
            );
        }
        if name.is_empty() && actionable {
            name = self.frames[f].hint_name(ax).unwrap_or_default();
        }
        if name.is_empty()
            && actionable
            && role == "clickable"
            && let Some(settable) = slider_handle(frame, ax, fact)
        {
            name = SLIDER_HANDLE.to_owned();
            if settable {
                role = "slider".into();
            }
        }
        if name.is_empty() && include == Include::Structure {
            name = frame.grid_title(ax).unwrap_or_default();
        }
        if role == "StaticText" {
            role = "text".into();
        }

        let is_password = fact.is_some_and(|f| f.attribute("type") == Some("password"));
        let value = field_value(&role, ax, fact).map(|v| {
            if is_password {
                // Never put a password, even a placeholder-looking one, into model context.
                if v.is_empty() {
                    String::new()
                } else {
                    "••••••".into()
                }
            } else {
                clean(&v, MAX_NAME_CHARS)
            }
        });

        let description = actionable.then(|| description(ax, fact, &name)).flatten();
        Node {
            kind: match include {
                Include::Actionable => NodeKind::Actionable,
                Include::Structure | Include::Form => NodeKind::Structure,
                Include::Live(_) | Include::Content | Include::Skip => NodeKind::Content,
            },
            depth,
            role,
            name,
            reference: if actionable {
                ax.backend_dom_node_id
                    .map(|backend| ElementRef::new(frame.capture.target, backend))
            } else {
                None
            },
            value,
            description,
            states: states(ax),
            effects: if actionable {
                effects(ax, fact)
            } else {
                Vec::new()
            },
            href: fact
                .filter(|_| ax.role() == "link")
                .and_then(|f| f.attribute("href"))
                .map(|h| shorten_href(h, &self.origin)),
            position: placed.as_ref().map(|p| p.position),
            distance: placed.as_ref().map_or(0.0, |p| p.distance),
            rect: placed.and_then(|p| p.rect),
            form_part: if actionable {
                self.form_part(f, ax, fact)
            } else {
                None
            },
        }
    }

    /// A field's type and, for dropdowns, its first options; or that it submits a form.
    fn form_part(&self, f: usize, ax: &AxNode, fact: Option<&ElementFacts>) -> Option<FormPart> {
        if forms::is_submit(ax.role(), fact) {
            return Some(FormPart::Submit);
        }
        let mut field = forms::field(ax.role(), ax.name(), fact)?;
        self.frames[f].complete_field(ax, fact, &mut field);
        Some(FormPart::Field(field))
    }
}

/// How much of an element's description is kept: a sentence.
const MAX_DESCRIPTION_CHARS: usize = 100;

/// An element's description, when its author gave one and it says more than its name.
/// A `title` attribute is a description too to Chrome, but on links it mostly repeats
/// the link's text (every Wikipedia link has one), so only the ARIA attributes count.
fn description(ax: &AxNode, fact: Option<&ElementFacts>, name: &str) -> Option<String> {
    let fact = fact?;
    if fact.attribute("aria-description").is_none() && fact.attribute("aria-describedby").is_none()
    {
        return None;
    }
    let description = clean(ax.description(), MAX_DESCRIPTION_CHARS);
    let repeats = name.to_lowercase().contains(&description.to_lowercase());
    (!description.is_empty() && !repeats).then_some(description)
}

/// The role a native control has whatever its ARIA role: a button, a link with an
/// address, a field or a list box. `None` for anything else.
fn native_role(fact: Option<&ElementFacts>) -> Option<&'static str> {
    let fact = fact?;
    match fact.tag.to_ascii_lowercase().as_str() {
        "button" => Some("button"),
        "a" if fact.attribute("href").is_some() => Some("link"),
        "select" => Some("combobox"),
        "textarea" => Some("textbox"),
        "input" => match fact
            .attribute("type")
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("hidden") => None,
            Some("checkbox") => Some("checkbox"),
            Some("radio") => Some("radio"),
            Some("range") => Some("slider"),
            Some("button" | "submit" | "reset" | "image") => Some("button"),
            _ => Some("textbox"),
        },
        _ => None,
    }
}
