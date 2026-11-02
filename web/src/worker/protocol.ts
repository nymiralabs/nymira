import type { Sealed } from '../crypto/aead';
import type { Argon2Params } from '../crypto/kdf';
import type { BackupCheck, ConfirmInput } from '../note/core';
import type { NoteRecord } from '../store/types';

/** What the page sends to build a genesis state. The worker maps `vault` to the contract address. */
export type ConfirmRequestInput = Omit<ConfirmInput, 'config'> & {
  vault: string;
  config: Omit<ConfirmInput['config'], 'contractAddress'>;
};

type Scope = { network: string; vault: string };

/** Requests to the note worker. `key` is always a non-extractable AES-GCM CryptoKey. */
export type WorkerRequest =
  | { op: 'deriveKey'; passphrase: string; salt: string; params: Argon2Params }
  | { op: 'generate'; key: CryptoKey; aad: string }
  | { op: 'confirm'; key: CryptoKey; aad: string; pending: Sealed; input: ConfirmRequestInput }
  | {
      op: 'makeBackup';
      key: CryptoKey;
      records: NoteRecord[];
      passphrase: string;
      now: number;
      scope: Scope;
    }
  | { op: 'testBackup'; key: CryptoKey; bytes: Uint8Array; passphrase: string; record: NoteRecord }
  | { op: 'restoreBackup'; key: CryptoKey; bytes: Uint8Array; passphrase: string; scope: Scope };

export interface ResultOf {
  deriveKey: CryptoKey;
  generate: { commitment: string; sealed: Sealed };
  confirm: Sealed;
  makeBackup: Uint8Array;
  testBackup: BackupCheck;
  restoreBackup: { records: NoteRecord[]; foreign: number };
}

export type WorkerResponse =
  { id: number; ok: true; result: unknown } | { id: number; ok: false; error: string };
