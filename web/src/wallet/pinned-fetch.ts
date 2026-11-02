// NEAR Connect fetches an executor's code with `fetch` every time a wallet is used. This wrapper
// answers those requests itself: it fetches the file from this origin, hashes it, compares the hash
// with the pin and only then hands the bytes on. An executor whose hash differs is never run.
// Requests for anything else go through unchanged (the frame's CSP limits where they can go).
import { toHex } from '../crypto/bytes';
import { EXECUTOR_PINS } from './manifest';

export class ExecutorHashError extends Error {}

export async function sha256Hex(bytes: ArrayBuffer): Promise<string> {
  return toHex(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)));
}

/** The pin for a request path like `/executors/meteor-wallet.js`, or null for other paths. */
export function pinForPath(pathname: string) {
  const match = /^\/executors\/([a-z0-9-]+\.js)$/.exec(pathname);
  if (!match) return null;
  return Object.values(EXECUTOR_PINS).find((pin) => pin.file === match[1]) ?? null;
}

export function installPinnedFetch(): void {
  const original = window.fetch.bind(window);
  window.fetch = async (input, init) => {
    const url = new URL(
      typeof input === 'string' || input instanceof URL ? String(input) : input.url,
      location.href,
    );
    if (url.origin === location.origin && url.pathname.startsWith('/executors/')) {
      const pin = pinForPath(url.pathname);
      if (!pin) throw new ExecutorHashError(`${url.pathname} is not a pinned executor`);
      // The query string (NEAR Connect adds a cache-busting nonce) is dropped.
      const response = await original(url.origin + url.pathname, { credentials: 'omit' });
      if (!response.ok) throw new ExecutorHashError(`executor request failed: ${response.status}`);
      const bytes = await response.arrayBuffer();
      if ((await sha256Hex(bytes)) !== pin.sha256) {
        throw new ExecutorHashError(`${pin.file} does not match its pinned SHA-256`);
      }
      return new Response(bytes, { status: 200, headers: { 'content-type': 'text/javascript' } });
    }
    return original(input, init);
  };
}
