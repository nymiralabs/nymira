// Everything that touches a plain note secret. It runs in the note worker (src/worker) and, with a
// Node-loaded WASM, in the unit tests. The page's main thread never sees a plain secret: it passes
// a non-extractable key in and gets sealed bytes and public values out.
import { openJson, sealJson, type Sealed } from '../crypto/aead';
import {
  BACKUP_FORMAT,
  decryptBackup,
  encryptBackup,
  type BackupNote,
  type BackupPlain,
} from '../backup/format';
import { noteAad, type NoteRecord } from '../store/types';

/** The two zkapi-browser calls the funding path needs. */
export interface NoteWasm {
  /** `{secret, registration_commitment}` as JSON text. */
  generateDeposit(): string;
  confirmDeposit(configJson: string, argsJson: string): string;
}

export interface PendingPayload {
  secret: string;
}

const HEX32 = /^0x[0-9a-f]{64}$/;

/**
 * The wallet core spells a field element without its leading zeros (`0x0abc` is `0xabc`); the
 * vault wants exactly 64 hex digits. One in sixteen commitments starts with a zero nibble.
 */
export function pad32(hex: string): string {
  const digits = hex.toLowerCase().replace(/^0x/, '');
  return '0x' + digits.padStart(64, '0');
}

export async function generateNote(
  wasm: NoteWasm,
  key: CryptoKey,
  aad: string,
): Promise<{ commitment: string; sealed: Sealed }> {
  const made = JSON.parse(wasm.generateDeposit()) as {
    secret: string;
    registration_commitment: string;
  };
  const commitment = pad32(made.registration_commitment);
  const secret = pad32(made.secret);
  if (!HEX32.test(commitment) || !HEX32.test(secret)) {
    throw new Error('the note library returned an unexpected format');
  }
  const sealed = await sealJson(key, { secret } satisfies PendingPayload, aad);
  return { commitment, sealed };
}

export interface ConfirmInput {
  /** The vault's `config()` fields the wallet core wants, plus the mapped contract address. */
  config: {
    chainId: string;
    contractAddress: string;
    operatorPk: { x: string; y: string };
    clearancePk: { x: string; y: string };
    maxBound: string;
  };
  noteId: number;
  amount: string;
  expiry: number;
}

/** Opens the pending secret, builds the genesis state and seals it. */
export async function confirmNote(
  wasm: NoteWasm,
  key: CryptoKey,
  aad: string,
  pending: Sealed,
  input: ConfirmInput,
): Promise<Sealed> {
  const { secret } = await openJson<PendingPayload>(key, pending, aad);
  // u64 and u128 values are written as bare digits: JSON.stringify would round them.
  const { config: c } = input;
  const configJson =
    `{"protocol_version":2,"chain_id":${c.chainId},"contract_address":${JSON.stringify(c.contractAddress)},` +
    `"request_charge_cap":${c.maxBound},"policy_charge_cap":${c.maxBound},"policy_enabled":false,` +
    `"state_signing_key":${JSON.stringify(c.operatorPk)},"clearance_signing_key":${JSON.stringify(c.clearancePk)}}`;
  const argsJson = `{"secret":${JSON.stringify(secret)},"note_id":${input.noteId},"amount":${input.amount},"expiry_ts":${input.expiry}}`;
  // Kept as the wallet core's own JSON text: parsing it would round the u64 chain id.
  const state = wasm.confirmDeposit(configJson, argsJson);
  return sealJson(key, { state }, aad);
}

function toBackupNote(
  record: NoteRecord,
  plain: { secret?: string; state?: string; extra?: string },
): BackupNote {
  const { sealed: _sealed, ...rest } = record;
  void _sealed;
  const note: BackupNote = { ...rest };
  if (record.kind === 'pending') note.secret = plain.secret;
  else {
    note.state = plain.state;
    if (plain.extra !== undefined) note.extra = plain.extra;
  }
  return note;
}

async function unsealRecord(key: CryptoKey, record: NoteRecord): Promise<BackupNote> {
  const aad = noteAad(record.network, record.vault, record.id);
  const plain = await openJson<{ secret?: string; state?: string; extra?: string }>(
    key,
    record.sealed,
    aad,
  );
  return toBackupNote(record, plain);
}

/** The encrypted backup file for these records (unsealed here, sealed again under the passphrase). */
export async function makeBackup(
  key: CryptoKey,
  records: NoteRecord[],
  passphrase: string,
  now: number,
  scope: { network: string; vault: string },
): Promise<Uint8Array> {
  const notes = await Promise.all(records.map((r) => unsealRecord(key, r)));
  const plain: BackupPlain = {
    format: BACKUP_FORMAT,
    version: 1,
    network: scope.network,
    vault: scope.vault,
    createdAt: now,
    notes,
  };
  return encryptBackup(plain, passphrase);
}

export interface BackupCheck {
  /** Notes in the file. */
  notes: number;
  /** True when the file holds the record, with the same secret or state as the sealed copy. */
  matches: boolean;
}

/** Decrypts a chosen file and checks it holds this record's secret. */
export async function testBackup(
  key: CryptoKey,
  bytes: Uint8Array,
  passphrase: string,
  record: NoteRecord,
): Promise<BackupCheck> {
  const plain = await decryptBackup(bytes, passphrase);
  const mine = await unsealRecord(key, record);
  const found = plain.notes.find((n) => n.commitment === record.commitment);
  const matches =
    found !== undefined &&
    found.kind === mine.kind &&
    found.secret === mine.secret &&
    found.state === mine.state &&
    found.extra === mine.extra;
  return { notes: plain.notes.length, matches };
}

/** The records of a backup, sealed again under `key` so they can go into IndexedDB. */
export async function restoreBackup(
  key: CryptoKey,
  bytes: Uint8Array,
  passphrase: string,
  scope: { network: string; vault: string },
  seal: (key: CryptoKey, value: unknown, aad: string) => Promise<Sealed> = sealJson,
): Promise<{ records: NoteRecord[]; foreign: number }> {
  const plain = await decryptBackup(bytes, passphrase);
  const records: NoteRecord[] = [];
  let foreign = 0;
  for (const note of plain.notes) {
    if (note.network !== scope.network || note.vault !== scope.vault) {
      foreign += 1;
      continue;
    }
    const { secret, state, extra, ...meta } = note;
    const payload =
      note.kind === 'pending' ? { secret } : extra === undefined ? { state } : { state, extra };
    records.push({
      ...meta,
      sealed: await seal(key, payload, noteAad(note.network, note.vault, note.id)),
    });
  }
  return { records, foreign };
}
