// Release integrity tool (rules: web/INTEGRITY.md).
//   assemble --dist D --meta M --out S --version V --profile P
//       copies the bundle D into the site S and adds `_headers`, `integrity/` and an unsigned
//       `manifest.json`; prints the two hashes. M is the build's header output (`<outDir>-meta`).
//   verify --site S --pubkey K
//       fails unless S carries a manifest signed by K that matches the files and the header rules.
//       CI and the Pages deploy run it; an unsigned build fails here.
//   hash D          prints the bundle hash of a directory.
//   fingerprint K   prints the key id and the SHA-256 fingerprint of a public key file.
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseArgs } from 'node:util';
import { lintClasses } from './lib/csp.mjs';
import {
  bundleListing,
  manifestText,
  parseCanonicalHeaderSet,
  sha256,
  verifySite,
} from './lib/integrity.mjs';
import { fingerprint, parsePublicKey } from './lib/minisign.mjs';

const [command, ...rest] = process.argv.slice(2);
const { values, positionals } = parseArgs({
  args: rest,
  allowPositionals: true,
  options: {
    dist: { type: 'string' },
    meta: { type: 'string' },
    out: { type: 'string' },
    site: { type: 'string' },
    pubkey: { type: 'string' },
    version: { type: 'string' },
    profile: { type: 'string' },
  },
});

function fail(message) {
  console.error(message);
  process.exit(1);
}

try {
  if (command === 'assemble') {
    const { dist, meta, out, version, profile } = values;
    if (!dist || !meta || !out || !version || !profile)
      fail('assemble: --dist --meta --out --version --profile');
    const headerText = readFileSync(join(meta, 'headers.txt'));
    const problems = lintClasses(parseCanonicalHeaderSet(headerText.toString('utf8')));
    if (problems.length) fail(`header policy:\n  ${problems.join('\n  ')}`);
    rmSync(out, { recursive: true, force: true });
    cpSync(dist, out, { recursive: true });
    mkdirSync(join(out, 'integrity'), { recursive: true });
    const listing = bundleListing(dist);
    writeFileSync(join(out, 'integrity/files.txt'), listing);
    writeFileSync(join(out, 'integrity/headers.txt'), headerText);
    writeFileSync(join(out, '_headers'), readFileSync(join(meta, '_headers')));
    const bundleHash = sha256(listing);
    const headerSetHash = sha256(headerText);
    writeFileSync(join(out, 'manifest.json'), manifestText(version, bundleHash, headerSetHash));
    console.log(`profile ${profile} version ${version}`);
    console.log(`bundle_hash ${bundleHash}`);
    console.log(`header_set_hash ${headerSetHash}`);
  } else if (command === 'verify') {
    const { site, pubkey } = values;
    if (!site || !pubkey) fail('verify: --site --pubkey');
    const r = verifySite(site, readFileSync(pubkey, 'utf8'));
    console.log(`signed by key ${r.keyId}: ${r.trustedComment}`);
    console.log(`${r.files} files, bundle_hash ${r.manifest.bundle_hash}`);
    console.log(`header_set_hash ${r.manifest.header_set_hash}`);
  } else if (command === 'fixture-summary') {
    // What a reader of the fixture must find (INTEGRITY.md, "The test fixture").
    const [dir] = positionals;
    const site = join(dir, 'site');
    const pub = readFileSync(join(dir, 'minisign.pub'), 'utf8');
    const manifest = JSON.parse(readFileSync(join(site, 'manifest.json'), 'utf8'));
    console.log(
      JSON.stringify(
        {
          key_id: parsePublicKey(pub).keyId.toString('hex'),
          key_fingerprint_sha256: fingerprint(pub),
          trusted_comment: 'nymira-web-manifest v1 profile=fixture version=0.0.1-fixture',
          manifest,
          files: readFileSync(join(site, 'integrity/files.txt'), 'utf8').trim().split('\n').length,
        },
        null,
        2,
      ),
    );
  } else if (command === 'hash') {
    const [dir] = positionals;
    if (!dir || !existsSync(dir)) fail('hash: a directory');
    console.log(sha256(bundleListing(dir)));
  } else if (command === 'fingerprint') {
    const [file] = positionals;
    if (!file) fail('fingerprint: a public key file');
    const text = readFileSync(file, 'utf8');
    console.log(`key_id ${parsePublicKey(text).keyId.toString('hex')}`);
    console.log(`sha256 ${fingerprint(text)}`);
  } else {
    fail('usage: integrity.mjs assemble|verify|hash|fingerprint (see the header of this file)');
  }
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
