// A model of how Cloudflare Pages applies a `_headers` file (developers.cloudflare.com/pages/
// configuration/headers): every matching rule applies, in file order; a header set by more than one
// rule is joined with ", "; a line `! Name` removes the header gathered so far. `*` is a splat
// that matches anything (one per pattern); `:name` matches up to the next `/`.
export function parseHeadersFile(text) {
  const rules = [];
  for (const raw of text.replace(/\r\n/g, '\n').split('\n')) {
    if (!raw.trim() || raw.trimStart().startsWith('#')) continue;
    if (/^\s/.test(raw)) {
      const rule = rules.at(-1);
      if (!rule) throw new Error('_headers: a header before any path');
      if (raw.length > 2000) throw new Error('_headers: a line is longer than 2000 characters');
      const line = raw.trim();
      if (line.startsWith('! ')) rule.detach.push(line.slice(2).trim().toLowerCase());
      else {
        const colon = line.indexOf(':');
        if (colon < 1) throw new Error(`_headers: bad header line "${line}"`);
        rule.headers.push([
          line.slice(0, colon).trim().toLowerCase(),
          line.slice(colon + 1).trim(),
        ]);
      }
    } else {
      rules.push({ pattern: raw.trim(), headers: [], detach: [] });
    }
  }
  if (rules.length > 100) throw new Error(`_headers: ${rules.length} rules, Pages allows 100`);
  return rules;
}

function matches(pattern, pathname) {
  const source = pattern
    .replace(/[.+?^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '(.*)')
    .replace(/:[A-Za-z][A-Za-z0-9_]*/g, '([^/]+)');
  return new RegExp(`^${source}$`).test(pathname);
}

/** The headers (lowercase names) a static asset at `pathname` is served with. */
export function headersAt(rules, pathname) {
  const out = {};
  for (const rule of rules) {
    if (!matches(rule.pattern, pathname)) continue;
    for (const name of rule.detach) delete out[name];
    for (const [name, value] of rule.headers) {
      out[name] = name in out ? `${out[name]}, ${value}` : value;
    }
  }
  return out;
}
