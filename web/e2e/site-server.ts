import { readFileSync, statSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import { extname, join, normalize } from 'node:path';
import { headersAt, parseHeadersFile } from '../scripts/lib/cf-headers.mjs';

const TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json',
  '.wasm': 'application/wasm',
  '.css': 'text/css; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
};

/**
 * Serves an assembled site the way Cloudflare Pages would: static files, `/x` for `/x.html`,
 * `404.html` for anything else, and the headers its `_headers` file gives for the requested path.
 * `extra` adds files that exist in the test only (they are served with the same rules).
 */
export function serveSite(dir: string, port: number, extra: Record<string, string> = {}): Promise<Server> {
  const rules = parseHeadersFile(readFileSync(join(dir, '_headers'), 'utf8'));
  const server = createServer((req, res) => {
    const pathname = decodeURIComponent(new URL(req.url ?? '/', 'http://x').pathname);
    let body: Buffer | undefined;
    let type = 'text/html; charset=utf-8';
    let status = 200;
    const virtual = extra[pathname];
    const candidates = pathname === '/' ? ['index.html'] : [pathname.slice(1), `${pathname.slice(1)}.html`];
    if (virtual !== undefined) {
      body = Buffer.from(virtual);
      type = TYPES[extname(pathname)] ?? type;
    } else {
      for (const rel of candidates) {
        const file = normalize(join(dir, rel));
        if (!file.startsWith(dir)) continue;
        try {
          if (statSync(file).isFile()) {
            body = readFileSync(file);
            type = TYPES[extname(file)] ?? 'application/octet-stream';
            break;
          }
        } catch {
          // Not this candidate.
        }
      }
    }
    if (!body) {
      body = readFileSync(join(dir, '404.html'));
      status = 404;
    }
    res.writeHead(status, { 'content-type': type, ...headersAt(rules, pathname) });
    res.end(body);
  });
  return new Promise((resolve) => server.listen(port, '127.0.0.1', () => resolve(server)));
}
