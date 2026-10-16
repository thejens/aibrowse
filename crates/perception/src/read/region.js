// Part of the page reader (read.rs): the body of one function, with region.js,
// tables.js and walk.js run in that order, sharing their names. This part: which region is read, where the address's #fragment points, and icon glyphs.

// The main region, narrowed to its dominant article when one holds most of its text
// (a README inside a repository page, a post inside a forum thread view). A main
// region counts only when it holds a real share of the page's text: some sites mark
// an empty or tiny box as main (GSMArena's `<div id=nav role=main>`), and reading only
// that read nothing. Of several, the one with the most text.
const bodyText = document.body.innerText.length;
const mains = [...document.querySelectorAll('main, [role=main]')]
  .map((m) => [m, m.innerText.length]).sort((a, b) => b[1] - a[1]);
const root = mains.length && mains[0][1] > 0 && mains[0][1] >= bodyText * 0.25 ? mains[0][0] : document.body;
const rootText = root.innerText.length;
const articles = [...root.querySelectorAll('article')].filter((a) => a.innerText.length >= rootText * 0.4);
const article = articles.length === 1 ? articles[0] : null;
// Beside the dominant article, the short regions that state facts about it stay: a
// package page's README is the article, and its version, last publish and license
// sit in a sidebar next to it (npm's "Package sidebar", crates.io's metadata column,
// its name and version in a header above). Without them the page said nothing about
// its own version. Each region on the way up from the article to the main region
// stays when it is short and not mostly links (a "related topics" list) or site
// chrome; the rest (a repository's file list, a forum's other threads) goes.
const FACTS_CHARS = 1500;
const chromeRegion = (el) => /^(NAV|HEADER|FOOTER|FORM|DIALOG|SEARCH)$/.test(el.tagName)
  || /^(navigation|banner|contentinfo|search|dialog|alertdialog|menu|menubar|toolbar|tablist)$/
    .test(el.getAttribute('role') || '');
const factRegion = (el) => {
  const words = el.innerText.trim().length;
  if (!words || words > FACTS_CHARS || chromeRegion(el)) return false;
  const linked = [...el.querySelectorAll('a')].reduce((n, a) => n + a.innerText.trim().length, 0);
  return linked < words * 0.5;
};
// The article's ancestors up to the root, and the regions kept beside them.
const path = new Set();
const beside = new Set();
if (article) {
  for (let el = article; el !== root; el = el.parentElement) {
    path.add(el.parentElement);
    for (const sib of el.parentElement.children) {
      if (sib !== el && !path.has(sib) && factRegion(sib)) beside.add(sib);
    }
  }
}
// Inside the root, along the way to the article, what is neither on the way nor kept.
const narrowedAway = (el) => article && path.has(el.parentElement) && el !== article
  && !path.has(el) && !beside.has(el);
// Site chrome is skipped only when there is no main region or article to read.
const bodyRoot = root === document.body && !article;
// The element the address's #fragment points at (RFC 6749's #section-4.1.2), marked
// in the text with MARK where it starts, so that a digest and the reached check can
// put what the link points at first. The same line is FRAGMENT_MARK in read.rs.
const MARK = '<!-- aibrowse: the address #fragment points here -->';
const target = (() => {
  let id = '';
  try { id = decodeURIComponent(location.hash.slice(1)); } catch { return null; }
  if (!id) return null;
  const el = document.getElementById(id) || document.getElementsByName(id)[0] || null;
  return el && root.contains(el) ? el : null;
})();
let marked = false;
// Blocks read as one piece, so marked before rather than where the target sits in them.
const WHOLE = /^(H[1-6]|TABLE|BLOCKQUOTE)$/;
// Answers on a question's page, the accepted one by its schema.org itemprop or class.
const isAccepted = (el) => el.getAttribute('itemprop') === 'acceptedAnswer'
  || /(^|\s)accepted-answer(\s|$)/.test(el.getAttribute('class') || '');
const hasAccepted = [...root.querySelectorAll('[itemprop=acceptedAnswer], .accepted-answer')].some(isAccepted);
const answerKind = (el) => !hasAccepted ? null : isAccepted(el) ? 'Accepted answer'
  : el.getAttribute('itemprop') === 'suggestedAnswer' ? 'Answer' : null;
// Answers already headed.
const answers = new Set();
// An icon drawn with an icon font: its text is a ligature's name ("arrow_upward_alt",
// "swap_vert" in Material Symbols) or a private-use character, never words. Only a
// leaf holding one such word can be one, which keeps the style lookups few.
const ICON_FONT = /material (icons|symbols)|font ?awesome|glyphicons|icomoon|ionicons|bootstrap-icons|fontello/i;
const iconGlyph = (el) => {
  if (el.childElementCount || !/^([a-z0-9_]+|[\uE000-\uF8FF]+)$/.test(el.textContent.trim())) return false;
  return /(^|\s)material-(icons|symbols)/.test(el.getAttribute('class') || '')
    || ICON_FONT.test(getComputedStyle(el).fontFamily);
};
// Whether an element holds an icon-font glyph, so that its words must be taken one by
// one (ownText) rather than as innerText, which would include the glyph's name.
const holdsGlyph = (el) => [...el.querySelectorAll('span, i, em, b, div')]
  .some(iconGlyph);
