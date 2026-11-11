import { execFileSync } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { FRAME_PATHS, classOf, cloudflareHeaders, headerClasses, headersFor } from '../headers';
import { headersAt, parseHeadersFile } from '../scripts/lib/cf-headers.mjs';
import { lintCsp, lintClasses } from '../scripts/lib/csp.mjs';
import {
  SIGNED_HEADERS,
  canonicalHeaderSet,
  classOfPath,
  parseCanonicalHeaderSet,
  verifySite,
} from '../scripts/lib/integrity.mjs';
import { generateTestKey, signForTest, verifyMinisign } from '../scripts/lib/minisign.mjs';
import { profile as mainnet } from '../src/profiles/mainnet';
import { profile as mock } from '../src/profiles/mock';
import { profile as testnet } from '../src/profiles/testnet';

const FIXTURE = join(__dirname, '../test-fixtures/integrity');
const pub = readFileSync(join(FIXTURE, 'minisign.pub'), 'utf8');
const tmps: string[] = [];
afterEach(() => {
  for (const t of tmps.splice(0)) rmSync(t, { recursive: true, force: true });
});

/** A writable copy of the fixture site. */
function site(): string {
  const dir = mkdtempSync(join(tmpdir(), 'nymira-site-'));
  tmps.push(dir);
  cpSync(join(FIXTURE, 'site'), dir, { recursive: true });
  return dir;
}

const lowercase = (set: Record<string, string>) =>
  Object.fromEntries(Object.entries(set).map(([k, v]) => [k.toLowerCase(), v]));

describe('minisign', () => {
  it('verifies a signature made by the minisign tool', () => {
    const sig = readFileSync(join(FIXTURE, 'site/manifest.json.minisig'), 'utf8');
    const r = verifyMinisign(readFileSync(join(FIXTURE, 'site/manifest.json')), sig, pub);
    expect(r.trustedComment).toBe('nymira-web-manifest v1 profile=fixture version=0.0.1-fixture');
  });

  it('refuses a changed message, a changed trusted comment, another key and a legacy signature', () => {
    const sig = readFileSync(join(FIXTURE, 'site/manifest.json.minisig'), 'utf8');
    const msg = readFileSync(join(FIXTURE, 'site/manifest.json'));
    expect(() => verifyMinisign(Buffer.concat([msg, Buffer.from(' ')]), sig, pub)).toThrow(
      /file signature/,
    );
    expect(() =>
      verifyMinisign(msg, sig.replace('version=0.0.1-fixture', 'version=9.9.9'), pub),
    ).toThrow(/trusted comment/);
    const other = generateTestKey();
    expect(() => verifyMinisign(msg, sig, other.publicText)).toThrow(/different key/);
    const lines = sig.split('\n');
    const raw = Buffer.from(lines[1]!, 'base64');
    raw.write('Ed', 0, 'latin1');
    lines[1] = raw.toString('base64');
    expect(() => verifyMinisign(msg, lines.join('\n'), pub)).toThrow(/refused/);
  });

  it('round-trips a throwaway key', () => {
    const key = generateTestKey();
    const sig = signForTest(key, Buffer.from('hello'), 'tc');
    expect(verifyMinisign(Buffer.from('hello'), sig, key.publicText).trustedComment).toBe('tc');
  });
});

describe('verifySite', () => {
  it('accepts the signed fixture', () => {
    const r = verifySite(join(FIXTURE, 'site'), pub);
    expect(r.files).toBe(4);
  });

  it('refuses an unsigned build', () => {
    const dir = site();
    rmSync(join(dir, 'manifest.json.minisig'));
    expect(() => verifySite(dir, pub)).toThrow(/unsigned build/);
  });

  it('refuses a manifest signed by another key', () => {
    const dir = site();
    const key = generateTestKey();
    const sig = signForTest(key, readFileSync(join(dir, 'manifest.json')), 'x');
    writeFileSync(join(dir, 'manifest.json.minisig'), sig);
    expect(() => verifySite(dir, pub)).toThrow(/different key/);
  });

  it('refuses a changed bundle file', () => {
    const dir = site();
    writeFileSync(join(dir, 'assets/app.js'), 'alert(1)\n');
    expect(() => verifySite(dir, pub)).toThrow(/differ from integrity\/files.txt/);
  });

  it('accepts a newer policy file beside the release and refuses any other added file', () => {
    // ADR 0021: the file is outside the signed listing; the page checks its signature itself.
    const dir = site();
    writeFileSync(join(dir, 'attestation-policy.json'), '{}');
    expect(() => verifySite(dir, pub)).not.toThrow();
    writeFileSync(join(dir, 'other-policy.json'), '{}');
    expect(() => verifySite(dir, pub)).toThrow(/differ from integrity\/files.txt/);
  });

  it('refuses an added bundle file', () => {
    const dir = site();
    writeFileSync(join(dir, 'assets/extra.js'), '1\n');
    expect(() => verifySite(dir, pub)).toThrow(/differ from integrity\/files.txt/);
  });

  it('refuses a `_headers` file that differs from the signed header set', () => {
    const dir = site();
    const text = readFileSync(join(dir, '_headers'), 'utf8');
    writeFileSync(
      join(dir, '_headers'),
      text.replace("script-src 'self'; connect", "script-src 'self' 'unsafe-eval'; connect"),
    );
    expect(() => verifySite(dir, pub)).toThrow(/_headers serves/);
  });

  it('refuses a `_headers` file whose rules overlap and so join two policies', () => {
    const dir = site();
    const text = readFileSync(join(dir, '_headers'), 'utf8');
    const csp = text.match(/\/wallet-host\n {2}(Content-Security-Policy: .*)\n/)![1]!;
    writeFileSync(join(dir, '_headers'), `${text}\n/wallet-host\n  ${csp}\n`);
    expect(() => verifySite(dir, pub)).toThrow(/_headers serves/);
  });

  it('refuses a changed header set file', () => {
    const dir = site();
    const p = join(dir, 'integrity/headers.txt');
    writeFileSync(p, readFileSync(p, 'utf8').replace('nosniff', 'sniff'));
    expect(() => verifySite(dir, pub)).toThrow(/header_set_hash/);
  });
});

describe('CSP check', () => {
  const good = headerClasses(testnet).app['Content-Security-Policy']!;

  it('passes the testnet and mainnet header sets', () => {
    for (const p of [testnet, mainnet]) {
      const classes = Object.fromEntries(
        Object.entries(headerClasses(p)).map(([k, v]) => [k, lowercase(v)]),
      );
      expect(lintClasses(classes)).toEqual([]);
    }
  });

  it('fails on unsafe-eval', () => {
    const bad = good.replace("'wasm-unsafe-eval'", "'wasm-unsafe-eval' 'unsafe-eval'");
    expect(lintCsp(bad, 'app')).toContain("script-src has 'unsafe-eval'");
    expect(lintCsp(bad, 'frame').join()).toContain("'unsafe-eval'");
  });

  it('fails on unsafe-inline scripts, wildcards, plain-http hosts and a missing policy', () => {
    expect(
      lintCsp(good.replace("script-src 'self'", "script-src 'self' 'unsafe-inline'"), 'app'),
    ).not.toEqual([]);
    expect(lintCsp(good.replace('connect-src', 'connect-src *'), 'app')).not.toEqual([]);
    expect(lintCsp(good.replace('connect-src', 'connect-src https:'), 'app')).not.toEqual([]);
    expect(
      lintCsp(good.replace('connect-src', 'connect-src http://rpc.example'), 'app'),
    ).not.toEqual([]);
    expect(
      lintCsp(good.replace("frame-ancestors 'none'", "frame-ancestors 'self'"), 'app'),
    ).not.toEqual([]);
    expect(lintCsp(good.replace("; require-trusted-types-for 'script'", ''), 'app')).not.toEqual(
      [],
    );
    expect(lintClasses({ app: {} })).toContain('app: no Content-Security-Policy');
  });

  it('refuses the mock profile: its hosts are loopback', () => {
    const classes = lowercase(headerClasses(mock).app);
    expect(lintClasses({ app: classes }).join()).toMatch(/test host/);
  });
});

describe('the generated `_headers`', () => {
  for (const [name, p] of [
    ['testnet', testnet],
    ['mainnet', mainnet],
  ] as const) {
    it(`serves ${name} the same set as headersFor, without overlap`, () => {
      const rules = parseHeadersFile(
        cloudflareHeaders(p, ['index.html', 'wallet-host.html', '404.html', '.nojekyll']),
      );
      const paths = [
        '/',
        '/index.html',
        '/index',
        '/404.html',
        '/wallet-host.html',
        '/wallet-host',
        '/assets/index-abc.js',
        '/assets/x.wasm',
        '/executors/meteor.js',
        '/manifest.json',
        '/manifest.json.minisig',
        '/integrity/files.txt',
      ];
      for (const path of paths) {
        expect(headersAt(rules, path), path).toEqual(lowercase(headersFor(p, path)));
      }
      // A path with no rule still gets the common headers.
      expect(Object.keys(headersAt(rules, '/nothing')).sort()).toEqual([
        'permissions-policy',
        'referrer-policy',
        'strict-transport-security',
        'x-content-type-options',
      ]);
    });
  }

  it('keeps the class rules of headers.ts and of the check in step', () => {
    for (const path of ['/', '/x.js', '/assets/a.js', '/executors/e.js', ...FRAME_PATHS, '/zzz']) {
      expect(classOfPath(path)).toBe(classOf(path));
    }
  });

  it('is a canonical header set that parses back', () => {
    const classes = headerClasses(testnet);
    const text = canonicalHeaderSet(classes);
    expect(canonicalHeaderSet(parseCanonicalHeaderSet(text))).toBe(text);
    for (const line of text.split('\n').filter((l) => l && !l.startsWith('#'))) {
      expect(SIGNED_HEADERS).toContain(line.slice(0, line.indexOf(':')));
    }
  });

  it('serves a Permissions-Policy that denies the unused features', () => {
    expect(headersFor(testnet, '/')['Permissions-Policy']).toMatch(/camera=\(\), /);
    expect(headersFor(testnet, '/')['Permissions-Policy']).not.toMatch(/publickey/);
  });
});

describe('assemble', () => {
  it('makes the same site twice from the same bundle', () => {
    const out = [mkdtempSync(join(tmpdir(), 'a-')), mkdtempSync(join(tmpdir(), 'b-'))];
    tmps.push(...out);
    const hashes = out.map((o) =>
      execFileSync(
        'node',
        [
          join(__dirname, '../scripts/integrity.mjs'),
          'assemble',
          '--dist',
          join(FIXTURE, 'src/bundle'),
          '--meta',
          join(FIXTURE, 'src/meta'),
          '--out',
          o,
          '--version',
          '0.0.1-fixture',
          '--profile',
          'fixture',
        ],
        { encoding: 'utf8' },
      ),
    );
    expect(hashes[0]).toBe(hashes[1]);
    expect(readFileSync(join(out[0]!, 'manifest.json'), 'utf8')).toBe(
      readFileSync(join(FIXTURE, 'site/manifest.json'), 'utf8'),
    );
  });
});
