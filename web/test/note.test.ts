// The note core against the real zkapi-browser WASM (built by `npm run build:wasm`).
import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import {
  browser_confirm_deposit,
  browser_generate_deposit,
  initSync,
} from '../src/wasm/zkapi-browser/zkapi_browser.js';
import { importAesKey, openJson } from '../src/crypto/aead';
import {
  confirmNote,
  generateNote,
  makeBackup,
  pad32,
  restoreBackup,
  testBackup,
  type NoteWasm,
} from '../src/note/core';
import { noteAad, type NoteRecord } from '../src/store/types';

const wasm: NoteWasm = {
  generateDeposit: browser_generate_deposit,
  confirmDeposit: browser_confirm_deposit,
};

const config = {
  chainId: '7718972759422244814',
  contractAddress: '0x' + '0a'.repeat(31) + '01',
  operatorPk: { x: '0x' + '01'.repeat(32), y: '0x' + '02'.repeat(32) },
  clearancePk: { x: '0x' + '03'.repeat(32), y: '0x' + '04'.repeat(32) },
  maxBound: '500000',
};

beforeAll(() => {
  initSync({
    module: readFileSync(
      new URL('../src/wasm/zkapi-browser/zkapi_browser_bg.wasm', import.meta.url),
    ),
  });
});

async function newRecord(
  key: CryptoKey,
  id = 'note-1',
): Promise<{ record: NoteRecord; aad: string }> {
  const aad = noteAad('testnet', 'vault.test', id);
  const { commitment, sealed } = await generateNote(wasm, key, aad);
  const record: NoteRecord = {
    id,
    network: 'testnet',
    vault: 'vault.test',
    token: 'usdc.test',
    denomination: '1000000',
    payout: 'alice.test',
    payoutExplicit: false,
    sender: 'alice.test',
    commitment,
    phase: 'backed_up',
    createdAt: 1,
    updatedAt: 1,
    kind: 'pending',
    sealed,
  };
  return { record, aad };
}

describe('note core with the real wallet core', () => {
  it('makes a secret and a commitment in the vault’s spelling, and seals the secret', async () => {
    const key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    const { record, aad } = await newRecord(key);
    expect(record.commitment).toMatch(/^0x[0-9a-f]{64}$/);
    const { secret } = await openJson<{ secret: string }>(key, record.sealed, aad);
    expect(secret).toMatch(/^0x[0-9a-f]{64}$/);
    expect(JSON.stringify(record)).not.toContain(secret.slice(2));
    // Two notes never share a secret.
    const second = await newRecord(key, 'note-2');
    expect(second.record.commitment).not.toBe(record.commitment);
  });

  it('pads a field element to the 64 digits the vault wants', () => {
    expect(pad32('0xabc')).toBe('0x' + '0'.repeat(61) + 'abc');
    expect(pad32('0x' + 'F'.repeat(64))).toBe('0x' + 'f'.repeat(64));
  });

  it('every generated commitment has 64 digits, also when the field element has leading zeros', async () => {
    const key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    for (let i = 0; i < 64; i++) {
      const { commitment } = await generateNote(wasm, key, 'aad');
      expect(commitment).toMatch(/^0x[0-9a-f]{64}$/);
    }
  });

  it('builds the genesis state with the exact u64 chain id', async () => {
    const key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    const { record, aad } = await newRecord(key);
    const sealedState = await confirmNote(wasm, key, aad, record.sealed, {
      config,
      noteId: 7,
      amount: '1000000',
      expiry: 1_795_000_000,
    });
    const { state } = await openJson<{ state: string }>(key, sealedState, aad);
    // The state is kept as the wallet core's text: parsing it would round the chain id.
    expect(state).toContain('7718972759422244814');
    const parsed = JSON.parse(state);
    expect(parsed).toMatchObject({
      note_id: 7,
      deposit_amount: 1000000,
      expiry_ts: 1_795_000_000,
      is_genesis: true,
    });
    const { secret } = await openJson<{ secret: string }>(key, record.sealed, aad);
    expect(pad32(parsed.secret_s)).toBe(secret);
  });

  it('backs up, tests and restores a note under a new key', async () => {
    const key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    const { record } = await newRecord(key);
    const pass = 'a long enough passphrase';
    const scope = { network: 'testnet', vault: 'vault.test' };
    const file = await makeBackup(key, [record], pass, 5, scope);

    expect(await testBackup(key, file, pass, record)).toEqual({ notes: 1, matches: true });
    // A file of another note does not match.
    const other = await newRecord(key, 'note-2');
    expect((await testBackup(key, file, pass, other.record)).matches).toBe(false);

    // A fresh browser: another key, same file.
    const fresh = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    const restored = await restoreBackup(fresh, file, pass, scope);
    expect(restored.records).toHaveLength(1);
    const back = restored.records[0] as NoteRecord;
    expect(back.commitment).toBe(record.commitment);
    const original = await openJson<{ secret: string }>(
      key,
      record.sealed,
      noteAad('testnet', 'vault.test', 'note-1'),
    );
    expect(await openJson(fresh, back.sealed, noteAad('testnet', 'vault.test', 'note-1'))).toEqual(
      original,
    );

    // Another vault’s notes are skipped, not mixed in.
    const foreign = await restoreBackup(fresh, file, pass, {
      network: 'testnet',
      vault: 'other.test',
    });
    expect(foreign).toMatchObject({ records: [], foreign: 1 });
  });
});
