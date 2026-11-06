// The lease path of the note core against the real zkapi wallet core (dev-setup build): a real
// request proof, an operator response signed with the test state key, S1 to S6 on install.
import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import {
  BrowserRequestProver,
  BrowserNoteTree,
  browser_complete_response,
  browser_confirm_deposit,
  browser_generate_deposit,
  browser_note_leaf,
  browser_setup_id,
  browser_tree_path_from_leaves,
  browser_verify_request,
  initSync,
} from '../src/wasm/zkapi-browser-dev/zkapi_browser.js';
import { importAesKey } from '../src/crypto/aead';
import { confirmNote, generateNote, type NoteWasm } from '../src/note/core';
import {
  completeLease,
  discardPending,
  leaseOpened,
  prepareLease,
  readNote,
  recordExchange,
  type LeaseConfig,
  type LeaseWasm,
} from '../src/note/lease';
import { splitObject } from '../src/note/rawjson';
import * as op from '../src/wasm/web-checks-test/nymira_web_wasm.js';

const SETUP = new URL('../../vendor/zkapi/protocol/setup/v2-concession-dev/', import.meta.url);
const CHAIN_ID = '7718972759422244814';
const CONTRACT = '0x' + '0a'.repeat(31) + '01';
const aad = 'nymira:v1:note:testnet:vault.test:n1';

let prover: BrowserRequestProver;
let key: CryptoKey;
let operatorPk: { x: string; y: string };
let config: LeaseConfig;

const wasm: LeaseWasm = {
  prepareRequest: (c, s, a) => prover.prepare_request(c, s, a),
  treePath: (snapshot, noteId) => BrowserNoteTree.from_state(snapshot).path(noteId),
  completeResponse: browser_complete_response,
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
  operatorPk = JSON.parse(op.test_operator_public_key(1n)) as { x: string; y: string };
  config = { chainId: CHAIN_ID, contractAddress: CONTRACT, operatorPk, clearancePk: operatorPk };
  prover = new BrowserRequestProver(readFileSync(new URL('request.pk', SETUP)));
  key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
}, 120_000);

async function fundedNote() {
  const { sealed: pending } = await generateNote(noteWasm, key, aad);
  const sealed = await confirmNote(noteWasm, key, aad, pending, {
    config: { ...config, operatorPk, clearancePk: operatorPk, maxBound: '500000' },
    noteId: 0,
    amount: '2000000',
    expiry: 4_000_000_000,
  });
  return sealed;
}

describe('lease path', () => {
  it('uses the dev setup here, and the deployed build does not', () => {
    expect(browser_setup_id()).toBe('dev');
    const deployed = readFileSync(
      new URL('../src/wasm/zkapi-browser/zkapi_browser_bg.wasm', import.meta.url),
    );
    // A second instance of the ceremony-setup build: it loads no development key.
    expect(
      deployed.equals(
        readFileSync(
          new URL('../src/wasm/zkapi-browser-dev/zkapi_browser_bg.wasm', import.meta.url),
        ),
      ),
    ).toBe(false);
  });

  it('proves a lease, installs the signed successor and keeps the record', async () => {
    const funded = await fundedNote();
    // The note's leaf sits at index 0 of a one-leaf tree.
    const before = await readNote(key, aad, funded);
    expect(before.balance).toBe('2000000');
    const stateText = (await import('../src/crypto/aead')).openJson;
    const payload = await stateText<{ state: string }>(key, funded, aad);
    const leaf = JSON.parse(browser_note_leaf(payload.state)) as string;
    const path = JSON.parse(browser_tree_path_from_leaves(JSON.stringify([leaf]), 0)) as {
      active_root: string;
    };
    const tree = { root: path.active_root, nextNoteId: 1, leaves: [leaf], block: 'blockhash' };

    const prepared = await prepareLease(wasm, key, aad, funded, {
      config,
      noteId: 0,
      bound: '100000',
      tree,
      requestTime: 1_800_000_000,
      clientRequestId: '11111111-1111-4111-8111-111111111111',
      createdAtMs: 1_800_000_000_000,
      openedAt: 1_800_000_000,
    });

    // The request is the wallet core's own text: the chain id keeps every digit.
    expect(prepared.requestJson).toContain(`"chain_id":${CHAIN_ID}`);
    const body = splitObject(prepared.requestJson);
    expect(body.payload).toBe('"{\\"mode\\":\\"openrouter_ephemeral_lease\\",\\"version\\":1}"');
    // The proof verifies under the dev verifying key.
    expect(
      browser_verify_request(
        body.public_inputs!,
        JSON.parse(body.proof!).proof as string,
        readFileSync(new URL('request.vk', SETUP)),
      ),
    ).toBe(true);

    // The bound is on the state that came back, before anything is sent.
    const afterPrepare = await readNote(key, aad, prepared.sealed);
    expect(afterPrepare.boundAtState).toBe('100000');
    expect(afterPrepare.pending).toBe(true);

    let sealed = await leaseOpened(key, aad, prepared.sealed, {
      leaseId: 'lease-1',
      token: 'token-1',
      issuedAt: 1_800_000_001,
      expiresAt: 1_800_000_900,
    });
    sealed = await recordExchange(key, aad, sealed, {
      model: 'z-ai/glm-5.3-flash',
      sampling: { stream: false, temperature: 0.7 },
      messageCount: 1,
      messageBytes: 12,
      promptTokens: 14,
      completionTokens: 5,
    });
    expect((await readNote(key, aad, sealed)).active?.token).toBe('token-1');

    const response = op.test_operator_respond(
      1n,
      prepared.requestJson,
      BigInt(CHAIN_ID),
      CONTRACT,
      2_000n,
    );
    const done = await completeLease(wasm, key, aad, sealed, config, {
      responseJson: response,
      closedAt: 1_800_000_100,
      expectedCharge: '2000',
      trust: 'model_signed',
      warnings: [],
    });
    expect(done.charge).toBe('2000');
    const after = await readNote(key, aad, done.sealed);
    expect(after.balance).toBe('1998000');
    expect(after.pending).toBe(false);
    expect(after.active).toBeUndefined();
    expect(after.leases).toHaveLength(1);
    expect(after.leases[0]).toMatchObject({
      leaseId: 'lease-1',
      root: tree.root,
      leafCount: 1,
      snapshotBlock: 'blockhash',
      boundClass: '100000',
      model: 'z-ai/glm-5.3-flash',
      promptTokens: 14,
      charge: '2000',
      trust: 'model_signed',
    });
    // The request and its nullifier stay for the privacy panel's reuse check.
    expect(after.last).toMatchObject({
      leaseId: 'lease-1',
      requestTime: 1_800_000_000,
      json: prepared.requestJson,
    });
    expect(after.last?.nullifier).toMatch(/^0x[0-9a-f]{64}$/);
    // Metadata only: nothing of a message is in the sealed record's lease.
    expect(JSON.stringify(after.leases)).not.toMatch(/content/);
  }, 120_000);

  it('refuses a response signed by another key (S6) and keeps the journal', async () => {
    const funded = await fundedNote();
    const payload = await (
      await import('../src/crypto/aead')
    ).openJson<{ state: string }>(key, funded, aad);
    const leaf = JSON.parse(browser_note_leaf(payload.state)) as string;
    const path = JSON.parse(browser_tree_path_from_leaves(JSON.stringify([leaf]), 0)) as {
      active_root: string;
    };
    const prepared = await prepareLease(wasm, key, aad, funded, {
      config,
      noteId: 0,
      bound: '100000',
      tree: { root: path.active_root, nextNoteId: 1, leaves: [leaf], block: 'b' },
      requestTime: 1_800_000_000,
      clientRequestId: '22222222-2222-4222-8222-222222222222',
      createdAtMs: 1,
      openedAt: 1,
    });
    const forged = op.test_operator_respond(
      2n,
      prepared.requestJson,
      BigInt(CHAIN_ID),
      CONTRACT,
      2_000n,
    );
    await expect(
      completeLease(wasm, key, aad, prepared.sealed, config, {
        responseJson: forged,
        closedAt: 2,
        expectedCharge: '2000',
        trust: 'model_signed',
        warnings: [],
      }),
    ).rejects.toThrow(/successor_signature_invalid/);
    // A refused request can be set aside (`root_stale`), and the same state proves again.
    const cleared = await discardPending(key, aad, prepared.sealed);
    expect((await readNote(key, aad, cleared)).pending).toBe(false);
    expect((await readNote(key, aad, cleared)).boundAtState).toBe('100000');
  }, 120_000);
});
