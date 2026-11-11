// A policy that pins NVIDIA certificates is signed under the v3 text that `nymirad` uses
// (`Policy::signed_text`); a policy without pins keeps the v2 text.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { ed25519 } from '@noble/curves/ed25519.js';
import { loadPolicy, signedText, type Policy } from '../src/chat/attest/policy';
import { toHex } from '../src/crypto/bytes';

const policyJson = JSON.parse(
  readFileSync(new URL('../pins/attestation-policy.json', import.meta.url), 'utf8'),
) as Policy;
const signerPk = readFileSync(new URL('../pins/policy-signer.pub', import.meta.url), 'utf8').trim();

const seed = new Uint8Array(32).fill(9);
const testPk = toHex(ed25519.getPublicKey(seed));
const sign = (p: Policy): Policy => ({
  ...p,
  signature: toHex(ed25519.sign(new TextEncoder().encode(signedText(p)), seed)),
});
const pin = { name: 'Test Intermediate', sha256: 'ab'.repeat(32), official_source_verified: false };
const base = (): Policy => ({
  ...structuredClone(policyJson),
  nvidia_pins: [structuredClone(pin)],
});
const unpinned = (p: Policy): Policy => {
  const { nvidia_pins: _drop, ...rest } = p;
  return rest;
};

describe('a policy that pins NVIDIA certificates', () => {
  it('is signed under the v3 text, as nymirad signs it', () => {
    const p = base();
    const old = signedText(unpinned(p));
    expect(old).toMatch(/^nymira\/attestation-policy\/v2\|/);
    expect(signedText(p)).toBe(
      old.replace('attestation-policy/v2', 'attestation-policy/v3') +
        `|Test Intermediate,${'ab'.repeat(32)},false`,
    );
    // An empty list is the same as none: the old text is kept.
    expect(signedText({ ...p, nvidia_pins: [] })).toBe(old);
  });

  it('loads when signed, and keeps the pins', () => {
    expect(loadPolicy(sign(base()), testPk, 1).nvidia_pins).toEqual([pin]);
  });

  it('fails the signature when a pin is added, removed or changed', () => {
    const pinned = sign(base());
    const plain = sign(unpinned(base()));
    expect(() => loadPolicy(unpinned(pinned), testPk, 1)).toThrow(/signature/);
    expect(() => loadPolicy({ ...pinned, nvidia_pins: [] }, testPk, 1)).toThrow(/signature/);
    expect(() => loadPolicy({ ...plain, nvidia_pins: [pin] }, testPk, 1)).toThrow(/signature/);
    for (const edit of [
      { sha256: 'cd'.repeat(32) },
      { official_source_verified: true },
      { name: 'Other' },
    ]) {
      expect(() =>
        loadPolicy({ ...pinned, nvidia_pins: [{ ...pin, ...edit }] }, testPk, 1),
      ).toThrow(/signature/);
    }
  });

  it('refuses a signed pin of the wrong shape', () => {
    for (const bad of [
      { ...pin, sha256: 'ab' },
      { ...pin, sha256: 'AB'.repeat(32) },
      { ...pin, name: '' },
      { ...pin, name: 'a,b' },
      { ...pin, name: 'a;b' },
      { ...pin, name: 'a|b' },
    ]) {
      expect(() => loadPolicy(sign({ ...base(), nvidia_pins: [bad] }), testPk, 1)).toThrow(
        /NVIDIA pin/,
      );
    }
  });

  it('loads the shipped policy under the pinned signer: its version and the two unverified pins', () => {
    const loaded = loadPolicy(policyJson, signerPk, 5);
    expect(loaded.version).toBeGreaterThanOrEqual(6);
    expect(loaded.nvidia_pins?.map((p) => [p.sha256, p.official_source_verified])).toEqual([
      ['38e3daf3cdbd5243bb15dd7993edcdd83a7238cb6745b980f8f3bf0a31a0d343', false],
      ['2df8907cf4d6c277b855d407ec178a649930a8e73f7294e8fce66e6ee27c5175', false],
    ]);
  });
});
