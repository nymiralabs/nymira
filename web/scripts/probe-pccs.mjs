// Opt-in probe: fetches collateral for the recorded model quote from the PCCS the app uses and
// verifies the quote at the current time. It shows what the browser path (reqwest in WASM over
// fetch) gets today. Not part of CI: it reaches a public service, and the TCB status moves.
//   node scripts/probe-pccs.mjs [pccs-url]
import { readFileSync } from 'node:fs';
import init, {
  fetch_collateral,
  verify_tdx_quote,
} from '../src/wasm/web-checks/nymira_web_wasm.js';

const pccs = process.argv[2] ?? 'https://pccs.phala.network';
await init({
  module_or_path: readFileSync(
    new URL('../src/wasm/web-checks/nymira_web_wasm_bg.wasm', import.meta.url),
  ),
});
const report = JSON.parse(
  readFileSync(
    new URL('../test-fixtures/near-ai-recorded/model-report.json', import.meta.url),
    'utf8',
  ),
);
const quote = Buffer.from(report.model_attestations[0].intel_quote, 'hex');
const started = Date.now();
const collateral = await fetch_collateral(pccs, quote);
console.log(
  `collateral fetched in ${Date.now() - started} ms (${collateral.length} bytes of JSON)`,
);
const facts = JSON.parse(
  verify_tdx_quote(quote, collateral, BigInt(Math.floor(Date.now() / 1000))),
);
console.log(
  JSON.stringify({ status: facts.status, advisory_ids: facts.advisory_ids, debug: facts.debug }),
);
