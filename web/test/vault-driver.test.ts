// The web side of `crates/nymirad/tests/web_interop.rs`: that Rust test runs this file twice
// (phase `generate`, then `prove`) with a job file, and sends what it writes to a sandbox vault.
// Skipped unless `NYMIRA_VAULT_JOB` names the job file, so `npm run test:unit` does not run it.
//
// It runs the same web code as the close and exit screens (`generateNote`, `confirmNote`,
// `prepareWithdrawal`, `callArgs`) with the same WASM builds, and maps the vault and the payout
// account with the same near-codec calls as `src/worker/note.worker.ts`. It does not go through
// the worker, the wallet connector or the UI.
import { readFileSync, writeFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  browser_confirm_deposit,
  browser_generate_deposit,
  browser_prepare_withdrawal,
  initSync,
} from '../src/wasm/zkapi-browser-dev/zkapi_browser.js';
import * as codec from '../src/wasm/near-codec/nymira_near_codec_wasm.js';
import * as op from '../src/wasm/web-checks-test/nymira_web_wasm.js';
import { importAesKey } from '../src/crypto/aead';
import { toHex } from '../src/crypto/bytes';
import { confirmNote, generateNote, type NoteWasm } from '../src/note/core';
import { callArgs, planParts, prepareWithdrawal, type WithdrawalWasm } from '../src/note/exit';

const job = process.env.NYMIRA_VAULT_JOB;
const SETUP = new URL('../../vendor/zkapi/protocol/setup/v2-concession-dev/', import.meta.url);
const KINDS = ['close', 'exit'] as const;
const aadOf = (kind: string) => `nymira:v1:note:testnet:vault.test:${kind}`;
const rel = (p: string) => new URL(p, import.meta.url);

const noteWasm: NoteWasm = {
  generateDeposit: browser_generate_deposit,
  confirmDeposit: browser_confirm_deposit,
};

describe.skipIf(!job)('vault driver', () => {
  it('runs the phase the Rust test asks for', async () => {
    const spec = JSON.parse(readFileSync(job!, 'utf8')) as Record<string, any>;
    const dir = spec.dir as string;
    initSync({ module: readFileSync(rel('../src/wasm/zkapi-browser-dev/zkapi_browser_bg.wasm')) });
    op.initSync({
      module: readFileSync(rel('../src/wasm/web-checks-test/nymira_web_wasm_bg.wasm')),
    });
    codec.initSync({
      module: readFileSync(rel('../src/wasm/near-codec/nymira_near_codec_wasm_bg.wasm')),
    });

    if (spec.phase === 'generate') {
      const raw = crypto.getRandomValues(new Uint8Array(32));
      const key = await importAesKey(raw);
      const notes: Record<string, unknown> = {};
      const commitments: Record<string, string> = {};
      for (const kind of KINDS) {
        const made = await generateNote(noteWasm, key, aadOf(kind));
        notes[kind] = made.sealed;
        commitments[kind] = made.commitment;
      }
      writeFileSync(`${dir}/state.json`, JSON.stringify({ key: toHex(raw), notes }));
      writeFileSync(`${dir}/commitments.json`, JSON.stringify(commitments));
      return;
    }

    expect(spec.phase).toBe('prove');
    const state = JSON.parse(readFileSync(`${dir}/state.json`, 'utf8'));
    const key = await importAesKey(new Uint8Array(Buffer.from(state.key, 'hex')));
    const withdrawalKey = new Uint8Array(readFileSync(new URL('withdrawal.pk', SETUP)));
    const wasm: WithdrawalWasm = {
      prepareWithdrawal: (c, s, a) => browser_prepare_withdrawal(c, s, a, withdrawalKey),
    };
    const contract = '0x' + toHex(codec.contractAddress(spec.vault));
    const destination = '0x' + toHex(codec.destination(spec.payout));
    const config = {
      chainId: spec.chainId as string,
      contractAddress: contract,
      operatorPk: spec.operatorPk,
      clearancePk: spec.clearancePk,
    };
    const flowSource = readFileSync(rel('../src/leave/flow.ts'), 'utf8');
    const gas = /export const GAS = '(\d+)'/.exec(flowSource)![1];

    const out: Record<string, unknown> = { gas };
    for (const kind of KINDS) {
      const aad = aadOf(kind);
      const n = spec.notes[kind] as { noteId: number; amount: string; expiry: number };
      let sealed = await confirmNote(noteWasm, key, aad, state.notes[kind], {
        config: { ...config, maxBound: spec.maxBound },
        ...n,
      });
      const now = spec.chainNow as number;
      let clearance: string | undefined;
      if (kind === 'close') {
        const own = await prepareWithdrawal(wasm, key, aad, sealed, {
          config,
          kind: 'ownership',
          now,
        });
        sealed = own.sealed;
        const nullifier = JSON.parse(planParts(own.planJson).nullifier) as string;
        clearance = op.test_operator_sign_clearance(2n, nullifier, BigInt(spec.chainId), contract);
      }
      const plan = await prepareWithdrawal(wasm, key, aad, sealed, {
        config,
        kind: kind === 'close' ? 'mutual' : 'escape',
        now,
        destination,
        ...(clearance ? { clearance } : {}),
      });
      const { publicInputs } = planParts(plan.planJson);
      const wordsHex = op.withdrawal_words(publicInputs);
      out[kind] = {
        args: callArgs(plan.planJson, wordsHex),
        publicInputs,
        wordsHex: JSON.parse(wordsHex),
      };
    }
    writeFileSync(`${dir}/calldata.json`, JSON.stringify(out));
  }, 300_000);
});
