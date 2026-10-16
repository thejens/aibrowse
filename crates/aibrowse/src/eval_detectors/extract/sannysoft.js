// bot.sannysoft.com: two result tables. The first marks result cells with the classes
// passed/warn/failed; the second ("Fingerprint Scanner") prints ok/WARN/FAIL.
(() => {
  const status = (cell) => {
    if (cell.classList.contains('failed')) return 'fail';
    if (cell.classList.contains('warn')) return 'warn';
    if (cell.classList.contains('passed')) return 'pass';
    const word = cell.innerText.trim().split(/\s/)[0].toLowerCase();
    return { ok: 'pass', warn: 'warn', fail: 'fail' }[word] || null;
  };
  const checks = [];
  for (const row of document.querySelectorAll('tr')) {
    if (row.cells.length < 2) continue;
    const s = status(row.cells[1]);
    if (!s) continue;
    checks.push({
      name: row.cells[0].innerText.replace(/\s+/g, ' ').trim(),
      status: s,
      value: row.cells[1].innerText.replace(/\s+/g, ' ').trim().slice(0, 160),
    });
  }
  const scanner = checks.filter((c) => /^[A-Z_]+$/.test(c.name)).length;
  return {
    ready: checks.length >= 10 && scanner >= 10,
    checks,
    excerpt: document.body ? document.body.innerText : '',
  };
})()
