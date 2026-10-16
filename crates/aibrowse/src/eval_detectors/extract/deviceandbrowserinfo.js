// deviceandbrowserinfo.com "Are you a bot?" (fingerprint) and its interaction test
// (behavior, after a form submit): a headline ("You are human!") and the raw detection
// JSON, {"isBot": bool, "details": {signal: bool}}, rendered in a <pre>/<code> block.
(() => {
  const text = document.body ? document.body.innerText : '';
  let report = null;
  for (const node of document.querySelectorAll('pre, code')) {
    try {
      const value = JSON.parse(node.innerText);
      if (value && typeof value === 'object' && 'details' in value) report = value;
    } catch (e) {}
  }
  const headline = (text.match(/You are (?:a )?(?:human|bot)[^\n]*/i) || [null])[0];
  const checks = report
    ? Object.entries(report.details).map(([name, flagged]) => ({
        name,
        status: flagged === false ? 'pass' : 'fail',
        value: String(flagged),
      }))
    : [];
  if (report && 'isBot' in report) {
    checks.unshift({ name: 'isBot', status: report.isBot ? 'fail' : 'pass', value: String(report.isBot) });
  }
  return { ready: !!report, headline, checks, excerpt: text };
})()
