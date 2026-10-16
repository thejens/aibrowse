// Part of the page reader (read.rs): the body of one function, with region.js,
// tables.js and walk.js run in that order, sharing their names. This part: tables.

// Tables, HTML or ARIA (role=table/grid, common in div-built comparison grids). A cell
// whose only content is an unlabelled icon (a checkmark, a dash) gets a token per
// distinct icon shape: the meaning isn't in the page's text, but identical icons share
// a token, so a reader can tell "included" from "not" by comparing rows and columns.
const table = (rowEls, cellsOf) => {
  const icons = new Map();
  const iconOf = (c) => {
    const art = c.querySelector('svg, img');
    if (!art) return '';
    const shape = art.tagName === 'IMG' ? (art.getAttribute('src') || '')
      : [...art.querySelectorAll('path, use, circle, rect, polyline, line')]
        .map((p) => p.getAttribute('d') || p.getAttribute('href') || p.getAttribute('points') || p.tagName).join('|');
    if (!icons.has(shape)) icons.set(shape, { token: `◆${icons.size + 1}`, count: 0 });
    const icon = icons.get(shape);
    icon.count += 1;
    return icon.token;
  };
  // What the cell shows first: a cell's aria-label often names its link's action
  // ("View Team profile") rather than what it says ("Manchester City"). The label
  // speaks for a cell that shows no text, then the icon.
  const text = (c) => {
    const own = squash(said(c)) || squash(c.getAttribute('aria-label') || '') || named(c);
    return (own || iconOf(c)).replace(/\|/g, '\\|');
  };
  // A cell that shows only an icon, by the icon's name: its title, its image's alt
  // text, a label on it. A header cell's own hidden words count too: MDN's compat
  // table heads its columns with browser logos, each over a hidden "Chrome",
  // "Firefox", and without them its numbers belonged to no browser.
  const named = (c) => {
    const title = squash(c.getAttribute('title') || '');
    if (title) return title;
    for (const art of c.querySelectorAll('img[alt], [aria-label], [title], svg title')) {
      const name = squash(art.tagName === 'IMG' ? art.alt : art.tagName.toLowerCase() === 'title'
        ? art.textContent : art.getAttribute('aria-label') || art.getAttribute('title'));
      if (name) return name;
    }
    const header = c.tagName === 'TH' || /^(columnheader|rowheader)$/.test(c.getAttribute('role') || '');
    const hidden = header ? squash(c.textContent) : '';
    return hidden.length <= 40 ? hidden : '';
  };
  // Cells laid out on a grid, as they show: a cell spanning rows repeats its text on
  // each (GSMArena's "Battery" beside both its "Type" and "Charging" rows, which
  // otherwise shifted a cell left), one spanning columns leaves the others empty.
  const shown = rowEls.filter(visible);
  if (!shown.length) return;
  const grid = shown.map(() => []);
  const span = (c, axis) => {
    const n = c[`${axis}Span`] ?? +(c.getAttribute(`aria-${axis}span`) || 1);
    // rowspan=0 spans the rest of the table.
    return n === 0 && axis === 'row' ? shown.length : Math.min(Math.max(n || 1, 1), 1000);
  };
  shown.forEach((r, y) => {
    let x = 0;
    for (const c of cellsOf(r)) {
      while (grid[y][x] !== undefined) x += 1;
      const t = text(c);
      const across = span(c, 'col');
      for (let dy = 0; dy < span(c, 'row') && y + dy < grid.length; dy += 1) {
        for (let dx = 0; dx < across; dx += 1) grid[y + dy][x + dx] = dx === 0 ? t : '';
      }
      x += across;
    }
  });
  const rows = grid.map((r) => Array.from(r, (t) => t ?? ''));
  const width = Math.max(...rows.map((r) => r.length));
  const lines = rows.map((r) => '| ' + [...r, ...Array(width - r.length).fill('')].join(' | ') + ' |');
  lines.splice(1, 0, '|' + ' --- |'.repeat(width));
  if (icons.size) {
    const legend = [...icons.values()].map((i) => `${i.token} ×${i.count}`).join(', ');
    lines.push('', `(icon-only cells, no text in the page: ${legend}; the same token is the same icon)`);
  }
  block(lines.join('\n'));
};
const ROW_SCOPE = 'table, [role=table], [role=grid], [role=treegrid]';
// A table used for layout rather than data (Hacker News: a thread is tables in table
// cells, with no headers): read as a table, the whole thread became one cell. One
// is presentational by its role, holds other tables or sits in one (a table walked
// into is in a layout table: a data table's cells are read as text), has a single
// column, or has no header cells and cells holding blocks of prose.
const layoutTable = (t) => {
  if (/^(presentation|none)$/.test(t.getAttribute('role') || '')) return true;
  if (t.querySelector('table')) return true;
  const cells = [...t.rows].flatMap((r) => [...r.cells]);
  if (cells.some((c) => c.tagName === 'TH')) return false;
  if (t.parentElement && t.parentElement.closest('table')) return true;
  if ([...t.rows].every((r) => r.cells.length <= 1)) return true;
  return cells.some((c) => c.querySelector('p, ul, ol, pre, blockquote, h1, h2, h3, h4, h5, h6')
    || squash(inner(c)).length > 300);
};
