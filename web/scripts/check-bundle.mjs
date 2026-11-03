// Checks a built bundle for what must never ship in a deployable profile:
//   node scripts/check-bundle.mjs dist testnet
// The mock profile (loopback RPC, test wallet, mock vault) exists for Playwright only. A testnet or
// mainnet bundle must hold none of it, and must not contain the test-only executor. The vendored
// wallet executors are third-party code and are checked by their hashes instead.
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const [dist = 'dist', profile = 'testnet'] = process.argv.slice(2);
const forbidden = [
  'fake-wallet',
  '127.0.0.1',
  'localhost',
  'vault.test',
  'usdc.test',
  'mock-vault',
  'nymira-test-wallet',
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
if (bad > 0) process.exit(1);
console.log(`${profile} bundle: no test-only content`);
