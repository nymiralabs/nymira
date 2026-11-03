// The chat's network edges with stubbed fetch: the tree read, the operator client and its errors,
// the status signature against the real WASM, the models table and the backup rule.
import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import { RpcClient } from '../src/chain/rpc';
import { readSnapshot } from '../src/chain/tree';
import { backupDue } from '../src/chat/backup-policy';
import { checkParams, verifyStatus } from '../src/chat/gate';
import { MODELS } from '../src/chat/models';
import {
  OperatorClient,
  OperatorError,
  OperatorUnreachable,
  describeOperatorError,
  TOKEN_HEADER,
} from '../src/chat/operator';
import { splitObject } from '../src/note/rawjson';
import type { VaultConfigPin } from '../src/profiles/types';
import * as op from '../src/wasm/web-checks-test/nymira_web_wasm.js';
import {
  initSync as initChecks,
  status_signature_valid,
} from '../src/wasm/web-checks/nymira_web_wasm.js';

beforeAll(() => {
  op.initSync({
    module: readFileSync(
      new URL('../src/wasm/web-checks-test/nymira_web_wasm_bg.wasm', import.meta.url),
    ),
  });
  initChecks({
    module: readFileSync(
      new URL('../src/wasm/web-checks/nymira_web_wasm_bg.wasm', import.meta.url),
    ),
  });
});

const rpcAnswer = (result: unknown) =>
  new Response(JSON.stringify({ jsonrpc: '2.0', id: 'x', result }), { status: 200 });
const viewResult = (value: unknown) => ({
  result: [...new TextEncoder().encode(JSON.stringify(value))],
});

describe('tree read', () => {
  it('reads root, count and every page of at most 100 leaves at one block, with fallback', async () => {
    const leaves = Array.from({ length: 250 }, (_, i) => '0x' + i.toString(16).padStart(64, '0'));
    const calls: { url: string; params: Record<string, unknown> }[] = [];
    const fetchImpl = (async (url: string, init: RequestInit) => {
      const body = JSON.parse(init.body as string) as {
        method: string;
        params: Record<string, unknown>;
      };
      calls.push({ url, params: body.params });
      // The first endpoint is down; only the second answers.
      if (url.includes('down')) return new Response('bad gateway', { status: 502 });
      if (body.method === 'block') return rpcAnswer({ header: { hash: 'BLOCKHASH' } });
      const name = body.params.method_name as string;
      const args = JSON.parse(atob(body.params.args_base64 as string)) as {
        from?: number;
        limit?: number;
      };
      if (name === 'current_root') return rpcAnswer(viewResult('0xroot'));
      if (name === 'leaf_count') return rpcAnswer(viewResult(leaves.length));
      return rpcAnswer(
        viewResult(leaves.slice(args.from!, args.from! + Math.min(args.limit!, 100))),
      );
    }) as unknown as typeof fetch;
    const rpc = new RpcClient(['http://down.test', 'http://up.test'], { fetchImpl });
    const snap = await readSnapshot(rpc, 'vault.test');
    expect(snap.leaves).toEqual(leaves);
    expect(snap).toMatchObject({ root: '0xroot', nextNoteId: 250, block: 'BLOCKHASH' });
    // Every view after the block read names that block, and no page asks for more than 100.
    const views = calls.filter(
      (c) => c.params.request_type === 'call_function' && !c.url.includes('down'),
    );
    expect(views.length).toBe(2 + 3);
    for (const v of views) expect(v.params.block_id).toBe('BLOCKHASH');
    expect(calls.some((c) => c.url.includes('down'))).toBe(true);
  });
});

describe('operator client', () => {
  const stub = (answer: () => Response) => {
    const seen: { url: string; init: RequestInit }[] = [];
    const impl = (async (url: string, init: RequestInit) => {
      seen.push({ url, init });
      return answer();
    }) as unknown as typeof fetch;
    return { seen, client: new OperatorClient('https://op.test/', impl) };
  };

  it('sends the lease token and the puzzle as headers and reads the lease answer', async () => {
    const { seen, client } = stub(
      () =>
        new Response(
          JSON.stringify({
            lease_id: 'l1',
            lease_token: 't1',
            bound: '100000',
            issued_at: 5,
            expires_at: 9,
          }),
          { status: 201 },
        ),
    );
    const lease = await client.openLease('{"x":1}', 'abcd:7');
    expect(lease).toMatchObject({ leaseId: 'l1', token: 't1', bound: '100000' });
    expect((seen[0]!.init.headers as Record<string, string>)['X-Nymira-Puzzle']).toBe('abcd:7');
    expect(seen[0]!.init.body).toBe('{"x":1}');

    await client.leaseStatus('l1', 't1').catch(() => undefined);
    expect((seen[1]!.init.headers as Record<string, string>)[TOKEN_HEADER]).toBe('t1');
    await client.chat('{}', 'l1', 't1', 'cc', 'mm').catch(() => undefined);
    expect((seen[2]!.init.headers as Record<string, string>)[TOKEN_HEADER]).toBe('t1');
  });

  it('branches on the error code and keeps the retry hint', async () => {
    const { client } = stub(
      () =>
        new Response(
          JSON.stringify({
            error: { code: 'operator_capacity', message: 'full', retry_after_secs: 30 },
          }),
          {
            status: 429,
          },
        ),
    );
    const err = await client.openLease('{}').catch((e) => e as OperatorError);
    expect(err).toBeInstanceOf(OperatorError);
    expect(err).toMatchObject({ status: 429, code: 'operator_capacity', retryAfterSecs: 30 });
  });

  it('keeps u64 digits of a recovered response and treats a network failure as unreachable', async () => {
    const { client } = stub(
      () =>
        new Response(
          '{"status":"finalized","nullifier_status":"finalized","request_response":{"charge_applied":2000,"big":7718972759422244814}}',
          { status: 200 },
        ),
    );
    const r = await client.recover('c1');
    expect(r.status).toBe('finalized');
    expect(r.responseJson).toContain('7718972759422244814');
    const down = new OperatorClient('https://op.test', (async () => {
      throw new TypeError('Failed to fetch');
    }) as unknown as typeof fetch);
    await expect(down.status()).rejects.toBeInstanceOf(OperatorUnreachable);
  });

  it('shows different messages for operator capacity and the provider rate limit', () => {
    const msg = (code: string) => describeOperatorError(new OperatorError(429, code, 'm', 12));
    expect(msg('operator_capacity')).toContain('operator');
    expect(msg('provider_rate_limit')).toContain('NEAR AI');
    expect(msg('operator_capacity')).not.toBe(msg('provider_rate_limit'));
    expect(msg('operator_capacity')).toContain('12 s');
    // Every other table code has its own text, and an unknown code is a plain error.
    const codes = [
      'puzzle_required',
      'root_stale',
      'nullifier_seen',
      'proof_expired',
      'lease_token_invalid',
      'emergency',
      'provider_timeout',
    ];
    expect(new Set(codes.map(msg)).size).toBe(codes.length);
    expect(describeOperatorError(new OperatorError(418, 'teapot', 'short and stout'))).toContain(
      '418 teapot',
    );
  });
});

describe('status signature (zkapi Schnorr, in WASM)', () => {
  const NOW = Math.floor(Date.now() / 1000);
  const config = {
    bound_classes: ['100000'],
    charge_step: '1000',
    min_charge: '2000',
    slot_rate: 1000,
  } as unknown as VaultConfigPin;
  const status = (seed: bigint, host = 'op.test') => ({
    nonce: { nonce: 'aabbccdd', host, ts: NOW },
    nonce_sig: JSON.parse(
      op.test_operator_sign_status(seed, 'aabbccdd', 'op.test', BigInt(NOW)),
    ) as unknown,
    bound_classes: ['100000'],
    charge_step: '1000',
    min_charge: '2000',
    slot_rate: 1000,
  });
  const check = (
    p: { x: string; y: string },
    n: { nonce: string; host: string; ts: number },
    sig: string,
  ) => status_signature_valid(p.x, p.y, n.nonce, n.host, BigInt(n.ts), sig);

  it('verifies the state key’s signature and refuses another key or a changed host', () => {
    const pk = JSON.parse(op.test_operator_public_key(1n)) as { x: string; y: string };
    expect(verifyStatus(status(1n), pk, NOW, check).host).toBe('op.test');
    expect(() => checkParams(status(1n), config)).not.toThrow();
    expect(() => verifyStatus(status(9n), pk, NOW, check)).toThrow(/state key/);
    expect(() => verifyStatus(status(1n, 'evil.test'), pk, NOW, check)).toThrow(/state key/);
  });
});

describe('tables', () => {
  it('the pinned models and prices equal the operator’s defaults', () => {
    const rust = readFileSync(
      new URL('../../crates/nymira-operator/src/proxy.rs', import.meta.url),
      'utf8',
    );
    const found = [...rust.matchAll(/model\("([^"]+)", (\d+), (\d+)\)/g)].map((m) => ({
      id: m[1],
      inputNano: BigInt(m[2]!),
      outputNano: BigInt(m[3]!),
    }));
    expect(MODELS).toEqual(found);
  });

  it('asks for a backup after every lease above 0.50 USDC and every tenth below it', () => {
    expect(backupDue(900_000n, 1)).toBe(true);
    expect(backupDue(500_000n, 1)).toBe(false);
    expect(backupDue(499_999n, 9)).toBe(false);
    expect(backupDue(499_999n, 10)).toBe(true);
    expect(backupDue(0n, 3)).toBe(true);
  });

  it('splits a JSON object without rounding a u64', () => {
    const parts = splitObject(
      '{"a": {"x": "q\\"}"}, "chain": 7718972759422244814, "list": [1, {"k": 2}] }',
    );
    expect(parts.chain).toBe('7718972759422244814');
    expect(parts.a).toBe('{"x": "q\\"}"}');
    expect(parts.list).toBe('[1, {"k": 2}]');
  });
});
