// Checks a deployed copy of a release against the site that was built and signed.
//   node scripts/check-live.mjs <base-url> <site-dir> [--no-headers]
// It fetches `manifest.json` and compares it byte for byte, fetches every file of the bundle
// listing and compares its SHA-256, and (unless --no-headers) compares the signed headers of the
// probe paths with the signed header set. Run it after each deploy, against Cloudflare Pages and
// against the GitHub Pages mirror (--no-headers: a static host cannot set them). It is a smoke
// test for a deploy, not `nymirad verify-web`: it runs from one place, over one network path.
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { classOfPath, parseCanonicalHeaderSet, sha256, SIGNED_HEADERS } from './lib/integrity.mjs';

/**
 * What to say about one signed header of one probed path: `null` when it matches, `{ note }` for
 * the one difference Cloudflare Pages is known to add, otherwise `{ problem }`.
 *
 * Cloudflare Pages adds `access-control-allow-origin: *` to the HTML documents, and a `_headers`
 * file cannot remove it (open item 136). The documents are public static files, so that value is
 * reported and accepted, on those two paths and only with that value. Any other header, value or
 * path stays a problem.
 */
export function headerVerdict(path, key, got, want) {
  if (got === want) return null;
  const shown = `${path}: ${key} is ${got ?? '(none)'}, the signed set says ${want ?? '(none)'}`;
  const documents = path === '/' || path === '/wallet-host.html';
  if (documents && key === 'access-control-allow-origin' && want === null && got === '*') {
    return { note: `${shown} (added by Cloudflare Pages, accepted)` };
  }
  return { problem: shown };
}

export async function checkLive(base, site, { headers = true } = {}) {
  const root = base.endsWith('/') ? base : `${base}/`;
  const get = async (path) => {
    const res = await fetch(new URL(path.replace(/^\//, ''), root), { redirect: 'follow' });
    return { res, body: Buffer.from(await res.arrayBuffer()) };
  };
  const problems = [];
  problems.notes = [];
  const manifest = await get('manifest.json');
  if (!manifest.res.ok || !manifest.body.equals(readFileSync(join(site, 'manifest.json')))) {
    problems.push('manifest.json is not the signed one');
  }
  // A newer signed policy beside the release (ADR 0021): when the site directory has one, the
  // host must serve those bytes.
  if (existsSync(join(site, 'attestation-policy.json'))) {
    const side = await get('attestation-policy.json');
    if (!side.res.ok || !side.body.equals(readFileSync(join(site, 'attestation-policy.json')))) {
      problems.push('attestation-policy.json is not the one that was deployed');
    }
  }
  const listing = readFileSync(join(site, 'integrity/files.txt'), 'utf8');
  for (const line of listing.split('\n').filter(Boolean)) {
    const [hash, path] = [line.slice(0, 64), line.slice(66)];
    // Hosts serve 404.html only with a 404 status, so it is not fetched.
    if (path === '404.html') continue;
    const { res, body } = await get(path === 'index.html' ? '' : path);
    if (!res.ok || sha256(body) !== hash)
      problems.push(`${path}: the served bytes differ from the listing`);
  }
  if (headers) {
    const classes = parseCanonicalHeaderSet(
      readFileSync(join(site, 'integrity/headers.txt'), 'utf8'),
    );
    const asset = listing
      .split('\n')
      .find((l) => / assets\/.+\.js$/.test(l))
      ?.slice(66);
    const probes = ['/', '/wallet-host.html', asset && `/${asset}`].filter(Boolean);
    for (const path of probes) {
      const { res } = await get(path);
      const want = classes[classOfPath(path)];
      for (const key of SIGNED_HEADERS) {
        const verdict = headerVerdict(path, key, res.headers.get(key), want[key] ?? null);
        if (verdict?.problem) problems.push(verdict.problem);
        else if (verdict?.note) problems.notes.push(verdict.note);
      }
    }
  }
  return problems;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [base, site, flag] = process.argv.slice(2);
  if (!base || !site) {
    console.error('usage: check-live.mjs <base-url> <site-dir> [--no-headers]');
    process.exit(2);
  }
  const problems = await checkLive(base, site, { headers: flag !== '--no-headers' });
  for (const note of problems.notes) console.log(`note: ${note}`);
  if (problems.length) {
    console.error(problems.join('\n'));
    process.exit(1);
  }
  console.log(`${base}: matches the signed release`);
}
