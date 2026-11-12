// Fetches NEAR AI's attestation reports now, verifies every entry (scripts/lib/verify-live.mjs)
// and writes the verified deployments for the policy signer. Exit 0 all verified, 1 anything
// failed (nothing is written). The origin is fixed; there is no way to point it elsewhere.
//   node --import ./scripts/lib/register-ts.mjs \
//     scripts/verify-live.mjs --out verified.json
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import init, * as wasm from '../src/wasm/web-checks/nymira_web_wasm.js';
import { liveQuoteVerifier, PCCS_URL } from '../src/chat/attest/intel.ts';
import { fixedGet, NEAR_AI, verifyLive } from './lib/verify-live.mjs';

const arg = (name) => {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
};
const here = (p) => fileURLToPath(new URL(p, import.meta.url));
const out = arg('--out');
if (!out) {
  console.error('usage: verify-live.mjs --out FILE');
  process.exit(2);
}
const modelsFile = readFileSync(here('../src/chat/models.ts'), 'utf8');
const models = [...modelsFile.matchAll(/id: '([^']+)'/g)].map((m) => m[1]);
try {
  await init({
    module_or_path: readFileSync(here('../src/wasm/web-checks/nymira_web_wasm_bg.wasm')),
  });
  const verified = await verifyLive({
    base: NEAR_AI,
    models,
    quote: liveQuoteVerifier(wasm, PCCS_URL),
    get: fixedGet,
  });
  writeFileSync(out, JSON.stringify(verified, null, 2));
  console.log(`verified ${verified.length} deployments of ${models.length} models and the gateway`);
} catch (e) {
  console.error(`verification failed: ${e?.code ? `${e.code}: ` : ''}${e?.message ?? e}`);
  process.exit(1);
}
