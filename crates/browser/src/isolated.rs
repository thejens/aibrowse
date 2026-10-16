use aibrowse_cdp::{CdpError, Session};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Result;

/// Name of our isolated world. Page scripts cannot see isolated worlds or anything
/// defined in them; the name only shows up in DevTools.
const WORLD_NAME: &str = "aibrowse";

/// Executes helper JavaScript in an isolated world of the main frame.
///
/// Isolated worlds share the DOM with the page but not its JavaScript globals, so our
/// helpers can neither be observed nor tampered with by page scripts (a page that
/// overrides `Node.prototype.contains` doesn't affect us). We never enable the `Runtime`
/// domain: `Runtime.evaluate`/`callFunctionOn` work without it, and enabling it is a
/// known automation tell.
pub(crate) struct IsolatedWorld {
    context_id: i64,
    navigation: u64,
}

impl IsolatedWorld {
    pub async fn create(session: &Session, frame_id: &str, navigation: u64) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Created {
            execution_context_id: i64,
        }
        let created: Created = session
            .call_as(
                "Page.createIsolatedWorld",
                json!({ "frameId": frame_id, "worldName": WORLD_NAME }),
            )
            .await?;
        Ok(Self {
            context_id: created.execution_context_id,
            navigation,
        })
    }

    /// The world dies with its document; callers recreate it after a navigation.
    pub fn is_current(&self, navigation: u64) -> bool {
        self.navigation == navigation
    }

    pub async fn evaluate(&self, session: &Session, expression: &str) -> Result<Value> {
        let response = session
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "contextId": self.context_id,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        result_value(response, "Runtime.evaluate")
    }

    /// Resolves a DOM node into this world and returns its remote object id.
    /// The backend node id of the element `expression` evaluates to in this world, if any.
    pub async fn element(&self, session: &Session, expression: &str) -> Result<Option<i64>> {
        let result = session
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "contextId": self.context_id }),
            )
            .await?;
        let Some(object_id) = result["result"]["objectId"].as_str() else {
            return Ok(None);
        };
        let described = session
            .call("DOM.describeNode", json!({ "objectId": object_id }))
            .await?;
        Ok(described["node"]["backendNodeId"].as_i64())
    }

    pub async fn resolve(&self, session: &Session, backend_node_id: i64) -> Result<String> {
        let response = session
            .call(
                "DOM.resolveNode",
                json!({ "backendNodeId": backend_node_id, "executionContextId": self.context_id }),
            )
            .await?;
        Ok(response["object"]["objectId"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    /// Calls `function` with `this` bound to the given node and the other nodes as args.
    pub async fn call_on(
        &self,
        session: &Session,
        backend_node_id: i64,
        function: &str,
        args: &[i64],
    ) -> Result<Value> {
        let object_id = self.resolve(session, backend_node_id).await?;
        let mut arguments = Vec::with_capacity(args.len());
        for arg in args {
            arguments.push(json!({ "objectId": self.resolve(session, *arg).await? }));
        }
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": function,
                    "objectId": object_id,
                    "arguments": arguments,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        result_value(response, "Runtime.callFunctionOn")
    }

    /// Calls `function` on a node with plain JSON arguments.
    pub async fn call_on_values(
        &self,
        session: &Session,
        backend_node_id: i64,
        function: &str,
        args: &[Value],
    ) -> Result<Value> {
        let object_id = self.resolve(session, backend_node_id).await?;
        let arguments: Vec<Value> = args.iter().map(|v| json!({ "value": v })).collect();
        let response = session
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": function,
                    "objectId": object_id,
                    "arguments": arguments,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        result_value(response, "Runtime.callFunctionOn")
    }
}

fn result_value(response: Value, method: &str) -> Result<Value> {
    if let Some(details) = response.get("exceptionDetails") {
        let message = details["exception"]["description"]
            .as_str()
            .or_else(|| details["text"].as_str())
            .unwrap_or("script threw")
            .to_owned();
        return Err(CdpError::Protocol {
            method: method.to_owned(),
            code: -1,
            message,
        }
        .into());
    }
    Ok(response["result"]["value"].clone())
}

/// Counts DOM mutations since the observer was installed. Used to detect when the page
/// has stopped changing after an action.
///
/// The same observer hears live regions (`role=alert|status`, `aria-live`), the way a
/// screen reader does: when one's text changes to something non-empty, the text is
/// queued for `TAKE_ANNOUNCEMENTS`. It is read inside the callback because toasts are
/// transient ("Saved" for three seconds): by the time the action has settled and the
/// page is observed, the message may be gone. Each region's last text is remembered, so
/// a re-render that writes the same message again is not news.
pub(crate) const MUTATION_COUNTER: &str = r#"(() => {
  if (!globalThis.__aibrowseMutations) {
    const LIVE = '[role=alert],[role=status],[aria-live=polite],[aria-live=assertive]';
    const state = { count: 0, said: [], last: new WeakMap() };
    const hear = (region) => {
      const text = (region.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 200);
      if (!text || state.last.get(region) === text) return;
      state.last.set(region, text);
      state.said.push(text);
      if (state.said.length > 10) state.said.shift();
    };
    new MutationObserver((records) => {
      state.count += records.length;
      const regions = new Set();
      for (const r of records) {
        const at = r.target.nodeType === 1 ? r.target : r.target.parentElement;
        const region = at && at.closest(LIVE);
        if (region) regions.add(region);
        for (const added of r.addedNodes) {
          if (added.nodeType !== 1) continue;
          if (added.matches(LIVE)) regions.add(added);
          for (const inner of added.querySelectorAll(LIVE)) regions.add(inner);
        }
      }
      regions.forEach(hear);
    }).observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    globalThis.__aibrowseMutations = state;
  }
  return globalThis.__aibrowseMutations.count;
})()"#;

/// The live-region messages heard since the last call (see `MUTATION_COUNTER`), oldest
/// first; empty before the observer is installed.
pub(crate) const TAKE_ANNOUNCEMENTS: &str = r#"(() => {
  const state = globalThis.__aibrowseMutations;
  if (!state) return [];
  const said = state.said;
  state.said = [];
  return said;
})()"#;

/// `this` is the intended target, the argument is what's actually under the cursor.
/// The click is good if it lands on the target or inside it, or on an ancestor that
/// receives it because the target ignores pointer events. Ancestry is walked through
/// shadow roots (`host`), which `Node.contains` does not cross.
///
/// A hit on the target's own label is good too: Bootstrap's toggle buttons hide the
/// radio or checkbox and draw its `<label for>` (an icon with a `title`) in its place,
/// and clicking the label is how a person uses it. The label is matched by the id it
/// names as well as by `labels`: where a page repeats ids (OpenStreetMap draws its
/// route form twice), the browser gives the label to the first control with the id,
/// and the label is still the only thing a person can click.
pub(crate) const CONTAINS_HIT: &str = r#"function (hit) {
  const within = (outer, inner) => {
    for (let n = inner; n; n = n.parentNode || n.host) if (n === outer) return true;
    return false;
  };
  if (within(this, hit)) return true;
  if (within(hit, this)) return hit !== document.body && hit !== document.documentElement;
  const el = hit.nodeType === 1 ? hit : hit.parentElement;
  const label = el && el.closest('label');
  return !!label && ([...(this.labels || [])].includes(label) || (!!this.id && label.htmlFor === this.id));
}"#;

/// `this` is the intended target, the argument covers it: whether the cover is a
/// decoration of the target's own field that hands a click on to the field. bahn.de
/// shows a date in words ("Oktober 2026") in a span over its day, month and year
/// spinbuttons; a click there focuses the day. Such a cover is no control itself, and
/// it lies inside the smallest wrapper it shares with the target, which holds nothing
/// else to act on but a few fields. A dialog's backdrop or a banner shares a wrapper
/// with the target only further up, where there are buttons and links besides.
pub(crate) const FIELD_DECORATION: &str = r#"function (hit) {
  const el = hit.nodeType === 1 ? hit : hit.parentElement;
  if (!el) return false;
  const CONTROLS = 'a[href], button, input, select, textarea, label, summary, [role=button], [role=link], '
    + '[role=checkbox], [role=radio], [role=switch], [role=tab], [role=menuitem], [role=option], '
    + '[role=spinbutton], [role=textbox], [role=searchbox], [role=combobox], [role=slider], '
    + '[tabindex]:not([tabindex="-1"])';
  const FIELDS = 'input:not([type=button]):not([type=submit]):not([type=reset]):not([type=image]), '
    + 'select, textarea, [role=spinbutton], [role=textbox], [role=searchbox], [role=combobox], [role=slider]';
  if (!this.matches(FIELDS)) return false;
  const control = el.closest(CONTROLS);
  if (control && !control.contains(this)) return false;
  let wrapper = el.parentElement;
  while (wrapper && !wrapper.contains(this)) wrapper = wrapper.parentElement;
  if (!wrapper || wrapper === document.body || wrapper === document.documentElement) return false;
  const others = [...wrapper.querySelectorAll(CONTROLS)]
    .filter((c) => c !== this && !c.contains(this) && !this.contains(c));
  if (others.length > 6 || others.some((c) => !c.matches(FIELDS))) return false;
  const inner = el.getBoundingClientRect();
  const outer = wrapper.getBoundingClientRect();
  return inner.left >= outer.left - 1 && inner.top >= outer.top - 1
    && inner.right <= outer.right + 1 && inner.bottom <= outer.bottom + 1;
}"#;

/// `this` is the intended target, the argument covers it: whether the cover is an inert
/// twin of the target, drawn over it as part of the same widget. FlixBus draws its date
/// button under a read-only copy of the date (`<input readonly aria-hidden=true
/// tabindex=-1>`) in the same small wrapper; a click there is what a person does, and
/// the widget opens its calendar for it. The cover (or every control in it, for the
/// twin's own box) is kept out of the accessibility tree or the tab order
/// (`aria-hidden`, `inert`, or `tabindex=-1` while read-only or disabled), so it is no
/// control of its own; it lies inside the smallest wrapper it
/// shares with the target, which holds only a few controls, and it is about the
/// target's size. A modal's backdrop is often `aria-hidden` too, but it shares no small
/// wrapper with the target and is fixed and far larger; a dialog or banner over the
/// target is a control-bearing overlay. Both stay reported.
pub(crate) const INERT_TWIN: &str = r#"function (hit) {
  const el = hit.nodeType === 1 ? hit : hit.parentElement;
  if (!el || el.contains(this)) return false;
  const doc = this.ownerDocument;
  const hidden = (n) => !!n.closest('[aria-hidden=true], [inert]');
  const untabbable = (n) => n.getAttribute('tabindex') === '-1'
    && (n.readOnly || n.disabled || n.getAttribute('aria-readonly') === 'true' || n.getAttribute('aria-disabled') === 'true');
  const inert = (n) => hidden(n) || untabbable(n);
  // The cover is such a control, or what holds one and no other control (the twin's
  // own box, with its hidden label).
  const ANY = 'input, select, textarea, button, a[href], [tabindex]';
  const control = el.closest(ANY);
  const inner = [...el.querySelectorAll(ANY)];
  const twin = control ? inert(control) : hidden(el) || (inner.length > 0 && inner.every(inert));
  if (!twin) return false;
  if (el.closest('dialog, [role=dialog], [role=alertdialog], [aria-modal=true]')) return false;
  let wrapper = el.parentElement;
  while (wrapper && !wrapper.contains(this)) wrapper = wrapper.parentElement;
  if (!wrapper || wrapper === doc.body || wrapper === doc.documentElement) return false;
  for (let n = el; n && n !== wrapper; n = n.parentElement) {
    if (getComputedStyle(n).position === 'fixed') return false;
  }
  const CONTROLS = 'a[href], button, input, select, textarea, [role=button], [role=link], [role=combobox], '
    + '[role=checkbox], [role=radio], [role=tab], [role=menuitem], [tabindex]:not([tabindex="-1"])';
  if (wrapper.querySelectorAll(CONTROLS).length > 6) return false;
  const cover = el.getBoundingClientRect();
  const target = this.getBoundingClientRect();
  const outer = wrapper.getBoundingClientRect();
  const inside = cover.left >= outer.left - 1 && cover.top >= outer.top - 1
    && cover.right <= outer.right + 1 && cover.bottom <= outer.bottom + 1;
  return inside && cover.width * cover.height <= 4 * Math.max(1, target.width * target.height);
}"#;

/// Like `CONTAINS_HIT`, for a point (frame viewport coordinates) whose topmost node is a
/// pseudo-element. Those can't be resolved to objects, but `elementFromPoint` returns the
/// element they belong to, which is where a click there lands: the button itself for a
/// `button::after` hit-area extension, a card-wide link for a "stretched link" overlay.
/// The target's own label counts as in `CONTAINS_HIT`: OpenStreetMap draws its mode
/// toggles' icons as the labels' `::before`.
///
/// `elementFromPoint` stops at a shadow host (Otto's filter buttons live in shadow
/// roots), so it is asked again inside each open shadow root down to the deepest element
/// there; containment then walks back up across the hosts.
pub(crate) const CONTAINS_POINT: &str = r#"function (x, y) {
  let hit = this.ownerDocument.elementFromPoint(x, y);
  while (hit && hit.shadowRoot) {
    const inner = hit.shadowRoot.elementFromPoint(x, y);
    if (!inner || inner === hit) break;
    hit = inner;
  }
  if (!hit) return false;
  for (let n = hit; n; n = n.parentNode || n.host) if (n === this) return true;
  const label = hit.closest('label');
  return !!label && ([...(this.labels || [])].includes(label) || (!!this.id && label.htmlFor === this.id));
}"#;

/// `this` covers an element that was to be clicked: the controls of the overlay it
/// belongs to (its nearest fixed ancestor, dialog, alert or modal with controls), as
/// `button "Collapse banner"`, so whoever acts next knows how to dismiss it. Empty when
/// it isn't part of such an overlay.
pub(crate) const OVERLAY_CONTROLS: &str = r#"function () {
  const squash = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const shown = (e) => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden';
  const controlsOf = (root) => [...new Set([...root.querySelectorAll('button, [role=button], a[href]')]
    .filter(shown)
    .map((e) => {
      const name = squash(e.getAttribute('aria-label') || e.innerText || e.title).slice(0, 40);
      return name ? `${e.tagName === 'A' ? 'link' : 'button'} "${name}"` : '';
    })
    .filter(Boolean))];
  // The nearest overlay around it that has controls of its own: an app shell that is
  // itself fixed would list the whole page's buttons.
  for (let n = this; n && n.nodeType === 1 && n !== document.body; n = n.parentElement) {
    const role = n.getAttribute('role') || '';
    const position = getComputedStyle(n).position;
    const overlay = position === 'fixed' || n.tagName === 'DIALOG'
      || /^(dialog|alertdialog|alert)$/.test(role) || n.getAttribute('aria-modal') === 'true';
    if (!overlay) continue;
    const controls = controlsOf(n);
    if (controls.length) return controls.slice(0, 6).join(', ');
  }
  return '';
}"#;

/// `this` covers a field to type into: whether it is just another control drawn over
/// it (a language `<select>` over a collapsed search box, a floating label), rather than
/// part of a dialog or modal that the page wants dealt with first.
pub(crate) const COVER_IS_CONTROL: &str = r#"function () {
  const el = this.nodeType === 1 ? this : this.parentElement;
  if (!el) return false;
  const control = el.closest('input, select, textarea, button, a[href], label, [role=button], [role=combobox], [role=link], [role=textbox], [role=searchbox]');
  const modal = el.closest('dialog, [role=dialog], [role=alertdialog], [aria-modal=true]');
  return !!control && !modal;
}"#;

/// What the focused field holds, for verifying typed text. Read from the focused element
/// rather than the clicked one: widgets such as typeahead search boxes swap in another
/// input on focus, and keystrokes go wherever focus is, as they would for a person.
pub(crate) const FOCUSED_VALUE: &str = r#"(() => {
  let el = document.activeElement;
  while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
  if (!el) return null;
  if ('value' in el) return String(el.value);
  return el.isContentEditable ? el.innerText : null;
})()"#;
