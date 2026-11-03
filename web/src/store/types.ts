import type { Sealed } from '../crypto/aead';

/**
 * Where a funding attempt stands. The order is the order of the flow:
 *  prepared    secret created and stored sealed; nothing sent
 *  backed_up   a backup file was downloaded and test-restored for this note
 *  sending     the transfer is being handed to the wallet (it may be lost to a crash here)
 *  sent        the wallet returned; acceptance not yet read from the receipt
 *  accepted    the `deposit` event matched; the genesis state is stored sealed
 *  rejected    the receipt has no matching `deposit` event (the token refunded the transfer)
 */
export type NotePhase = 'prepared' | 'backed_up' | 'sending' | 'sent' | 'accepted' | 'rejected';

/** What a sealed payload holds: the bare secret before acceptance, the wallet state after. */
export type SealedKind = 'pending' | 'state';

/**
 * Where a note stands on its way out (public values only):
 *  closing   close proofs made (a clearance may have been issued); the vault does not show it closed
 *  closed    the vault shows the note closed by a mutual close
 *  exiting   an exit proof was made; `deadline` is set once `request_exit` shows in the vault
 *  exited    `finalize_exit` went through (a payout that failed waits for `claim_payout`)
 * A note with `leave` set opens no new lease, here or in any restored copy.
 */
export interface LeaveInfo {
  state: 'closing' | 'closed' | 'exiting' | 'exited';
  /** How the close was sent: the user's wallet, or the operator's `POST /v1/close`. */
  via?: 'wallet' | 'operator';
  txHash?: string;
  /** Unix seconds after which `finalize_exit` works. */
  deadline?: number;
  /** Unix seconds the state last changed. */
  at: number;
}

/**
 * One note in IndexedDB. Everything except `sealed` is public information (it is on chain or it
 * is what the user typed); the note secret exists only inside `sealed`.
 */
export interface NoteRecord {
  id: string;
  network: string;
  vault: string;
  token: string;
  /** Denomination in token base units, as the vault names it. */
  denomination: string;
  /** The account the vault will pay (fixed at deposit). */
  payout: string;
  /** True when the deposit message carried `withdraw_to`. */
  payoutExplicit: boolean;
  sender: string;
  commitment: string;
  phase: NotePhase;
  txHash?: string;
  noteId?: number;
  expiry?: number;
  createdAt: number;
  updatedAt: number;
  leave?: LeaveInfo;
  /** What the expiry guard has shown or tried (`leave/guard.ts`). */
  guard?: { notice?: number; closeFailed?: boolean; lastTry?: number };
  kind: SealedKind;
  sealed: Sealed;
}

/** How the wrapping key is derived. Stored in the `meta` store under `wrap`. */
export type WrapMeta =
  | {
      v: 1;
      method: 'passphrase';
      kdf: { alg: 'argon2id'; m: number; t: number; p: number; salt: string };
      check: Sealed;
    }
  | {
      v: 1;
      method: 'webauthn-prf';
      credentialId: string;
      prfSalt: string;
      check: Sealed;
    };

export const WRAP_CHECK_TEXT = 'nymira-wrap-check';

/** The context a sealed payload is bound to (AES-GCM additional data). */
export const noteAad = (network: string, vault: string, id: string): string =>
  `nymira:v1:note:${network}:${vault}:${id}`;
export const WRAP_AAD = 'nymira:v1:wrap-check';
