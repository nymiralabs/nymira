// Argon2id (hash-wasm, WASM) for the passphrase that wraps note secrets and for every backup file.
// One parameter set for both, stated in the app: 64 MiB, 3 passes, 1 lane.
//
// Why these numbers: they are above the OWASP minimum (19 MiB, 2 passes) because a leaked backup
// is a spendable note and the passphrase floor is 12 characters, not a long random one. 64 MiB is
// well inside a phone's memory. One lane because the module is single-threaded.
import { argon2id } from 'hash-wasm';

export interface Argon2Params {
  alg: 'argon2id';
  /** Memory in KiB. */
  m: number;
  t: number;
  p: number;
}

export const ARGON2: Argon2Params = { alg: 'argon2id', m: 65536, t: 3, p: 1 };
export const MIN_PASSPHRASE_LENGTH = 12;
export const SALT_BYTES = 16;

export function normalizePassphrase(passphrase: string): string {
  return passphrase.normalize('NFKC');
}

/** Counts characters (code points), so an emoji is one. */
export function passphraseLength(passphrase: string): number {
  return [...normalizePassphrase(passphrase)].length;
}

export function checkPassphrase(passphrase: string): void {
  if (passphraseLength(passphrase) < MIN_PASSPHRASE_LENGTH) {
    throw new Error(`the passphrase needs at least ${MIN_PASSPHRASE_LENGTH} characters`);
  }
}

/** 32 raw key bytes. The caller imports them as a non-extractable key and drops the bytes. */
export async function deriveKeyBytes(
  passphrase: string,
  salt: Uint8Array,
  params: Argon2Params = ARGON2,
): Promise<Uint8Array> {
  checkPassphrase(passphrase);
  if (params.alg !== 'argon2id') throw new Error('unsupported KDF');
  return argon2id({
    password: normalizePassphrase(passphrase),
    salt,
    parallelism: params.p,
    iterations: params.t,
    memorySize: params.m,
    hashLength: 32,
    outputType: 'binary',
  });
}
