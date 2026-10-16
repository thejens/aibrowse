// The addresses a page's pagination leads to, read only: see PAGING_LINKS in fanout.rs.
(() => {
  const here = new URL(location.href);
  const found = new Set();
  // A bar's links, or a link itself.
  const hide = (el) => {
    const links = el.matches('a[href]') ? [el] : [...el.querySelectorAll('a[href]')];
    for (const a of links) { try { found.add(new URL(a.href, here).href); } catch {} }
  };
  const named = '[class*="paginat" i], [class*="pager" i], [id*="paginat" i], [id*="pager" i], [aria-label*="paginat" i], [aria-label*="pager" i]';
  for (const bar of document.querySelectorAll(named)) {
    // A wrapper around the list itself ("has-pager" on a layout div) is no bar.
    if (bar.querySelectorAll('a, button').length <= 30 && (bar.innerText || '').length <= 300) hide(bar);
  }
  const number = (v) => v === null || /^\d{1,4}$/.test(v);
  // Each short link to this page that changes one numbered parameter, by parameter.
  const by = new Map();
  for (const a of document.querySelectorAll('a[href]')) {
    if (a.relList.contains('next') || a.relList.contains('prev')) { hide(a); continue; }
    // An item's link carries its title; a page number or "next ›" is short.
    const text = (a.innerText || '').trim();
    if (text.length > 24) continue;
    let to;
    try { to = new URL(a.href, here); } catch { continue; }
    if (to.origin !== here.origin || to.pathname !== here.pathname) continue;
    const keys = new Set([...here.searchParams.keys(), ...to.searchParams.keys()]);
    const changed = [...keys].filter((k) => here.searchParams.get(k) !== to.searchParams.get(k));
    if (changed.length !== 1) continue;
    const key = changed[0];
    if (!number(here.searchParams.get(key)) || !number(to.searchParams.get(key))) continue;
    const links = by.get(key) || { pages: false, links: [] };
    links.pages ||= /^\d{1,4}$/.test(text);
    links.links.push(a);
    by.set(key, links);
  }
  for (const { pages, links } of by.values()) if (pages) links.forEach(hide);
  return [...found];
})()
