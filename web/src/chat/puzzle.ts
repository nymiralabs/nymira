// Hashcash bound to the status slot (`nymira-operator/src/puzzle.rs`): the header value is
// `<slot nonce>:<counter>` and SHA-256("<nonce>:<counter>") starts with `difficulty` zero bits.
// It runs in the worker; a high difficulty keeps the main thread free.
import { sha256 } from '@noble/hashes/sha2.js';

export function leadingZeroBits(digest: Uint8Array): number {
  let bits = 0;
  for (const byte of digest) {
    if (byte === 0) {
      bits += 8;
    } else {
      return bits + Math.clz32(byte) - 24;
    }
  }
  return bits;
}

const encoder = new TextEncoder();

export function solvePuzzle(nonce: string, difficulty: number): string {
  const bits = Math.min(difficulty, 32);
  for (let counter = 0; ; counter++) {
    if (leadingZeroBits(sha256(encoder.encode(`${nonce}:${counter}`))) >= bits) {
      return `${nonce}:${counter}`;
    }
  }
}
