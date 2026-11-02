// The response headers the app is written for. The e2e tests serve the build with these, so the
// app is exercised under the strict policy. The hosting ticket (#39) turns the same function into
// the Pages `_headers` file and hashes the result into the signed manifest.
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
    ...(p.oneClick.dryQuotes || p.oneClick.intentsPaths ? [origin(p.oneClick.baseUrl)] : []),
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

const COMMON: HeaderSet = {
  'Referrer-Policy': 'no-referrer',
  'Permissions-Policy': '',
  'X-Content-Type-Options': 'nosniff',
  'Strict-Transport-Security': 'max-age=63072000; includeSubDomains',
};

/** Headers for a request path. */
export function headersFor(p: Profile, pathname: string): HeaderSet {
  if (pathname === '/wallet-host.html') {
    return { ...COMMON, 'Content-Security-Policy': walletFrameCsp(p) };
  }
  const headers: HeaderSet = { ...COMMON, 'Content-Security-Policy': appCsp(p) };
  // The sandboxed wallet frame has an opaque origin: it loads its own script chunks and the
  // executors as cross-origin (CORS) requests. These files are public.
  if (pathname.startsWith('/assets/') || pathname.startsWith('/executors/')) {
    headers['Access-Control-Allow-Origin'] = '*';
  }
  return headers;
}
