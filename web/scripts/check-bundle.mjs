// Checks a built bundle for what must never ship in a deployable profile:
//   node scripts/check-bundle.mjs dist testnet
// The mock profile (loopback RPC, test wallet, mock vault) exists for Playwright only. A testnet or
// mainnet bundle must hold none of it, and must not contain the test-only executor. The vendored
// wallet executors are third-party code and are checked by their hashes instead.
import { createHash } from 'node:crypto';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const args = process.argv.slice(2);
const requireKeys = args.includes('--require-keys');
const [dist = 'dist', profile = 'testnet'] = args.filter((a) => !a.startsWith('--'));
const forbidden = [
  'fake-wallet',
  '127.0.0.1',
  'localhost',
  'vault.test',
  'usdc.test',
  'mock-vault',
  'nymira-test-wallet',
  // Chat test doubles: the test quote verifier and the test operator's signing exports.
  'nymira-test-quote',
  'test_operator_',
];
const textFile = /\.(js|html|css|json|map)$/;
let bad = 0;

function walk(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      // Third-party executors are checked by hash, not by text.
      if (relative(dist, path) !== 'executors') walk(path);
    } else if (textFile.test(name)) {
      const text = readFileSync(path, 'utf8');
      for (const needle of forbidden) {
        if (text.includes(needle)) {
          console.error(`${relative(dist, path)} contains "${needle}"`);
          bad += 1;
        }
      }
    }
  }
}

if (profile === 'mock') {
  console.error('the mock profile is not checked: it is never deployed');
  process.exit(2);
}
walk(dist);
try {
  readFileSync(join(dist, 'executors', 'fake-wallet.js'));
  console.error('executors/fake-wallet.js is in the bundle');
  bad += 1;
} catch {
  // Absent, as it should be.
}
// The development-setup wallet core and its keys are for the mock profile only. A deployed bundle
// carries the ceremony-setup build, and under `setup/` only the two proving keys the profile pins,
// byte for byte (the hash is read from the profile file, not from the bundle).
const profileText = readFileSync(new URL(`../src/profiles/${profile}.ts`, import.meta.url), 'utf8');
const pinned = {};
for (const [field, file] of [
  ['provingKey', 'request.pk'],
  ['withdrawalKey', 'withdrawal.pk'],
]) {
  const m = profileText.match(new RegExp(`${field}:\\s*\\{[^}]*sha256:\\s*'([0-9a-f]{64})'`));
  if (m) pinned[file] = m[1];
}
const setup = join(dist, 'setup');
const present = existsSync(setup) ? readdirSync(setup) : [];
for (const name of present) {
  const hash = pinned[name]
    ? createHash('sha256')
        .update(readFileSync(join(setup, name)))
        .digest('hex')
    : null;
  if (hash !== pinned[name]) {
    console.error(`setup/${name} is not a key the ${profile} profile pins (or its hash differs)`);
    bad += 1;
  }
}
if (requireKeys) {
  for (const name of Object.keys(pinned)) {
    if (!present.includes(name)) {
      console.error(`setup/${name} is missing from the bundle`);
      bad += 1;
    }
  }
}
const devWasm = new URL('../src/wasm/zkapi-browser-dev/zkapi_browser_bg.wasm', import.meta.url);
if (existsSync(devWasm)) {
  const devHash = createHash('sha256').update(readFileSync(devWasm)).digest('hex');
  const assets = join(dist, 'assets');
  for (const name of existsSync(assets) ? readdirSync(assets) : []) {
    if (!name.endsWith('.wasm')) continue;
    const hash = createHash('sha256')
      .update(readFileSync(join(assets, name)))
      .digest('hex');
    if (hash === devHash) {
      console.error(`assets/${name} is the dev-setup wallet core`);
      bad += 1;
    }
  }
}
if (bad > 0) process.exit(1);
console.log(`${profile} bundle: no test-only content`);
