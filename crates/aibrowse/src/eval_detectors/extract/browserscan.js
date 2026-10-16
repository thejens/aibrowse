// BrowserScan bot detection: "Test Results:" then the overall verdict, then each check
// as a name line followed by a status line (Normal, or something else when flagged).
(() => {
  const text = document.body ? document.body.innerText : '';
  const lines = text.split('\n').map((l) => l.trim()).filter(Boolean);
  const at = lines.findIndex((l) => /^test results:?$/i.test(l));
  const headline = at >= 0 ? lines[at + 1] : null;
  const verdicts = /^(normal|abnormal|robot|bot|detected|suspicious|warning|unknown)$/i;
  const checks = [];
  const seen = new Set();
  for (let i = 1; i < lines.length; i++) {
    if (!verdicts.test(lines[i]) || verdicts.test(lines[i - 1]) || i - 1 === at) continue;
    const name = lines[i - 1];
    if (name.length > 40 || seen.has(name)) continue;
    seen.add(name);
    const word = lines[i].toLowerCase();
    checks.push({
      name,
      status: word === 'normal' ? 'pass' : word === 'unknown' || word === 'warning' ? 'warn' : 'fail',
      value: lines[i],
    });
  }
  return {
    ready: !!headline && checks.length >= 5,
    headline,
    checks,
    excerpt: text,
  };
})()
