// Policy checks on the header sets of a deployable build. `lintCsp` returns a list of problems;
// an empty list means the policy meets the hosting ticket (#39). Release assembly and the deploy
// refuse a build whose policy has any.
export function parseCsp(csp) {
  const out = new Map();
  for (const part of csp.split(';')) {
    const [name, ...values] = part.trim().split(/\s+/);
    if (name) out.set(name.toLowerCase(), values);
  }
  return out;
}

/** `kind` is `app` (every path but the frame) or `frame` (the sandboxed wallet frame). */
export function lintCsp(csp, kind) {
  const problems = [];
  const d = parseCsp(csp);
  const has = (name, ...want) => {
    const got = d.get(name);
    if (!got || got.length !== want.length || want.some((w, i) => got[i] !== w)) {
      problems.push(`${name} must be exactly ${want.join(' ') || '(empty)'}`);
    }
  };
  for (const [name, values] of d) {
    if (values.includes("'unsafe-eval'")) problems.push(`${name} has 'unsafe-eval'`);
    for (const v of values) {
      if (
        v === '*' ||
        v === 'https:' ||
        v === 'http:' ||
        v.startsWith('*.') ||
        v.includes('://*')
      ) {
        problems.push(`${name} has the wildcard source ${v}`);
      }
    }
  }
  has('default-src', "'none'");
  has('base-uri', "'none'");
  has('form-action', "'none'");
  has('object-src', "'none'");
  const script = d.get('script-src') ?? [];
  if (!script.includes("'self'")) problems.push("script-src lacks 'self'");
  for (const v of script) {
    const ok =
      v === "'self'" ||
      v === "'wasm-unsafe-eval'" ||
      /^'sha(256|384|512)-[A-Za-z0-9+/=]+'$/.test(v);
    if (!ok && !(kind === 'frame' && v === "'unsafe-inline'")) {
      problems.push(`script-src has ${v}`);
    }
  }
  const connect = d.get('connect-src') ?? [];
  for (const v of connect) {
    const scheme = kind === 'frame' ? '(https|wss)' : 'https';
    if (v !== "'self'" && !new RegExp(`^${scheme}://[a-z0-9.-]+(:\\d+)?$`).test(v)) {
      problems.push(`connect-src has ${v}: only 'self' and https origins`);
    }
    if (/localhost|127\.0\.0\.1|\.test$/.test(v))
      problems.push(`connect-src has the test host ${v}`);
  }
  if (kind === 'app') {
    has('style-src', "'self'");
    has('img-src', "'self'");
    has('font-src', "'self'");
    has('worker-src', "'self'");
    has('frame-ancestors', "'none'");
    has('require-trusted-types-for', "'script'");
    if (!d.has('trusted-types')) problems.push('trusted-types is missing');
    if (script.includes("'unsafe-inline'")) problems.push("script-src has 'unsafe-inline'");
  } else {
    has('frame-ancestors', "'self'");
  }
  return problems;
}

/** The checks that apply to every class: the common headers and the CSP. */
export function lintClasses(classes) {
  const problems = [];
  for (const [name, set] of Object.entries(classes)) {
    const csp = set['content-security-policy'];
    if (!csp) {
      problems.push(`${name}: no Content-Security-Policy`);
      continue;
    }
    for (const p of lintCsp(csp, name === 'frame' ? 'frame' : 'app'))
      problems.push(`${name}: ${p}`);
    if (set['referrer-policy'] !== 'no-referrer')
      problems.push(`${name}: Referrer-Policy must be no-referrer`);
    if (!/(^|, )camera=\(\)/.test(set['permissions-policy'] ?? '')) {
      problems.push(`${name}: Permissions-Policy does not deny the unused features`);
    }
    if (!/^max-age=\d{8,}/.test(set['strict-transport-security'] ?? '')) {
      problems.push(`${name}: Strict-Transport-Security needs a long max-age`);
    }
    if (set['x-content-type-options'] !== 'nosniff')
      problems.push(`${name}: X-Content-Type-Options must be nosniff`);
  }
  return problems;
}
