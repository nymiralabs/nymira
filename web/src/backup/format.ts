// The backup file. Plain JSON text with an Argon2id parameter header and an AES-256-GCM body:
//
//   {"format":"nymira-backup","version":1,
//    "kdf":{"alg":"argon2id","m":65536,"t":3,"p":1,"salt":"<base64>"},
//    "cipher":"aes-256-gcm","iv":"<base64>","ciphertext":"<base64>"}
//
// The key is Argon2id(passphrase, salt, m, t, p) with the same parameters as the secret wrapping.
// The plaintext is a BackupPlain. No page-side code reads the plaintext: this module runs in the
// note worker (and in tests, which decrypt a downloaded file to look for leaks).
import { b64decode, b64encode } from '../crypto/bytes';
import { open, seal, importAesKey } from '../crypto/aead';
import { ARGON2, SALT_BYTES, deriveKeyBytes, type Argon2Params } from '../crypto/kdf';
import type { LeaveInfo, NotePhase, NoteRecord, SealedKind } from '../store/types';

export const BACKUP_FORMAT = 'nymira-backup';
export const BACKUP_AAD = 'nymira:v1:backup';
export const BACKUP_EXTENSION = '.nymira-backup';

export interface BackupNote {
  id: string;
  network: string;
  vault: string;
  token: string;
  denomination: string;
  payout: string;
  payoutExplicit: boolean;
  sender: string;
  commitment: string;
  phase: NotePhase;
  txHash?: string;
  noteId?: number;
  expiry?: number;
  createdAt: number;
  updatedAt: number;
  /** Where the note stands on its way out, and what the expiry guard did (public values). */
  leave?: LeaveInfo;
  guard?: NoteRecord['guard'];
  kind: SealedKind;
  /** The note secret (`kind: pending`, `0x` plus 64 hex digits). */
  secret?: string;
  /** The wallet state (`kind: state`): zkapi's BrowserNoteState as JSON text. */
  state?: string;
  /** The lease in progress and the finished lease records (`kind: state`), as JSON text. */
  extra?: string;
}

export interface BackupPlain {
  format: typeof BACKUP_FORMAT;
  version: 1;
  network: string;
  vault: string;
  createdAt: number;
  notes: BackupNote[];
}

interface Envelope {
  format: string;
  version: number;
  kdf: Argon2Params & { salt: string };
  cipher: 'aes-256-gcm';
  iv: string;
  ciphertext: string;
}

export async function encryptBackup(plain: BackupPlain, passphrase: string): Promise<Uint8Array> {
  const salt = crypto.getRandomValues(new Uint8Array(SALT_BYTES));
  const key = await importAesKey(await deriveKeyBytes(passphrase, salt, ARGON2));
  const sealed = await seal(key, new TextEncoder().encode(JSON.stringify(plain)), BACKUP_AAD);
  const envelope: Envelope = {
    format: BACKUP_FORMAT,
    version: 1,
    kdf: { ...ARGON2, salt: b64encode(salt) },
    cipher: 'aes-256-gcm',
    iv: sealed.iv,
    ciphertext: sealed.ct,
  };
  return new TextEncoder().encode(JSON.stringify(envelope));
}

export class BackupError extends Error {}

export async function decryptBackup(bytes: Uint8Array, passphrase: string): Promise<BackupPlain> {
  let envelope: Envelope;
  try {
    envelope = JSON.parse(new TextDecoder().decode(bytes)) as Envelope;
  } catch {
    throw new BackupError('this file is not a Nymira backup');
  }
  if (envelope.format !== BACKUP_FORMAT || envelope.version !== 1) {
    throw new BackupError('this file is not a Nymira backup, or comes from a newer version');
  }
  const { kdf } = envelope;
  // Refuse parameters far above ours (a crafted file must not exhaust memory) or below.
  if (
    kdf?.alg !== 'argon2id' ||
    kdf.m < ARGON2.m ||
    kdf.m > ARGON2.m * 4 ||
    kdf.t < ARGON2.t ||
    kdf.t > ARGON2.t * 4 ||
    kdf.p !== ARGON2.p
  ) {
    throw new BackupError('this backup uses key derivation settings the app does not accept');
  }
  const key = await importAesKey(await deriveKeyBytes(passphrase, b64decode(kdf.salt), kdf));
  let plain: Uint8Array;
  try {
    plain = await open(key, { iv: envelope.iv, ct: envelope.ciphertext }, BACKUP_AAD);
  } catch {
    throw new BackupError('wrong passphrase, or the file is damaged');
  }
  const parsed = JSON.parse(new TextDecoder().decode(plain)) as BackupPlain;
  if (parsed.format !== BACKUP_FORMAT || parsed.version !== 1 || !Array.isArray(parsed.notes)) {
    throw new BackupError('the backup content is not in the expected format');
  }
  return parsed;
}

export function backupFileName(now: Date = new Date()): string {
  const stamp = now.toISOString().slice(0, 10);
  return `nymira-backup-${stamp}${BACKUP_EXTENSION}`;
}
