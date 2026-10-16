// CreepJS: the "Headless" card reports "N% like headless", "N% headless" and
// "N% stealth"; lies, when found, appear as "lies (N)". Read from the rendered text.
(() => {
  const text = document.body ? document.body.innerText : '';
  const pct = (re) => {
    const m = text.match(re);
    return m ? Number(m[1]) : null;
  };
  const scores = {};
  const like = pct(/(\d+(?:\.\d+)?)%\s*like headless/i);
  const headless = pct(/(\d+(?:\.\d+)?)%\s*headless\b/i);
  const stealth = pct(/(\d+(?:\.\d+)?)%\s*stealth/i);
  const lies = pct(/lies\s*\((\d+)\)/i);
  if (like !== null) scores['like headless'] = like;
  if (headless !== null) scores['headless'] = headless;
  if (stealth !== null) scores['stealth'] = stealth;
  if (lies !== null) scores['lies'] = lies;
  const id = text.match(/FP ID:\s*(\w+)/);
  // "like headless" is only a score: desktop Chrome itself lacks several of the APIs it
  // counts (Android-only ones such as the Contacts Manager), so it is never 0 for a
  // real user. Headless, stealth (tampered natives) and lies are direct detections.
  const checks = [['headless', headless], ['stealth', stealth], ['lies', lies]]
    .filter(([, v]) => v !== null)
    .map(([name, v]) => ({ name, status: v > 0 ? 'fail' : 'pass', value: String(v) }));
  // Each card's title is followed by its 8-hex-digit hash ("Canvas 2d93d74595"); when
  // two runs' FP IDs differ, comparing these says which component differs.
  const components = {};
  for (const line of text.split('\n')) {
    const m = line.trim().match(/^([A-Za-z][\w /]{1,30}?)([0-9a-f]{8})$/);
    if (m) components[m[1].trim()] = m[2];
  }
  return {
    ready: like !== null && headless !== null && stealth !== null,
    components,
    headline: id ? `FP ID ${id[1].slice(0, 16)}` : null,
    checks,
    scores,
    excerpt: text,
  };
})()
