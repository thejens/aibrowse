// Part of the page reader (read.rs): the body of one function, with region.js,
// tables.js and walk.js run in that order, sharing their names. This part: the walk over the region, writing markdown; it returns the text.

const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'CANVAS', 'IFRAME',
  'BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'OPTION', 'DIALOG']);
// Site chrome around the content; skipped only when there was no explicit main region.
const CHROME = new Set(['NAV', 'HEADER', 'FOOTER', 'ASIDE']);
const BLOCK = new Set(['P', 'DIV', 'TABLE', 'SECTION', 'ARTICLE', 'MAIN', 'FORM', 'FIGURE', 'FIGCAPTION',
  'DL', 'DT', 'DD', 'DETAILS', 'SUMMARY', 'ADDRESS', 'HEADER', 'FOOTER', 'CENTER', 'TR']);
const lines = [];
let line = '';
// The marker of the list item being written ("3. ", "- "). Until the item has text of
// its own the marker waits, so that content inside a block (a card's <div>) lands on
// the marker's line rather than under a bare "3.".
let marker = '';
// Inside a list item, where its lines start: what follows a nested list in the same
// item (Yr's day: "Saturday 17 Oct.", its four periods, then "Maximum minimum
// temperature") stays in the item instead of ending the list and losing its day.
let within = '';
// Zero-width and bidi formatting characters are invisible on screen; drop them.
const INVISIBLE = /[\u200B-\u200F\u202A-\u202E\u2060-\u2064\uFEFF]/g;
const squash = (s) => (s ?? '').replace(INVISIBLE, '').replace(/\s+/g, ' ').trim();
const flush = () => {
  const t = line.replace(/[ \t]+/g, ' ').trim();
  if (marker && t === marker.trim()) return;
  // A nested item keeps its indent, so it reads as part of the item above it.
  if (t) lines.push((marker ? (marker.match(/^ */) || [''])[0] : within) + t);
  line = '';
  marker = '';
};
const gap = () => { if (lines.length && lines[lines.length - 1] !== '') lines.push(''); };
const block = (s) => {
  if (marker && line.trim() === marker.trim()) {
    // A one-line block (a heading) opening a list item goes on the item's line; a
    // longer one (code, a table) goes under its number.
    if (s && !s.includes('\n')) { line += s.replace(/^#+ /, ''); flush(); return; }
    lines.push(marker.trim());
    line = '';
    marker = '';
  }
  flush();
  if (s) { lines.push(''); lines.push(s); lines.push(''); }
};
// The words an element shows, without the buttons sites put inside headings and code
// blocks ("Copy item path", "Copy") and icons. Only what renders: a heading's
// responsive twin (a long and a short title, one of them display:none) is said once,
// and a <noscript> image fallback isn't read as raw HTML. Links styled as buttons
// stay: they often carry part of a title (IKEA's "BILLY Bookcase, white, [80x28x202
// cm]"), and so does an accordion's toggle, which holds the whole title
// (`<h3><button aria-expanded>`). With `lines`, the line breaks are kept, a block
// element (a code viewer's line `<div>`) on a line of its own: for code.
const NOT_TEXT = new Set(['BUTTON', 'SVG', 'SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'INPUT',
  'SELECT', 'TEXTAREA']);
const ownText = (el, keepLines) => {
  let out = '';
  const take = (n) => {
    if (n.nodeType === Node.TEXT_NODE) { out += n.textContent.replace(INVISIBLE, ''); return; }
    if (n.nodeType !== Node.ELEMENT_NODE) return;
    // svg's tagName is lowercase in an HTML document.
    const tag = n.tagName.toUpperCase();
    if (tag === 'BR') { out += '\n'; return; }
    if (NOT_TEXT.has(tag) && !(tag === 'BUTTON' && n.hasAttribute('aria-expanded'))) return;
    if (n.getAttribute('aria-hidden') === 'true' || !visible(n) || iconGlyph(n)) return;
    if (keepLines && n === target && !marked) {
      if (out && !out.endsWith('\n')) out += '\n';
      out += MARK + '\n';
      marked = true;
    }
    // A block's words don't run into its neighbours' ("Title" and "Subtitle").
    const own = /^(block|flex|grid|list-item|table-row)$/.test(getComputedStyle(n).display);
    const gap = keepLines ? '\n' : ' ';
    if (own && out && !out.endsWith(gap)) out += gap;
    for (const child of rendered(n)) take(child);
    if (own && !out.endsWith(gap)) out += gap;
  };
  for (const child of rendered(el)) take(child);
  return out;
};
const headingText = (el) => squash(ownText(el, false));
// What an element says, as innerText would. Inside a collapsed panel nothing is laid
// out and innerText is empty (a closed <details>' fee table read as `|  |  |`), so
// there it is the element's own words, its blocks kept apart (textContent ran a
// link's title into its subtitle: "studentRestore your status").
// An element outside HTML (an svg) has no innerText at all.
const inner = (el) => (collapsed > 0 ? ownText(el, false) : el.innerText) ?? el.textContent ?? '';
// An element's words without the names of icon-font glyphs in it ("swap_vert" in a
// sortable column's header): for the short pieces (cells, links) where icons sit.
const said = (el) => (holdsGlyph(el) ? ownText(el, false) : inner(el));
const href = (a) => { try { return new URL(a.getAttribute('href'), location.href).href; } catch { return null; } };
// display:contents elements have no box, so checkVisibility() calls them invisible even
// though their children render normally; card layouts use them heavily.
// Inside a collapsed panel (see `collapsedPanel`) nothing renders, so there only what
// its own style hides stays out.
let collapsed = 0;
// The collapsed panel being read, which its own style hides.
let panel = null;
const visible = (el) => collapsed > 0 ? getComputedStyle(el).display !== 'none'
  : !el.checkVisibility || el.checkVisibility({ visibilityProperty: true })
    || getComputedStyle(el).display === 'contents';

// A disclosure's toggle: a button that expands a panel. On help pages the answers sit
// in collapsed panels under their questions ("What do I do if my card is stolen?"),
// and they are the page's content: a reader expands them, and a text that leaves
// them out says the page holds nothing about its topic.
// Not one that opens a popup (a menu, a listbox, a dialog): those hold controls.
const POPUP = /^(menu|listbox|dialog|tree|grid)$/;
const toggles = (el) => {
  if ((el.getAttribute('aria-haspopup') || 'false') !== 'false') return false;
  const ids = (el.getAttribute('aria-controls') || '').split(/\s+/).filter(Boolean);
  const scope = el.getRootNode();
  const panels = ids.map((id) => scope.getElementById && scope.getElementById(id)).filter(Boolean);
  if (el.id) panels.push(...scope.querySelectorAll(`[role=region][aria-labelledby~="${CSS.escape(el.id)}"]`));
  return panels.length > 0 && panels.every((p) => !POPUP.test(p.getAttribute('role') || '')
    && p.tagName !== 'DIALOG' && p.getAttribute('role') !== 'tabpanel');
};
// A panel hidden because its disclosure is closed: a region labelled by a collapsed
// toggle, the target of one's aria-controls, or the body of a closed <details>. Hidden
// tab panels are not: they are alternatives to what shows (monthly or yearly prices).
const collapsedPanel = (el) => {
  if (el.getAttribute('role') === 'tabpanel') return false;
  const parent = el.parentElement;
  if (parent && parent.tagName === 'DETAILS' && !parent.open && el.tagName !== 'SUMMARY') return true;
  const scope = el.getRootNode();
  const closed = (t) => t && t.getAttribute('aria-expanded') === 'false';
  if (el.getAttribute('role') === 'region') {
    const labels = (el.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
    if (labels.some((id) => closed(scope.getElementById && scope.getElementById(id)))) return true;
  }
  return !!el.id && !!scope.querySelector(`[aria-controls~="${CSS.escape(el.id)}"][aria-expanded="false"]`);
};

// What renders inside an element, as the reader sees it (the flat tree): a web
// component's open shadow root rather than its light children, and in a <slot> the
// light nodes assigned to it, or its fallback content when none are. MDN's
// compatibility tables live two shadow roots deep; closed roots can't be read.
const rendered = (el) => {
  if (el.shadowRoot) return el.shadowRoot.childNodes;
  if (el.tagName === 'SLOT') {
    const assigned = el.assignedNodes({ flatten: true });
    return assigned.length ? assigned : el.childNodes;
  }
  return el.childNodes;
};

// A form that is nothing but its controls: a search box or a sign-in form, whose
// few other words ("Forgot password?") aren't the page's content.
const formOnly = (f) => {
  if (!f.querySelector('input[type=search], [role=searchbox], input[type=password]')) return false;
  let words = squash(inner(f)).length;
  for (const c of f.querySelectorAll('label, button, select, textarea, [role=button]')) {
    words -= squash(inner(c)).length;
  }
  return words < 200;
};

// A modal over the page (a consent or paywall prompt) is open. Sites then aria-hide
// everything behind it (Tom's Hardware hides every paragraph of its article), and
// what is behind it still renders: it is the page's content.
const modalOpen = [...document.querySelectorAll('dialog[open], [aria-modal=true], [role=dialog], [role=alertdialog]')]
  .some((d) => !d.checkVisibility || d.checkVisibility({ visibilityProperty: true }));
// An aria-hidden part that is read anyway: words, rather than an icon, behind an open
// modal; otherwise only an article's worth of paragraphs, which no site hides as
// decoration (a carousel's hidden clone of a slide is shorter).
const hiddenButRead = (el) => {
  const words = squash(inner(el)).length;
  return modalOpen ? words >= 20 : words >= 400 && el.querySelectorAll('p').length >= 2;
};

// Card-sized: several parts and a sentence's worth of words. A control's label
// ("Save changes", "Accept all and continue") is one short part.
const card = (el) => squash(inner(el)).length >= 40
  && el.querySelectorAll('div, span, p, li, strong, time, h1, h2, h3, h4, h5, h6').length >= 3;

// A button that shows a value rather than naming an action: a converter's time per
// city (timeanddate's "11:00", a button that opens a time picker), a calendar's day,
// a seat. Its words hold a digit (no control label, "Submit" or "Copy", does) and are
// short, not a bare page number or counter, and it sits in a list, a table or a row
// of results, or shows several parts of its own ("Mon" "17").
const VALUE_PLACE = 'li, td, th, [role=row], [role=cell], [role=gridcell], [role=listitem], output';
const valueButton = (el) => {
  const words = squash(inner(el));
  if (!words || words.length > 40 || !/\d/.test(words) || /^\d{1,3}$/.test(words)) return false;
  const parts = [...el.children].filter((c) => squash(inner(c))).length;
  return parts >= 2 || !!el.closest(VALUE_PLACE);
};

// Boxes side by side with no whitespace between them in the page's source (a user's
// reputation, "63.5k", and their badge count, "41"): written as they come, the words
// ran together. They stand apart when the reader sees a gap between them, or one
// starts a new line; letters split into boxes of their own for an animation touch.
// An icon between two words parts them too (GSMArena's "PD2.0", a wireless-charging
// icon, "25W").
const apart = (el) => {
  if (!/[\p{L}\p{N}]$/u.test(line)) return false;
  const b = el.getBoundingClientRect();
  if (!b.width) return false;
  if (!el.textContent.trim()) return true;
  const prev = el.previousSibling;
  if (!prev || prev.nodeType !== Node.ELEMENT_NODE) return false;
  const a = prev.getBoundingClientRect();
  if (!a.width) return false;
  return Math.max(b.left - a.right, a.left - b.right) >= 3 || b.top >= a.bottom - 1;
};

const walk = (node, indent) => {
  if (node.nodeType === Node.TEXT_NODE) { line += node.textContent.replace(INVISIBLE, '').replace(/\s+/g, ' '); return; }
  if (node.nodeType !== Node.ELEMENT_NODE) return;
  const el = node;
  if (narrowedAway(el) || iconGlyph(el)) return;
  // Upper case: an svg's (and MathML's) tagName is lower case in an HTML document.
  const tag = el.tagName.toUpperCase();
  // Where the address's #fragment points, a mark line; a block read as one piece (a
  // heading, a table) is marked before it. Inside a <pre>, ownText marks the line.
  if (target && !marked && tag !== 'PRE'
      && (el === target || (el.contains(target) && WHOLE.test(tag)))) {
    flush();
    lines.push('', MARK, '');
    marked = true;
  }
  // On a question's page that marks the answer its asker accepted (schema.org's
  // acceptedAnswer, which Stack Overflow and other Q&A sites mark up), each answer is
  // headed by what it is: the check mark that says which is accepted has no words.
  // The other answers are headed too, which ends the accepted one's section.
  const answer = answerKind(el);
  if (answer && !answers.has(el)) {
    answers.add(el);
    block(`# ${answer}`);
  }
  // A toggle's words title its panel: the heading inside it, or the words themselves.
  if ((tag === 'BUTTON' || el.getAttribute('role') === 'button') && el.hasAttribute('aria-expanded')
      && visible(el) && toggles(el)) {
    const headings = el.querySelectorAll('h1, h2, h3, h4, h5, h6');
    if (headings.length) headings.forEach((h) => walk(h, indent));
    else block(squash(said(el)));
    return;
  }
  if (el !== panel && !visible(el) && collapsedPanel(el)) {
    const outer = panel;
    panel = el;
    collapsed += 1;
    try { walk(el, indent); } finally { collapsed -= 1; panel = outer; }
    return;
  }
  // A button that holds a card (a train departure, a product tile: a time, a price,
  // a duration, in parts of their own) is content, not a control: SJ's results are
  // such buttons, and a page read without them showed no trains at all.
  if (tag === 'BUTTON' && visible(el) && card(el)) {
    flush();
    el.childNodes.forEach((child) => walk(child, indent));
    flush();
    return;
  }
  // Its parts are words apart ("Mon" "17"), on the line of what it is the value of.
  if (tag === 'BUTTON' && visible(el) && valueButton(el)) {
    line += ' ';
    el.childNodes.forEach((child) => { walk(child, indent); line += ' '; });
    return;
  }
  // Before skipping: an icon (an svg) parts the words around it.
  if (apart(el)) line += ' ';
  if (SKIP.has(tag) || (bodyRoot && CHROME.has(tag)) || (!visible(el) && el !== panel)) return;
  if (el.getAttribute('aria-hidden') === 'true' && el !== panel && !hiddenButRead(el)) return;
  // A modal dialog over the page (a cookie consent, a newsletter prompt) isn't the
  // page's content, like a <dialog>: a consent popup's policy tables came before
  // a shop's own text, and a check of the page's opening read only them.
  if (el !== root && (/^(dialog|alertdialog)$/.test(el.getAttribute('role') || '')
    || el.getAttribute('aria-modal') === 'true')) return;
  // Navigation, settings and controls are not content, even inside the main region.
  // A form's controls and labels aren't either, but what else it shows is: converters
  // and calculators show their results inside the form (timeanddate's city times).
  if (tag === 'NAV' || tag === 'LABEL' || tag === 'SEARCH' || (tag === 'FORM' && formOnly(el))) return;
  if (/^(navigation|menu|menubar|radiogroup|toolbar|tablist|search)$/.test(el.getAttribute('role') || '')) return;
  if (tag !== 'TABLE' && /^(table|grid|treegrid)$/.test(el.getAttribute('role') || '')) {
    flush();
    const own = (x) => x.parentElement && x.parentElement.closest(ROW_SCOPE) === el;
    const rowEls = [...el.querySelectorAll('[role=row]')].filter((r) => r.closest(ROW_SCOPE) === el);
    table(rowEls, (r) => [...r.querySelectorAll('[role=cell], [role=gridcell], [role=columnheader], [role=rowheader]')]
      .filter((c) => c.closest('[role=row]') === r && own(c)));
    return;
  }
  if (/^H[1-6]$/.test(tag)) { block('#'.repeat(+tag[1]) + ' ' + headingText(el)); return; }
  switch (tag) {
    case 'BR': flush(); return;
    case 'HR': block('---'); return;
    // Code without the copy buttons code blocks carry. Inline <code> stays on its
    // line; one holding several lines (a code block without <pre>) is a block.
    case 'PRE': block('```\n' + ownText(el, true).replace(/^\n+|\n+$/g, '') + '\n```'); return;
    case 'CODE': {
      // Source line breaks count only where the code's style keeps them.
      const code = /^(pre|break-spaces)/.test(getComputedStyle(el).whiteSpace)
        ? ownText(el, true).replace(/^\n+|\n+$/g, '') : squash(ownText(el, false));
      if (code.includes('\n')) block('```\n' + code + '\n```');
      else if (code.trim()) line += '`' + code + '`';
      return;
    }
    case 'IMG': { const alt = squash(el.alt || ''); if (alt) line += ` [image: ${alt}] `; return; }
    case 'A': {
      const text = squash(said(el));
      const url = href(el);
      if (!text) return;
      line += url && !url.startsWith('javascript:') ? ` [${text}](${url}) ` : ` ${text} `;
      return;
    }
    case 'UL': case 'OL': {
      flush();
      // A list of its own starts after a blank line, apart from the paragraph before
      // it: run together, a digest took them for one passage and cut it before the
      // first item (Allrecipes' ingredients under its scaling note). A nested list
      // stays with its item.
      if (!within && lines.length && lines[lines.length - 1] !== '') lines.push('');
      let n = 0;
      for (const li of el.children) {
        if (li.tagName !== 'LI' || !visible(li)) continue;
        n += 1;
        marker = '  '.repeat(indent) + (tag === 'OL' ? `${n}. ` : '- ');
        line = marker;
        const outer = within;
        within = '  '.repeat(indent + 1);
        for (const child of li.childNodes) {
          if (child.nodeType === Node.ELEMENT_NODE && (child.tagName === 'UL' || child.tagName === 'OL')) {
            flush(); walk(child, indent + 1);
          } else {
            walk(child, indent);
          }
        }
        // An item with nothing to say (an icon without a name) leaves no bare number.
        if (marker && line.trim() === marker.trim()) line = '';
        // The item's own line (its marker and words) sits where the item does.
        within = outer;
        marker = '';
        flush();
      }
      lines.push('');
      return;
    }
    case 'TABLE':
      flush();
      // A layout table's cells are read like any other boxes, below.
      if (layoutTable(el)) break;
      // Its caption, on a line of its own right before it, says what it is of (a
      // month, a day): a digest keeps such a line with the table.
      if (el.caption && visible(el.caption)) block(squash(said(el.caption)));
      table([...el.rows], (r) => [...r.cells]);
      return;
    case 'BLOCKQUOTE':
      block(squash(inner(el)).split('\n').map((l) => '> ' + l).join('\n'));
      return;
  }
  // A paragraph stands apart, after a blank line, as in markdown: with single line
  // breaks a digest took a whole article for one passage and cut it short. Not in a
  // list item, where a blank line would end the item.
  if (tag === 'P' && !within) {
    flush(); gap();
    for (const child of rendered(el)) walk(child, indent);
    flush(); gap();
    return;
  }
  const isBlock = BLOCK.has(tag);
  if (isBlock) flush();
  for (const child of rendered(el)) walk(child, indent);
  if (isBlock) flush();
  // A layout table's cells, side by side on their row's line, keep their words apart.
  if (tag === 'TD' || tag === 'TH') line += ' ';
};
walk(root, 0);
flush();
return lines.join('\n').replace(/\n{3,}/g, '\n\n').trim();
