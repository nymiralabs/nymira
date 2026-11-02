// Prints the test operator's state key (seed 1) and clearance key (seed 2) for the mock vault
// fixture. `build-wasm.sh` must have run.
import { readFileSync } from 'node:fs';
import * as op from '../src/wasm/web-checks-test/nymira_web_wasm.js';

op.initSync({
  module: readFileSync(new URL('../src/wasm/web-checks-test/nymira_web_wasm_bg.wasm', import.meta.url)),
});
console.log(JSON.stringify({ operator_pk: JSON.parse(op.test_operator_public_key(1n)), clearance_pk: JSON.parse(op.test_operator_public_key(2n)) }, null, 2));
