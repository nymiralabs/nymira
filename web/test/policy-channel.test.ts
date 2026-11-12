// The site's newer signed policy (ADR 0021): taken only when signed by the bundled key and newer.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { ed25519 } from '@noble/curves/ed25519.js';
import { newerPolicy, policyUpdates, SIDE_FILE } from '../src/chat/attest/channel';
import { signedText, type Policy } from '../src/chat/attest/policy';
import { toHex } from '../src/crypto/bytes';

const seed = new Uint8Array(32).fill(7);
const pk = toHex(ed25519.getPublicKey(seed));
const sign = (p: Policy, key = seed): Policy => ({
  ...p,
  signature: toHex(ed25519.sign(new TextEncoder().encode(signedText(p)), key)),
});
const base = JSON.parse(
  readFileSync(new URL('../pins/attestation-policy.json', import.meta.url), 'utf8'),
) as Policy;
const bundled = sign({ ...structuredClone(base), version: 6 });
const newer = (version: number, key = seed) => {
  const p = structuredClone(bundled);
  p.version = version;
  p.deployments.find((d) => d.role === 'model')!.compose_hashes.push('ee'.repeat(32));
  return sign(p, key);
};
const answer =
  (body: string, status = 200): typeof fetch =>
  async () =>
    new Response(body, { status });

describe('a newer policy beside the bundle', () => {
  it('checks again for each conversation and keeps the newest verified policy if the site fails or rolls back', async () => {
    let response = answer(JSON.stringify(newer(7)));
    const get: typeof fetch = (input, init) => response(input, init);
    const current = policyUpdates(bundled, pk, get);
    expect((await current()).policy.version).toBe(7);
    response = answer(JSON.stringify(newer(8)));
    expect((await current()).policy.version).toBe(8);
    response = answer('', 503);
    expect((await current()).policy.version).toBe(8);
    response = answer(JSON.stringify(newer(7)));
    expect((await current()).policy.version).toBe(8);
  });
  it('is used when it is signed by the bundled key and its version is higher', async () => {
    const got = await newerPolicy(bundled, pk, answer(JSON.stringify(newer(7))));
    expect(got.from).toBe('site');
    expect(got.policy.version).toBe(7);
    expect(got.policy.deployments.find((d) => d.role === 'model')!.compose_hashes).toContain(
      'ee'.repeat(32),
    );
  });

  it('asks for the file of the site and sends nothing else', async () => {
    let seen: { url: string; init?: RequestInit } | undefined;
    const spy: typeof fetch = async (url, init) => {
      seen = { url: String(url), init };
      return new Response('', { status: 404 });
    };
    await newerPolicy(bundled, pk, spy);
    expect(seen?.url).toBe(SIDE_FILE);
    expect(seen?.init).toMatchObject({ credentials: 'omit', cache: 'no-store', redirect: 'error' });
  });

  it('is ignored when it is not newer, so a rollback changes nothing', async () => {
    for (const v of [6, 5]) {
      const got = await newerPolicy(bundled, pk, answer(JSON.stringify(newer(v))));
      expect(got).toEqual({ policy: bundled, from: 'bundle', note: undefined });
    }
  });

  it('is ignored when another key signed it', async () => {
    const other = newer(7, new Uint8Array(32).fill(8));
    const got = await newerPolicy(bundled, pk, answer(JSON.stringify(other)));
    expect(got.from).toBe('bundle');
    expect(got.note).toMatch(/signature/);
  });

  it('is ignored when a signed value was changed', async () => {
    const p = newer(7);
    p.deployments[0]!.mr_td = '00'.repeat(48);
    const got = await newerPolicy(bundled, pk, answer(JSON.stringify(p)));
    expect(got.from).toBe('bundle');
  });

  it('leaves the bundled policy in force when the file is missing, broken, huge or unreachable', async () => {
    expect((await newerPolicy(bundled, pk, answer('', 404))).note).toBeUndefined();
    expect((await newerPolicy(bundled, pk, answer('<html>', 200))).from).toBe('bundle');
    expect((await newerPolicy(bundled, pk, answer('x'.repeat(70_000)))).note).toMatch(/large/);
    expect((await newerPolicy(bundled, pk, answer('', 503))).note).toMatch(/503/);
    const down: typeof fetch = async () => {
      throw new TypeError('network down');
    };
    expect((await newerPolicy(bundled, pk, down)).from).toBe('bundle');
  });

  it('refuses a policy that accepts a lenient TCB status, like the bundled loader', async () => {
    const p = structuredClone(newer(7));
    p.accepted_tcb_statuses = ['UpToDate', 'OutOfDate'];
    const got = await newerPolicy(bundled, pk, answer(JSON.stringify(sign(p))));
    expect(got.from).toBe('bundle');
    expect(got.note).toMatch(/strict/);
  });
});
