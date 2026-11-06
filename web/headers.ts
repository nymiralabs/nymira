// The response headers the app is written for. The e2e tests serve the build with these, so the
// app is exercised under the strict policy. The build turns the same sets into the Pages `_headers`
// file and into `integrity/headers.txt`, whose hash goes into the signed manifest (INTEGRITY.md).
//
// Two header sets:
//  - the app (every path): no inline script or style, no eval, connections only to the hosts the
//    profile names;
//  - the wallet frame (`/wallet-host.html`): NEAR Connect 0.11.4 injects a <style> element, uses
//    innerHTML and builds a srcdoc iframe with an inline <script> for each wallet. None of that
//    can run under the app's policy, so the library runs in a sandboxed iframe (no
//    `allow-same-origin`: an opaque origin with no access to the app's storage or DOM) that has a
//    relaxed policy of its own.
import type { Profile } from './src/profiles/types';
import { WALLET_HOSTS } from './wallet-hosts';

const origin = (url: string): string => new URL(url).origin;

function unique(items: string[]): string[] {
  return [...new Set(items.filter(Boolean))];
}

/** The app's policy. `extraConnect` is where later tickets add hosts (the NEAR AI gateway etc.). */
export function appCsp(p: Profile, extraConnect: string[] = []): string {
  const connect = unique([
    "'self'",
    ...p.rpcUrls.map(origin),
    ...(p.operatorUrl ? [origin(p.operatorUrl)] : []),
    // Chat: NEAR AI's gateway (attestation reports) and the PCCS that serves Intel's collateral.
    // NVIDIA's attestation service is not listed: it refuses browser requests, so none is made.
    origin(p.gatewayUrl),
    origin(p.pccsUrl),
    ...(p.oneClick.dryQuotes || p.oneClick.intentsPath !== 'none'
      ? [origin(p.oneClick.baseUrl)]
      : []),
    ...extraConnect,
  ]);
  return [
    "default-src 'none'",
    "script-src 'self' 'wasm-unsafe-eval'",
    `connect-src ${connect.join(' ')}`,
    "img-src 'self'",
    "font-src 'self'",
    "style-src 'self'",
    // The wallet frame is a page of this origin, sandboxed.
    "frame-src 'self'",
    "worker-src 'self'",
    "base-uri 'none'",
    "form-action 'none'",
    "object-src 'none'",
    "frame-ancestors 'none'",
    // No string reaches a DOM sink: React and this app only build elements.
    "require-trusted-types-for 'script'",
    // One policy, for the note worker's script URL (src/worker/client.ts).
    'trusted-types nymira-worker',
  ].join('; ');
}

/** The wallet frame's policy: relaxed inside an opaque-origin sandbox. */
export function walletFrameCsp(p: Profile): string {
  const needs = p.walletIds.map((id) => WALLET_HOSTS[id] ?? {});
  const hosts = (pick: (n: (typeof needs)[number]) => string[] | undefined) =>
    needs.flatMap((n) => pick(n) ?? []);
  // WebKit does not match 'self' for the opaque origin of a sandboxed frame, so the app's own
  // origin is named as well (the executors are fetched from it).
  const connect = unique([
    "'self'",
    origin(p.appOrigin),
    ...p.rpcUrls.map(origin),
    ...hosts((n) => n.connect),
    ...hosts((n) => n.connectOn?.[p.network as 'testnet' | 'mainnet']),
    ...p.walletConnectSrc,
  ]);
  return [
    "default-src 'none'",
    // NEAR Connect and the executors it hosts need inline script, inline style and a srcdoc frame.
    "script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'",
    "style-src 'unsafe-inline'",
    `connect-src ${connect.join(' ')}`,
    `img-src ${unique(['data:', ...hosts((n) => n.img)]).join(' ')}`,
    // Meteor's interface embeds its fonts as data: URIs.
    'font-src data:',
    `frame-src ${unique(["'self'", 'about:', 'data:', ...hosts((n) => n.frame)]).join(' ')}`,
    "base-uri 'none'",
    "form-action 'none'",
    "object-src 'none'",
    "frame-ancestors 'self'",
  ].join('; ');
}

export type HeaderSet = Record<string, string>;
export type HeaderClass = 'app' | 'assets' | 'frame';

// An empty Permissions-Policy header restricts nothing, so the value is an empty allow-list for
// each powerful feature the app never uses. WebAuthn (`publickey-credentials-get`) stays at its
// default because the passphrase-free unlock uses it.
const PERMISSIONS_POLICY = [
  'accelerometer',
  'ambient-light-sensor',
  'autoplay',
  'bluetooth',
  'camera',
  'display-capture',
  'geolocation',
  'gyroscope',
  'hid',
  'magnetometer',
  'microphone',
  'payment',
  'serial',
  'usb',
  'xr-spatial-tracking',
]
  .map((feature) => `${feature}=()`)
  .join(', ');

/** Sent on every response, whatever the path (the `/*` rule of `_headers`). */
export const COMMON: HeaderSet = {
  'Referrer-Policy': 'no-referrer',
  'Permissions-Policy': PERMISSIONS_POLICY,
  'X-Content-Type-Options': 'nosniff',
  'Strict-Transport-Security': 'max-age=63072000; includeSubDomains',
};

/** The paths that serve the wallet frame. Pages serves `/x.html` at `/x` too. */
export const FRAME_PATHS = ['/wallet-host.html', '/wallet-host'];

/** Which header set a request path gets. */
export function classOf(pathname: string): HeaderClass {
  if (FRAME_PATHS.includes(pathname)) return 'frame';
  // The sandboxed wallet frame has an opaque origin: it loads its own script chunks and the
  // executors as cross-origin (CORS) requests. These files are public.
  if (pathname.startsWith('/assets/') || pathname.startsWith('/executors/')) return 'assets';
  return 'app';
}

/** The three header sets of a build, complete (common headers included). */
export function headerClasses(p: Profile): Record<HeaderClass, HeaderSet> {
  const app = { ...COMMON, 'Content-Security-Policy': appCsp(p) };
  return {
    app,
    assets: { ...app, 'Access-Control-Allow-Origin': '*' },
    frame: { ...COMMON, 'Content-Security-Policy': walletFrameCsp(p) },
  };
}

/** Headers for a request path. */
export function headersFor(p: Profile, pathname: string): HeaderSet {
  return headerClasses(p)[classOf(pathname)];
}

/**
 * The Cloudflare Pages `_headers` file. Pages merges every matching rule and joins repeated
 * headers with a comma, so the rules must not overlap: `/*` carries the common headers and each
 * other rule names its own paths and carries the Content-Security-Policy only (plus the CORS
 * header on the two asset directories). `rootFiles` are the files at the top of the bundle.
 */
export function cloudflareHeaders(p: Profile, rootFiles: string[]): string {
  const sets = headerClasses(p);
  const lines: string[] = [
    '# Generated by the build from headers.ts. Do not edit: its hash is in the signed manifest.',
    '/*',
    ...Object.entries(COMMON).map(([name, value]) => `  ${name}: ${value}`),
    '',
  ];
  const block = (path: string, headers: HeaderSet) => {
    lines.push(path);
    for (const [name, value] of Object.entries(headers)) lines.push(`  ${name}: ${value}`);
    lines.push('');
  };
  const csp = (c: HeaderClass): HeaderSet => ({
    'Content-Security-Policy': sets[c]['Content-Security-Policy']!,
  });
  const appPaths = new Set([
    '/',
    '/index',
    '/manifest.json',
    '/manifest.json.minisig',
    '/integrity/*',
  ]);
  for (const file of rootFiles) {
    if (file.startsWith('.') || file.startsWith('_')) continue;
    const path = `/${file}`;
    if (FRAME_PATHS.includes(path) || FRAME_PATHS.includes(path.replace(/\.html$/, ''))) continue;
    appPaths.add(path);
    if (file.endsWith('.html')) appPaths.add(path.slice(0, -'.html'.length));
  }
  for (const path of [...appPaths].sort()) block(path, csp('app'));
  for (const path of FRAME_PATHS) block(path, csp('frame'));
  for (const path of ['/assets/*', '/executors/*']) {
    block(path, { ...csp('assets'), 'Access-Control-Allow-Origin': '*' });
  }
  const text = lines.join('\n');
  const rules = lines.filter((l) => l && !l.startsWith(' ') && !l.startsWith('#')).length;
  if (rules > 100) throw new Error(`_headers has ${rules} rules; Cloudflare Pages allows 100`);
  return text;
}
