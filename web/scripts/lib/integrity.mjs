// The bundle hash, the header-set hash and the manifest (rules: web/INTEGRITY.md).
import { createHash } from 'node:crypto';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { headersAt, parseHeadersFile } from './cf-headers.mjs';
import { lintClasses } from './csp.mjs';
import { verifyMinisign } from './minisign.mjs';

export const sha256 = (data) => createHash('sha256').update(data).digest('hex');

/** The headers that are signed, as lowercase names. Anything else a host adds is not compared. */
export const SIGNED_HEADERS = [
  'access-control-allow-origin',
  'content-security-policy',
  'permissions-policy',
  'referrer-policy',
  'strict-transport-security',
  'x-content-type-options',
];

/**
 * The files a deployed site holds that are not part of the bundle. `attestation-policy.json` is
 * the newer signed policy a deploy may add beside a release (ADR 0021): the page takes it only if
 * it verifies under the signer key in the bundle, so it needs no place in the signed listing.
 */
export const NON_BUNDLE = [
  '_headers',
  'attestation-policy.json',
  'manifest.json',
  'manifest.json.minisig',
  'integrity/files.txt',
  'integrity/headers.txt',
];

export const CLASSES = ['app', 'assets', 'frame'];

/** Which header class a path of the site is served with (same rule as `classOf` in headers.ts). */
export function classOfPath(pathname) {
  if (pathname === '/wallet-host.html' || pathname === '/wallet-host') return 'frame';
  if (pathname.startsWith('/assets/') || pathname.startsWith('/executors/')) return 'assets';
  return 'app';
}

/** Header set text: `# class` lines, then `name: value` lines by lowercase name; LF only. */
export function canonicalHeaderSet(classes) {
  let text = '';
  for (const name of CLASSES) {
    text += `# ${name}\n`;
    const set = Object.fromEntries(
      Object.entries(classes[name] ?? {}).map(([k, v]) => [k.toLowerCase(), v]),
    );
    for (const key of SIGNED_HEADERS) if (key in set) text += `${key}: ${set[key]}\n`;
  }
  return text;
}

/** The inverse of `canonicalHeaderSet`. */
export function parseCanonicalHeaderSet(text) {
  const classes = {};
  let current = null;
  for (const line of text.split('\n')) {
    if (!line) continue;
    if (line.startsWith('# ')) current = classes[line.slice(2)] = {};
    else {
      const colon = line.indexOf(': ');
      if (!current || colon < 1) throw new Error(`header set: bad line "${line}"`);
      current[line.slice(0, colon)] = line.slice(colon + 2);
    }
  }
  return classes;
}

/** Relative paths ('/' separators) of every file under `dir`, in byte order of their UTF-8. */
export function listFiles(dir, skip = []) {
  const out = [];
  const walk = (rel) => {
    for (const name of readdirSync(join(dir, rel))) {
      const path = rel ? `${rel}/${name}` : name;
      if (statSync(join(dir, path)).isDirectory()) walk(path);
      else if (!skip.includes(path)) out.push(path);
    }
  };
  walk('');
  return out.sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
}

/** `<sha256 hex>  <path>\n` per file: what `sha256sum` prints. */
export function bundleListing(dir, skip = []) {
  return listFiles(dir, skip)
    .map((path) => `${sha256(readFileSync(join(dir, path)))}  ${path}\n`)
    .join('');
}

export function manifestText(version, bundleHash, headerSetHash) {
  return `${JSON.stringify({ version, bundle_hash: bundleHash, header_set_hash: headerSetHash }, null, 2)}\n`;
}

const HEX64 = /^[0-9a-f]{64}$/;

/** Throws on the first problem. Returns what it checked. */
export function verifySite(site, publicKeyText) {
  const read = (path) => readFileSync(join(site, path));
  const need = (path) => {
    if (!existsSync(join(site, path))) throw new Error(`${path} is missing`);
  };
  need('manifest.json');
  if (!existsSync(join(site, 'manifest.json.minisig'))) {
    throw new Error('unsigned build: manifest.json.minisig is missing');
  }
  const manifestBytes = read('manifest.json');
  const signed = verifyMinisign(
    manifestBytes,
    read('manifest.json.minisig').toString('utf8'),
    publicKeyText,
  );
  const manifest = JSON.parse(manifestBytes.toString('utf8'));
  if (Object.keys(manifest).join() !== 'version,bundle_hash,header_set_hash') {
    throw new Error('manifest: fields must be exactly version, bundle_hash, header_set_hash');
  }
  if (!HEX64.test(manifest.bundle_hash) || !HEX64.test(manifest.header_set_hash)) {
    throw new Error('manifest: hashes must be 64 lowercase hex characters');
  }
  if (
    manifestBytes.toString('utf8') !==
    manifestText(manifest.version, manifest.bundle_hash, manifest.header_set_hash)
  ) {
    throw new Error('manifest: not in the canonical form');
  }

  // Bundle: the listing file is what the signed hash covers; the files must match the listing.
  need('integrity/files.txt');
  const files = read('integrity/files.txt');
  if (sha256(files) !== manifest.bundle_hash)
    throw new Error('integrity/files.txt does not match bundle_hash');
  const actual = bundleListing(site, NON_BUNDLE);
  if (actual !== files.toString('utf8'))
    throw new Error('the files on disk differ from integrity/files.txt');

  // Headers: the file, then the `_headers` file as Pages would apply it.
  need('integrity/headers.txt');
  const headerText = read('integrity/headers.txt');
  if (sha256(headerText) !== manifest.header_set_hash) {
    throw new Error('integrity/headers.txt does not match header_set_hash');
  }
  const classes = parseCanonicalHeaderSet(headerText.toString('utf8'));
  if (canonicalHeaderSet(classes) !== headerText.toString('utf8')) {
    throw new Error('integrity/headers.txt is not in the canonical form');
  }
  const problems = lintClasses(classes);
  if (problems.length) throw new Error(`header policy:\n  ${problems.join('\n  ')}`);
  need('_headers');
  const rules = parseHeadersFile(read('_headers').toString('utf8'));
  const probes = new Set([
    '/',
    '/wallet-host.html',
    '/wallet-host',
    '/integrity/files.txt',
    '/manifest.json',
  ]);
  for (const path of listFiles(site, NON_BUNDLE)) {
    if (path.includes('/') ? /^(assets|executors)\//.test(path) : !/^[._]/.test(path)) {
      probes.add(`/${path}`);
    }
  }
  for (const path of probes) {
    const got = headersAt(rules, path);
    const want = Object.fromEntries(
      Object.entries(classes[classOfPath(path)]).map(([k, v]) => [k, v]),
    );
    for (const key of SIGNED_HEADERS) {
      if (got[key] !== want[key]) {
        throw new Error(
          `_headers serves ${path} with ${key}: ${got[key] ?? '(none)'}, the signed set says ${want[key] ?? '(none)'}`,
        );
      }
    }
  }
  return {
    manifest,
    keyId: signed.keyId,
    trustedComment: signed.trustedComment,
    files: files.toString('utf8').split('\n').length - 1,
  };
}
