import { execFileSync } from 'node:child_process';
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import type { Server } from 'node:http';
import { join, resolve } from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { canonicalHeaderSet, sha256, verifySite } from '../scripts/lib/integrity.mjs';
import { generateTestKey, signForTest } from '../scripts/lib/minisign.mjs';
import { serveSite } from './site-server';

// The release path end to end, on a real testnet build: build, assemble, sign with a throwaway
// key, serve with the generated `_headers` file the way Pages applies it, and load it in a browser.
// This is the closest check to the live host that runs without a Cloudflare account.
const root = resolve(import.meta.dirname, '..');
const dist = join(root, 'dist-testnet');
const site = join(root, 'site-test');
const origin = 'http://127.0.0.1:4176';
const PROFILE_ORIGIN = 'https://nymira.pages.dev';
let server: Server;
let manifest: { version: string; bundle_hash: string; header_set_hash: string };
let assetPath: string;

const PROBE_PAGE = `<!doctype html><title>probe</title>
<script>window.__inline = true</script>
<img src="https://example.com/pixel.png" alt="" />
<script src="/assets/probe.js"></script>`;

// Runs from `script-src 'self'`: code that tries what an attacker's script would.
const PROBE_JS = `(async () => {
  const out = {};
  try { eval('1'); out.eval = 'ran'; } catch { out.eval = 'blocked'; }
  try { new Function('return 1')(); out.fn = 'ran'; } catch { out.fn = 'blocked'; }
  try { await fetch('https://example.com/', { mode: 'no-cors' }); out.fetch = 'sent'; } catch { out.fetch = 'blocked'; }
  window.__probe = out;
})();`;

// Opens the page at the app's real origin: the browser asks `nymira.pages.dev`, the test answers
// from the local site with the headers its `_headers` file gives. Every other host is refused.
async function atRealOrigin(page: Page): Promise<void> {
  const real = new URL(PROFILE_ORIGIN).origin;
  await page.route('**/*', async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== real) return route.abort();
    try {
      const res = await page.request.get(`${origin}${url.pathname}${url.search}`);
      return await route.fulfill({
        status: res.status(),
        headers: res.headers(),
        body: await res.body(),
      });
    } catch (e) {
      // A request still in flight when the test ends: the browser context is already gone, and
      // reading its body throws "Response has been disposed" after the test itself has passed.
      if (page.isClosed() || /disposed|closed/i.test(String(e))) return;
      throw e;
    }
  });
}

test.beforeAll(async () => {
  test.setTimeout(300_000);
  const vite = join(root, 'node_modules/vite/bin/vite.js');
  execFileSync('node', [vite, 'build', '--mode', 'testnet', '--outDir', 'dist-testnet'], {
    cwd: root,
    stdio: 'inherit',
  });
  const cli = join(root, 'scripts/integrity.mjs');
  execFileSync(
    'node',
    [
      cli,
      'assemble',
      '--dist',
      dist,
      '--meta',
      `${dist}-meta`,
      '--out',
      site,
      '--version',
      '0.0.0-test',
      '--profile',
      'testnet',
    ],
    { stdio: 'inherit' },
  );
  const key = generateTestKey();
  writeFileSync(
    join(site, 'manifest.json.minisig'),
    signForTest(
      key,
      readFileSync(join(site, 'manifest.json')),
      'nymira-web-manifest v1 profile=testnet version=0.0.0-test',
    ),
  );
  verifySite(site, key.publicText);
  manifest = JSON.parse(readFileSync(join(site, 'manifest.json'), 'utf8'));
  assetPath = `/assets/${readdirSync(join(site, 'assets')).find((n) => n.endsWith('.js'))}`;
  server = await serveSite(site, 4176, {
    '/integrity/probe.html': PROBE_PAGE,
    '/assets/probe.js': PROBE_JS,
  });
});

test.afterAll(async () => {
  await new Promise((done) => server?.close(done));
});

test('the served header set equals the one in the signed manifest', async ({ request }) => {
  const classes: Record<string, Record<string, string>> = {};
  for (const [name, path] of [
    ['app', '/'],
    ['assets', assetPath],
    ['frame', '/wallet-host.html'],
  ] as const) {
    const res = await request.get(`${origin}${path}`);
    expect(res.status(), path).toBe(200);
    classes[name] = res.headers();
  }
  expect(sha256(canonicalHeaderSet(classes))).toBe(manifest.header_set_hash);
  // The extensionless form Pages also serves gets the frame's set, not the app's.
  const pretty = await request.get(`${origin}/wallet-host`);
  expect(pretty.headers()['content-security-policy']).toBe(
    classes.frame?.['content-security-policy'],
  );
  // Nothing is joined: no header holds two policies.
  expect(classes.app?.['content-security-policy']).not.toContain(', default-src');
});

test('the manifest and the bundle file list are served, and the footer shows the hash', async ({
  page,
  request,
}) => {
  const served = await (await request.get(`${origin}/manifest.json`)).text();
  expect(JSON.parse(served)).toEqual(manifest);
  const files = await (await request.get(`${origin}/integrity/files.txt`)).text();
  expect(sha256(files)).toBe(manifest.bundle_hash);
  await atRealOrigin(page);
  await page.goto(`${PROFILE_ORIGIN}/`);
  await expect(page.getByTestId('bundle-hash')).toContainText(manifest.bundle_hash, {
    timeout: 30_000,
  });
});

test('the served policy blocks an inline script, eval and unlisted hosts', async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as { __v: string[] };
    w.__v = [];
    document.addEventListener('securitypolicyviolation', (e) =>
      w.__v.push(`${e.effectiveDirective} ${e.blockedURI}`),
    );
  });
  await atRealOrigin(page);
  await page.goto(`${PROFILE_ORIGIN}/integrity/probe.html`);
  await page.waitForFunction(() => (window as unknown as { __probe?: unknown }).__probe);
  const result = await page.evaluate(() => {
    const w = window as unknown as {
      __inline?: boolean;
      __probe: Record<string, unknown>;
      __v: string[];
    };
    const img = document.images[0] as HTMLImageElement;
    return {
      inline: w.__inline,
      probe: w.__probe,
      violations: w.__v,
      imageWidth: img.naturalWidth,
    };
  });
  expect(result.inline).toBeUndefined();
  expect(result.probe).toMatchObject({ eval: 'blocked', fn: 'blocked', fetch: 'blocked' });
  expect(result.imageWidth).toBe(0);
  // Each refusal is the policy's doing, not a network failure. eval and new Function are turned
  // back by Trusted Types before script-src is asked; the lint test covers the missing unsafe-eval.
  const directives = result.violations.map((v) => v.split(' ')[0]);
  for (const want of ['script-src-elem', 'require-trusted-types-for', 'connect-src', 'img-src']) {
    expect(directives, want).toContain(want);
  }
});

test('an injected script, style and request in the real app are blocked', async ({ page }) => {
  await atRealOrigin(page);
  await page.goto(`${PROFILE_ORIGIN}/`);
  await expect(page.getByTestId('landing')).toBeVisible({ timeout: 30_000 });
  const result = await page.evaluate(async () => {
    const out: Record<string, string> = {};
    try {
      const script = document.createElement('script');
      script.textContent = 'window.__injected = true';
      document.head.append(script);
    } catch {
      /* blocked by Trusted Types */
    }
    out.inline = (window as unknown as { __injected?: boolean }).__injected ? 'ran' : 'blocked';
    try {
      await fetch('https://example.com/', { mode: 'no-cors' });
      out.fetch = 'sent';
    } catch {
      out.fetch = 'blocked';
    }
    return out;
  });
  expect(result).toEqual({ inline: 'blocked', fetch: 'blocked' });
});

test('an unknown path gets the 404 page with the common headers', async ({ request }) => {
  const res = await request.get(`${origin}/nothing-here`);
  expect(res.status()).toBe(404);
  expect(res.headers()['referrer-policy']).toBe('no-referrer');
});
