//! In-page scripts reading range sliders for `Tab::set_range`: what a slider is set to,
//! where its thumb and track are, and how to find it again once the page draws it anew.
//!
//! Three kinds are read: a native `<input type=range>`, an ARIA slider
//! (`aria-valuenow`), and a handle a widget library draws without either (jQuery UI's
//! range slider: `<span tabindex=0 class="ui-slider-handle" style="left: 15%">` in a
//! `<div class="ui-slider">`). Such a handle says where it is only by its position on the
//! track; what that means in the page's units ($75) is read where a person reads it, in
//! the readout the page shows beside the slider ("$75 - $300").

/// `(el) => { min, max, now }` for any slider kind, or `null` for anything else.
///
/// A drawn handle is told by its class names, the way `perception::widgets` tells it: a
/// focusable element (keyboard use needs it) whose class says it is a slider's handle,
/// or a handle in a track whose class says slider. Its share of the track is its inline
/// `left` (`bottom` on a vertical track) in percent, as jQuery UI sets it, or else its
/// middle against the track. The page's units come from a readout near the track (an
/// input's value, an `<output>`, or a short text) with one number per handle: two
/// handles and their two numbers fix the scale; a lone handle's number is taken to count
/// from zero. A readout whose scale would start below zero (tick labels "0 500") isn't
/// one. Without a readout the handle is set in percent of its track.
macro_rules! slider_state {
    () => {
        r#"((el) => {
  const num = (v) => { const n = parseFloat(v); return Number.isFinite(n) ? n : null; };
  if (el.matches('input[type=range]')) {
    const now = num(el.value);
    return now === null ? null : { min: num(el.min) ?? 0, max: num(el.max) ?? 100, now };
  }
  if (el.hasAttribute('aria-valuenow')) {
    const now = num(el.getAttribute('aria-valuenow'));
    return now === null ? null : {
      min: num(el.getAttribute('aria-valuemin')) ?? 0,
      max: num(el.getAttribute('aria-valuemax')) ?? 100,
      now,
    };
  }
  const words = (n) => [...((n && n.classList) || [])].map((t) => t.toLowerCase());
  const handle = (n) => n.hasAttribute('tabindex') && (
    words(n).some((t) => t.includes('slider') && t.includes('handle'))
    || (words(n).some((t) => t.includes('handle')) && words(n.parentElement).some((t) => t.includes('slider'))));
  if (!handle(el) || !el.parentElement) return null;
  const track = el.parentElement;
  const handles = [...track.children].filter(handle);
  const t = track.getBoundingClientRect();
  const vertical = t.height > t.width;
  const share = (h) => {
    const set = vertical ? h.style.bottom : h.style.left;
    if (set && set.trim().endsWith('%')) return parseFloat(set) / 100;
    const b = h.getBoundingClientRect();
    if (vertical) return t.height > 0 ? (t.bottom - (b.top + b.height / 2)) / t.height : null;
    return t.width > 0 ? (b.left + b.width / 2 - t.left) / t.width : null;
  };
  const shares = handles.map(share);
  if (shares.some((s) => s === null || !Number.isFinite(s))) return null;
  const at = handles.indexOf(el);
  const round = (v) => Math.round(v * 1000) / 1000;
  // Numbers as written: "1,000" and "1.000" group thousands, "2,5" and "2.5" have
  // decimals. A dash between two numbers is a range ("$75 - $300"), not a sign.
  const parse = (token) => {
    const t = token.replace(/[.,]+$/, '');
    const marks = t.match(/[.,]/g) || [];
    if (!marks.length) return parseFloat(t);
    const last = Math.max(t.lastIndexOf('.'), t.lastIndexOf(','));
    const grouped = new Set(marks).size === 1 && t.split(marks[0]).slice(1).every((g) => g.length === 3);
    if (grouped) return parseFloat(t.replace(/[.,]/g, ''));
    return parseFloat(t.slice(0, last).replace(/[.,]/g, '') + '.' + t.slice(last + 1));
  };
  const numbers = (text) => (String(text || '').match(/\d[\d.,]*/g) || []).map(parse);
  const fits = (found) => {
    if (found.length !== handles.length || found.some((n) => !Number.isFinite(n))) return null;
    if (handles.length === 1) {
      return shares[0] > 0.005 ? { min: 0, max: round(found[0] / shares[0]) } : null;
    }
    const [p0, p1] = [shares[0], shares[handles.length - 1]];
    const [n0, n1] = [found[0], found[found.length - 1]];
    if (p1 - p0 < 0.005 || n1 < n0) return null;
    const scale = (n1 - n0) / (p1 - p0);
    const min = n0 - p0 * scale;
    if (min < -scale * 0.005) return null;
    return { min: round(Math.max(0, min)), max: round(Math.max(0, min) + scale) };
  };
  const doc = el.ownerDocument;
  for (let scope = track.parentElement, k = 0; scope && k < 3; scope = scope.parentElement, k++) {
    const outside = (n) => !track.contains(n) && !n.contains(track);
    const fields = [...scope.querySelectorAll('input:not([type=range]):not([type=hidden]), output')]
      .filter(outside).map((f) => f.value || f.textContent);
    const texts = [...scope.querySelectorAll('*')]
      .filter((n) => outside(n) && !n.matches('input, output, script, style') && (n.textContent || '').length <= 60)
      .map((n) => n.textContent);
    for (const text of [...fields, ...texts]) {
      const found = numbers(text);
      const scale = fits(found);
      if (scale) return { ...scale, now: found[at] };
    }
    if (scope === doc.body) break;
  }
  return { min: 0, max: 100, now: round(shares[at] * 100) };
})"#
    };
}

#[derive(serde::Deserialize)]
pub(crate) struct RangeGeometry {
    pub min: f64,
    pub max: f64,
    pub now: f64,
    /// Width of the track in CSS pixels, and where it starts in its frame's viewport.
    pub track: f64,
    pub track_left: f64,
    /// What can be grabbed, in its frame's viewport: [x, y, width, height].
    pub handle: [f64; 4],
}

/// A slider's min, max and value, and its track's width: a native range input is its
/// own track; an ARIA slider's thumb, or a drawn handle, moves along the nearest
/// ancestor several times its width.
pub(crate) const RANGE_GEOMETRY: &str = concat!(
    "function () {\n  const state = ",
    slider_state!(),
    r#"(this);
  if (!state) return null;
  const { min, max, now } = state;
  const native = this.matches('input[type=range]');
  // The handle: the slider, or the nearest ancestor with a box (the thumb a page
  // draws around a visually hidden range input). A range input hidden by clipping or
  // transparency (MUI's) keeps a box the thumb's size, and isn't its own track.
  const style = getComputedStyle(this);
  const hidden = native && ((style.clipPath && style.clipPath !== 'none')
    || (style.clip && style.clip !== 'auto') || parseFloat(style.opacity) === 0);
  let handle = hidden ? this.parentElement : this;
  while (handle && handle.getBoundingClientRect().width < 2) handle = handle.parentElement;
  // Only the page itself has a box: the slider isn't laid out yet (a panel opening).
  if (!handle || handle === this.ownerDocument.body || handle === this.ownerDocument.documentElement) return null;
  const box = handle.getBoundingClientRect();
  // A native range input drawn by the browser is its own track. A drawn thumb is
  // positioned inside its track, which is so its offset parent (MUI's slider root);
  // failing that, the nearest ancestor several times its width.
  // The page itself is never the track: without one, the slider isn't dragged.
  const page = (n) => !n || n === this.ownerDocument.body || n === this.ownerDocument.documentElement;
  let track = box;
  if (handle !== this || !native) {
    const parent = handle.offsetParent;
    const own = !page(parent) && parent.getBoundingClientRect();
    if (own && own.width >= box.width * 3) {
      track = own;
    } else {
      for (let n = handle.parentElement; !page(n); n = n.parentElement) {
        const r = n.getBoundingClientRect();
        if (r.width >= box.width * 3) { track = r; break; }
      }
    }
  }
  // A native range input is its own track, and its thumb's center travels it from half
  // a thumb in at one end to half a thumb in at the other: the value's share of that
  // span is where the thumb is (pressing elsewhere would jump the value there).
  const own = handle === this && native;
  const thumb = Math.min(box.height, 16) || 16;
  const share = max > min ? (now - min) / (max - min) : 0;
  const at = own ? box.x + thumb / 2 + share * (box.width - thumb) : 0;
  return {
    min, max, now,
    track: own ? box.width - thumb : (track === box ? box.width : track.width),
    track_left: own ? box.x + thumb / 2 : track.x,
    handle: own ? [at - thumb / 2, box.y, thumb, box.height] : [box.x, box.y, box.width, box.height],
  };
}"#
);

/// What a document's sliders are, in order: drawn handles by the class names that make
/// them one (`slider_state!`), as far as a selector can say.
macro_rules! sliders_selector {
    () => {
        "input[type=range], [role=slider], [tabindex][class*=slider i][class*=handle i]"
    };
}

/// A document's sliders, in order.
pub(crate) const SLIDERS: &str = concat!(
    "[...document.querySelectorAll('",
    sliders_selector!(),
    "')]"
);
pub(crate) const SLIDER_INDEX: &str = concat!(
    "function () { return [...this.ownerDocument.querySelectorAll('",
    sliders_selector!(),
    "')].indexOf(this); }"
);

/// What tells a slider from another: its range, its name and where it is.
pub(crate) const SLIDER_MARK: &str = concat!(
    "function () {\n  const state = ",
    slider_state!(),
    r#"(this) || {};
  const r = this.getBoundingClientRect();
  const name = (this.getAttribute('aria-label') || (this.labels && this.labels[0] && this.labels[0].innerText) || '').trim();
  return { min: String(state.min), max: String(state.max), name, x: r.x, y: r.y };
}"#
);

#[derive(serde::Deserialize)]
pub(crate) struct SliderMark {
    min: String,
    max: String,
    name: String,
    x: f64,
    y: f64,
}

impl SliderMark {
    /// The same slider drawn anew: the same range and name, in about the same place.
    /// Another slider that now has the old one's index (its panel closed) is not it.
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self.min == other.min
            && self.max == other.max
            && self.name == other.name
            && (self.x - other.x).abs() <= MARK_DRIFT
            && (self.y - other.y).abs() <= MARK_DRIFT
    }
}

/// How far a slider drawn anew may have moved and still be the same one (CSS pixels): a
/// page draws it anew where it was. Another slider moves up into its place when its
/// panel closes, but not exactly there (18 and 12 px off in the test).
const MARK_DRIFT: f64 = 2.0;

/// A slider's value: a native range input's, an ARIA slider's `aria-valuenow`, or a
/// drawn handle's as its readout says.
pub(crate) const RANGE_VALUE: &str = concat!(
    r#"function () {
  // A slider drawn anew leaves this one detached: still readable, no longer the one.
  if (!this.isConnected) return null;
  const state = "#,
    slider_state!(),
    r#"(this);
  return state && Number.isFinite(state.now) ? state.now : null;
}"#
);
