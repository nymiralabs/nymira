import type { Sealed } from '../crypto/aead';
import type { Argon2Params } from '../crypto/kdf';
import type { BackupCheck, ConfirmInput } from '../note/core';
import type {
  Completed,
  ExchangeUse,
  LeaseConfig,
  NoteView,
  PrepareInput,
  PreparedLease,
  Settlement,
} from '../note/lease';
import type { PreparedWithdrawal, WithdrawalInput } from '../note/exit';
import type { NoteRecord } from '../store/types';

/** What the page sends to build a genesis state. The worker maps `vault` to the contract address. */
export type ConfirmRequestInput = Omit<ConfirmInput, 'config'> & {
  vault: string;
  config: Omit<ConfirmInput['config'], 'contractAddress'>;
};

type Scope = { network: string; vault: string };

/** A lease config as the page knows it: the worker maps `vault` to the contract address. */
export type LeaseConfigInput = Omit<LeaseConfig, 'contractAddress'> & { vault: string };

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
  | { op: 'restoreBackup'; key: CryptoKey; bytes: Uint8Array; passphrase: string; scope: Scope }
  | { op: 'loadProver'; url: string; sha256: string }
  | {
      op: 'prepareLease';
      key: CryptoKey;
      aad: string;
      sealed: Sealed;
      input: Omit<PrepareInput, 'config'> & { config: LeaseConfigInput };
    }
  | { op: 'loadWithdrawalProver'; url: string; sha256: string }
  | {
      op: 'prepareWithdrawal';
      key: CryptoKey;
      aad: string;
      sealed: Sealed;
      /** `payout` is the payout account id; the worker maps it to the 20-byte destination. */
      input: Omit<WithdrawalInput, 'config' | 'destination'> & {
        config: LeaseConfigInput;
        payout?: string;
      };
    }
  | { op: 'solvePuzzle'; nonce: string; difficulty: number }
  | {
      op: 'leaseOpened';
      key: CryptoKey;
      aad: string;
      sealed: Sealed;
      lease: { leaseId: string; token: string; issuedAt: number; expiresAt: number };
    }
  | { op: 'recordExchange'; key: CryptoKey; aad: string; sealed: Sealed; use: ExchangeUse }
  | {
      op: 'completeLease';
      key: CryptoKey;
      aad: string;
      sealed: Sealed;
      config: LeaseConfigInput;
      settlement: Settlement;
    }
  | { op: 'discardPending'; key: CryptoKey; aad: string; sealed: Sealed }
  | { op: 'readNote'; key: CryptoKey; aad: string; sealed: Sealed };

export interface ResultOf {
  deriveKey: CryptoKey;
  generate: { commitment: string; sealed: Sealed };
  confirm: Sealed;
  makeBackup: Uint8Array;
  testBackup: BackupCheck;
  restoreBackup: { records: NoteRecord[]; foreign: number };
  loadProver: { bytes: number; ms: number };
  loadWithdrawalProver: { bytes: number; ms: number };
  prepareWithdrawal: PreparedWithdrawal;
  prepareLease: PreparedLease;
  solvePuzzle: string;
  leaseOpened: Sealed;
  recordExchange: Sealed;
  completeLease: Completed;
  discardPending: Sealed;
  readNote: NoteView;
}

export type WorkerResponse =
  { id: number; ok: true; result: unknown } | { id: number; ok: false; error: string };
