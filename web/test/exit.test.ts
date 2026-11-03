// The exit proofs of the note core against the real zkapi wallet core (dev-setup build): an
// ownership proof the operator would verify, a clearance signed with the test clearance key, a
// mutual close and an escape verified the way the vault verifies them (from the 16 statement
// words), and the lock rules that keep a note that is leaving from making another request.
import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import {
  BrowserRequestProver,
  BrowserNoteTree,
  browser_complete_response,
  browser_confirm_deposit,
  browser_generate_deposit,
  browser_note_leaf,
  browser_prepare_withdrawal,
  browser_tree_path_from_leaves,
  initSync,
} from '../src/wasm/zkapi-browser-dev/zkapi_browser.js';
import { importAesKey, openJson } from '../src/crypto/aead';
import { confirmNote, generateNote, type NoteWasm } from '../src/note/core';
import {
  EXIT_PROOF_SECS,
  OWNERSHIP_PROOF_SECS,
  callArgs,
  clearanceBody,
  planParts,
  prepareWithdrawal,
  type WithdrawalWasm,
} from '../src/note/exit';
import {
  leaseOpened,
  prepareLease,
  readNote,
  type LeaseConfig,
  type LeaseWasm,
} from '../src/note/lease';
import { splitObject } from '../src/note/rawjson';
import * as op from '../src/wasm/web-checks-test/nymira_web_wasm.js';

const SETUP = new URL('../../vendor/zkapi/protocol/setup/v2-concession-dev/', import.meta.url);
const CHAIN_ID = '7718972759422244814';
const CONTRACT = '0x' + '0a'.repeat(31) + '01';
const DESTINATION = '0x' + '00'.repeat(19) + '09';
const aad = 'nymira:v1:note:testnet:vault.test:n1';
const NOW = 1_800_000_000;

let prover: BrowserRequestProver;
let withdrawalKey: Uint8Array;
let withdrawalVk: Uint8Array;
let key: CryptoKey;
let config: LeaseConfig;

const leaseWasm: LeaseWasm = {
  prepareRequest: (c, s, a) => prover.prepare_request(c, s, a),
  treePath: (snapshot, noteId) => BrowserNoteTree.from_state(snapshot).path(noteId),
  completeResponse: browser_complete_response,
};
const exitWasm: WithdrawalWasm = {
  prepareWithdrawal: (c, s, a) => browser_prepare_withdrawal(c, s, a, withdrawalKey),
};
const noteWasm: NoteWasm = {
  generateDeposit: browser_generate_deposit,
  confirmDeposit: browser_confirm_deposit,
};

beforeAll(async () => {
  initSync({
    module: readFileSync(
      new URL('../src/wasm/zkapi-browser-dev/zkapi_browser_bg.wasm', import.meta.url),
    ),
  });
  op.initSync({
    module: readFileSync(
      new URL('../src/wasm/web-checks-test/nymira_web_wasm_bg.wasm', import.meta.url),
    ),
  });
  const operatorPk = JSON.parse(op.test_operator_public_key(1n)) as { x: string; y: string };
  const clearancePk = JSON.parse(op.test_operator_public_key(2n)) as { x: string; y: string };
  config = { chainId: CHAIN_ID, contractAddress: CONTRACT, operatorPk, clearancePk };
  prover = new BrowserRequestProver(readFileSync(new URL('request.pk', SETUP)));
  withdrawalKey = new Uint8Array(readFileSync(new URL('withdrawal.pk', SETUP)));
  withdrawalVk = new Uint8Array(readFileSync(new URL('withdrawal.vk', SETUP)));
  key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
}, 120_000);

async function fundedNote() {
  const { sealed: pending } = await generateNote(noteWasm, key, aad);
  return confirmNote(noteWasm, key, aad, pending, {
    config: { ...config, maxBound: '500000' },
    noteId: 0,
    amount: '2000000',
    expiry: 4_000_000_000,
  });
}

const inputs = (kind: 'ownership' | 'mutual' | 'escape', now = NOW, clearance?: string) => ({
  config,
  kind,
  now,
  ...(kind === 'ownership' ? {} : { destination: DESTINATION }),
  ...(clearance ? { clearance } : {}),
});

const statement = (planJson: string) =>
  JSON.parse(planParts(planJson).publicInputs) as {
    kind: number;
    final_balance: number | string;
    conceded: number | string;
    valid_until: number;
    destination: number[];
  };

/** Proves a lease request on `sealed` (not sent), so the state records a bound. */
async function withRequestLeft(sealed: Awaited<ReturnType<typeof fundedNote>>, bound = '100000') {
  const { state } = await openJson<{ state: string }>(key, sealed, aad);
  const leaf = JSON.parse(browser_note_leaf(state)) as string;
  const path = JSON.parse(browser_tree_path_from_leaves(JSON.stringify([leaf]), 0)) as {
    active_root: string;
  };
  const tree = { root: path.active_root, nextNoteId: 1, leaves: [leaf], block: 'b' };
  return prepareLease(leaseWasm, key, aad, sealed, {
    config,
    noteId: 0,
    bound,
    tree,
    requestTime: NOW,
    clientRequestId: '11111111-1111-4111-8111-111111111111',
    createdAtMs: NOW * 1000,
    openedAt: NOW,
  });
}

describe('exit proofs', () => {
  it('closes: an ownership proof, a clearance, a mutual close the vault would verify', async () => {
    const funded = await fundedNote();
    const own = await prepareWithdrawal(exitWasm, key, aad, funded, inputs('ownership'));
    expect(own.reused).toBe(false);
    expect(own.validUntil).toBe(NOW + OWNERSHIP_PROOF_SECS);
    const ownership = statement(own.planJson);
    expect(ownership).toMatchObject({ kind: 2, conceded: 0, valid_until: NOW + 3300 });
    // The operator verifies it from the public inputs and the proof.
    const parts = planParts(own.planJson);
    expect(op.test_operator_verify_withdrawal(withdrawalVk, parts.publicInputs, parts.proof)).toBe(
      true,
    );
    // The clearance request is exactly what the operator parses.
    const body = splitObject(clearanceBody(own.planJson));
    expect(Object.keys(body).sort()).toEqual(['ownership_proof', 'withdrawal_nullifier']);
    expect(Object.keys(splitObject(body.ownership_proof!)).sort()).toEqual([
      'proof',
      'public_inputs',
    ]);
    expect(body.withdrawal_nullifier).toBe(parts.nullifier);

    // The operator signs the clearance; the wallet core checks it against the pinned key.
    const nullifier = JSON.parse(parts.nullifier) as string;
    const clearance = op.test_operator_sign_clearance(2n, nullifier, BigInt(CHAIN_ID), CONTRACT);
    const mutual = await prepareWithdrawal(
      exitWasm,
      key,
      aad,
      own.sealed,
      inputs('mutual', NOW + 100, clearance),
    );
    expect(mutual.validUntil).toBe(NOW + 100 + EXIT_PROOF_SECS);
    const close = statement(mutual.planJson);
    expect(close).toMatchObject({ kind: 1, final_balance: 2_000_000, conceded: 0 });
    expect(close.valid_until).toBe(NOW + 100 + 10_800);
    expect(close.destination.at(-1)).toBe(9);
    // What goes to `mutual_close`: base64 words, verified from those words as the vault does.
    const words = op.withdrawal_words(parts.publicInputs);
    expect(JSON.parse(words)).toHaveLength(16);
    const closeWords = op.withdrawal_words(planParts(mutual.planJson).publicInputs);
    const args = callArgs(mutual.planJson, closeWords);
    expect(args.inputs).toHaveLength(16);
    for (const word of args.inputs) expect(atob(word).length).toBe(32);
    expect(
      op.test_operator_verify_withdrawal_words(
        withdrawalVk,
        closeWords,
        planParts(mutual.planJson).proof,
      ),
    ).toBe(true);
    // A changed word fails: the order and content of the words are what the proof binds.
    const bad = JSON.parse(closeWords) as string[];
    [bad[9], bad[14]] = [bad[14]!, bad[9]!];
    expect(
      op.test_operator_verify_withdrawal_words(
        withdrawalVk,
        JSON.stringify(bad),
        planParts(mutual.planJson).proof,
      ),
    ).toBe(false);
  }, 180_000);

  it('rejects a clearance that the pinned key did not sign', async () => {
    const funded = await fundedNote();
    const own = await prepareWithdrawal(exitWasm, key, aad, funded, inputs('ownership'));
    const nullifier = JSON.parse(planParts(own.planJson).nullifier) as string;
    const forged = op.test_operator_sign_clearance(1n, nullifier, BigInt(CHAIN_ID), CONTRACT);
    await expect(
      prepareWithdrawal(exitWasm, key, aad, own.sealed, inputs('mutual', NOW, forged)),
    ).rejects.toThrow(/clearance/i);
  }, 120_000);

  it('resends the kept ownership proof instead of making a second one', async () => {
    const funded = await fundedNote();
    const first = await prepareWithdrawal(exitWasm, key, aad, funded, inputs('ownership'));
    const again = await prepareWithdrawal(
      exitWasm,
      key,
      aad,
      first.sealed,
      inputs('ownership', NOW + 60),
    );
    expect(again.reused).toBe(true);
    expect(again.planJson).toBe(first.planJson);
    // In the last seconds it is neither resent nor replaced.
    await expect(
      prepareWithdrawal(exitWasm, key, aad, again.sealed, inputs('ownership', NOW + 3300 - 2)),
    ).rejects.toThrow(/still valid/);
    // After it ended, a new one is made.
    const fresh = await prepareWithdrawal(
      exitWasm,
      key,
      aad,
      again.sealed,
      inputs('ownership', NOW + 3301),
    );
    expect(fresh.reused).toBe(false);
    const view = await readNote(key, aad, fresh.sealed);
    expect(view.exit?.released.filter((p) => p.kind === 2)).toHaveLength(2);
  }, 180_000);

  it('exits with concession 0 when no request left the state', async () => {
    const funded = await fundedNote();
    const exit = await prepareWithdrawal(exitWasm, key, aad, funded, inputs('escape'));
    expect(exit.conceded).toBe('0');
    const s = statement(exit.planJson);
    expect(s).toMatchObject({ kind: 0, final_balance: 2_000_000, conceded: 0 });
    const words = op.withdrawal_words(planParts(exit.planJson).publicInputs);
    expect(
      op.test_operator_verify_withdrawal_words(withdrawalVk, words, planParts(exit.planJson).proof),
    ).toBe(true);
    // The lock and the released proof are in the sealed payload before the proof is returned.
    const view = await readNote(key, aad, exit.sealed);
    expect(view.exit).toMatchObject({
      mode: 'escape',
      released: [{ kind: 0, conceded: '0', validUntil: NOW + EXIT_PROOF_SECS }],
    });
  }, 120_000);

  it('exits conceding the stored bound when a request left the state, and starts no lease after', async () => {
    const funded = await fundedNote();
    const prepared = await withRequestLeft(funded, '100000');
    // A request is pending (proven, response not installed): an exit still works.
    const exit = await prepareWithdrawal(exitWasm, key, aad, prepared.sealed, inputs('escape'));
    expect(exit.conceded).toBe('100000');
    expect(statement(exit.planJson)).toMatchObject({
      kind: 0,
      conceded: 100000,
      final_balance: 1_900_000,
    });
    // After the exit proof left, the note makes no request proof any more.
    await expect(withRequestLeft(exit.sealed)).rejects.toThrow(/leaving/);
    // A mutual close needs the pending request settled first.
    await expect(
      prepareWithdrawal(exitWasm, key, aad, prepared.sealed, inputs('ownership')),
    ).rejects.toThrow(/not settled/);
    // A second exit proof is a retry of the same escape: allowed, concedes the same.
    const retry = await prepareWithdrawal(
      exitWasm,
      key,
      aad,
      exit.sealed,
      inputs('escape', NOW + 5),
    );
    expect(retry.conceded).toBe('100000');
  }, 180_000);

  it('keeps the state free of requests once an ownership proof is out, and refuses mutual after an escape', async () => {
    const funded = await fundedNote();
    const own = await prepareWithdrawal(exitWasm, key, aad, funded, inputs('ownership'));
    await expect(withRequestLeft(own.sealed)).rejects.toThrow(/leaving/);
    // The operator did not cooperate: the lock switches from mutual to escape.
    const exit = await prepareWithdrawal(exitWasm, key, aad, own.sealed, inputs('escape'));
    expect((await readNote(key, aad, exit.sealed)).exit?.mode).toBe('escape');
    // And never back once an escape proof was released.
    await expect(
      prepareWithdrawal(exitWasm, key, aad, exit.sealed, inputs('ownership', NOW + 4000)),
    ).rejects.toThrow(/only leave by that exit/);
  }, 180_000);

  it('refuses while a lease is open on the note', async () => {
    const funded = await fundedNote();
    const prepared = await withRequestLeft(funded);
    const opened = await leaseOpened(key, aad, prepared.sealed, {
      leaseId: 'lease-1',
      token: 'token-1',
      issuedAt: NOW,
      expiresAt: NOW + 3600,
    });
    await expect(prepareWithdrawal(exitWasm, key, aad, opened, inputs('escape'))).rejects.toThrow(
      /lease is open/,
    );
  }, 120_000);
});
