import { createHash } from 'node:crypto';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { RpcClient } from '../src/chain/rpc';
import { OperatorClient, OperatorError } from '../src/chat/operator';
import { WhoSeesWhat } from '../src/ui/Landing';
import { coverOf, small, thin } from '../src/privacy/cover';
import {
  countable,
  differences,
  evaluate,
  headline,
  read,
  readNow,
  type Raw,
} from '../src/privacy/setsize';
import { fill, sentence } from '../src/privacy/strings';
import * as text from '../src/privacy/text';

const root = join(__dirname, '..', '..');
const sharedPath = join(root, 'shared', 'privacy-strings.json');
const rust = readFileSync(join(root, 'crates/nymirad/src/strings.rs'), 'utf8');

describe('the shared sentence file', () => {
  it('is the file nymirad pins, hash and keys', () => {
    const raw = readFileSync(sharedPath);
    const pinned = /"([0-9a-f]{64})"/.exec(rust)?.[1];
    expect(createHash('sha256').update(raw).digest('hex')).toBe(pinned);
    const keysBlock = /let keys[^;]*;\s*assert_eq!\(\s*keys,\s*\[([^\]]*)\]/s.exec(rust)?.[1] ?? '';
    const rustKeys = [...keysBlock.matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
    expect(Object.keys(JSON.parse(raw.toString())).sort()).toEqual(rustKeys);
  });

  it('is imported, not copied: no web source repeats a shared sentence', () => {
    const shared = Object.values(
      JSON.parse(readFileSync(sharedPath, 'utf8')) as object,
    ) as string[];
    const files: string[] = [];
    for (const dir of ['src/privacy', 'src/ui']) {
      for (const f of readdirSync(join(__dirname, '..', dir))) {
        files.push(join(__dirname, '..', dir, f));
      }
    }
    for (const file of files.filter((f) => /\.tsx?$/.test(f))) {
      const source = readFileSync(file, 'utf8');
      for (const s of shared) {
        // A sentence with a placeholder is compared up to the placeholder.
        const head = s.split('{')[0]!.trim();
        if (head.length >= 20) expect(source, `${file} repeats "${head}"`).not.toContain(head);
      }
    }
  });

  it('fills placeholders', () => {
    expect(fill('set_size', { n: 3 })).toBe('set size: 3 (external)');
    expect(sentence('now_label')).toBe('now');
  });

  it('has no em dash and the panel text makes no unlinkability claim', () => {
    const all = [
      ...Object.values(JSON.parse(readFileSync(sharedPath, 'utf8')) as object),
      ...Object.values(text).filter((v): v is string => typeof v === 'string'),
    ] as string[];
    for (const t of all) {
      expect(t).not.toContain('—');
      expect(t).not.toMatch(
        /cannot be (linked|traced|tied)|can't be (linked|traced)|not linkable|never be linked|unlinkable|untraceable|anonymous|hidden from/i,
      );
    }
  });
});

describe('set size, the cases nymirad runs too', () => {
  const cases = JSON.parse(readFileSync(join(root, 'shared/set-size-cases.json'), 'utf8')) as {
    name: string;
    sets: number[][];
    one_provider: string | null;
    pinned: number[];
    operator: number[] | null;
    headline: string;
    differences: string[];
  }[];
  for (const c of cases) {
    it(c.name, () => {
      const raw: Raw = { sets: c.sets.map((s) => new Set(s)), oneProvider: c.one_provider };
      const size = evaluate(raw, c.pinned, c.operator);
      expect(headline(size)).toBe(c.headline);
      expect(differences(size)).toEqual(c.differences);
    });
  }
});

// A fake vault: one provider's `note_records` and `leaf_count`, as the views answer them.
function fakeProvider(records: { id: number; status: string; expiry: number }[], block = 'B1') {
  const calls: unknown[] = [];
  const fetchImpl = (async (_url: string, init: { body: string }) => {
    const body = JSON.parse(init.body) as { method: string; params: Record<string, unknown> };
    const send = (result: unknown) =>
      new Response(JSON.stringify({ jsonrpc: '2.0', id: 1, result }));
    if (body.method === 'block') return send({ header: { hash: block } });
    const args = JSON.parse(
      Buffer.from(body.params.args_base64 as string, 'base64').toString(),
    ) as { from?: number; limit?: number };
    calls.push({ method: body.params.method_name, block: body.params.block_id, args });
    let value: unknown;
    if (body.params.method_name === 'leaf_count') value = records.length;
    else {
      value = records.slice(args.from, args.from! + Math.min(args.limit!, 100)).map((r) => ({
        note_id: r.id,
        deposit_amount: '1000000',
        expiry: r.expiry,
        status: r.status,
        leaf_retired: false,
      }));
    }
    return send({ result: [...new TextEncoder().encode(JSON.stringify(value))] });
  }) as unknown as typeof fetch;
  return { rpc: new RpcClient(['https://p.test'], { fetchImpl }), calls };
}

const T = 1_800_000_000;
const active = (n: number) =>
  Array.from({ length: n }, (_, id) => ({ id, status: 'Active', expiry: T + 1000 }));

describe('reading note_records', () => {
  it('pages at 100 and reads at the proof block', async () => {
    const p = fakeProvider(active(230));
    const set = await countable(p.rpc, 'v.test', 'BLOCK', 230, T);
    expect(set.size).toBe(230);
    const reads = p.calls.filter((c) => (c as { method: string }).method === 'note_records');
    expect(reads).toHaveLength(3);
    expect(reads.every((c) => (c as { block: string }).block === 'BLOCK')).toBe(true);
  });

  it('drops expired, pending exit, closed and later-deposited notes', async () => {
    const records = [
      { id: 0, status: 'Active', expiry: T },
      { id: 1, status: 'Active', expiry: T - 1 },
      { id: 2, status: 'PendingExit', expiry: T + 5 },
      { id: 3, status: 'Closed', expiry: T + 5 },
      { id: 4, status: 'Active', expiry: T + 5 },
      { id: 5, status: 'Active', expiry: T + 5 },
    ];
    const p = fakeProvider(records);
    // Leaf count 5 at the proof: note 5 was deposited later and is not read.
    expect([...(await countable(p.rpc, 'v.test', 'B', 5, T))]).toEqual([0, 4]);
  });

  it('refuses a page that skips an ID', async () => {
    const p = fakeProvider(active(3).map((r) => ({ ...r, id: r.id + 1 })));
    await expect(countable(p.rpc, 'v.test', 'B', 3, T)).rejects.toThrow('malformed');
  });

  it('takes the lower of two providers, and says when only one answered', async () => {
    const a = fakeProvider(active(6));
    const b = fakeProvider(active(6).map((r) => (r.id === 5 ? { ...r, status: 'Closed' } : r)));
    const both = evaluate(await read([a.rpc, b.rpc], 'v.test', 'B', 6, T), [0, 1, 2], [0, 1, 2]);
    expect([both.external, both.total]).toEqual([2, 5]);
    expect(both.providersDiffer).toEqual([
      [3, 6],
      [2, 5],
    ]);
    const broken = new RpcClient(['https://down.test'], {
      fetchImpl: (async () => new Response('x', { status: 500 })) as unknown as typeof fetch,
      cooldownMs: 0,
    });
    const one = await read([a.rpc, broken], 'v.test', 'B', 6, T);
    expect(one.sets).toHaveLength(1);
    expect(one.oneProvider).toContain('no RPC endpoint answered');
    await expect(read([broken], 'v.test', 'B', 6, T)).rejects.toThrow();
  });

  it('reads now at the current final block', async () => {
    const p = fakeProvider(active(4), 'HEAD');
    const raw = await readNow([p.rpc], 'v.test', T);
    expect(raw.sets[0]!.size).toBe(4);
    expect(raw.oneProvider).toBe('no second provider is configured');
    expect(p.calls.every((c) => (c as { block: string }).block === 'HEAD')).toBe(true);
  });
});

describe('cover', () => {
  const team = new Set([1, 2]);
  it('counts deposits between from leaf counts with the team removed', () => {
    const c = coverOf(0, 1000, { openedAt: 1600, leafCount: 6 }, team);
    expect(c).toEqual({ gapSecs: 600, between: 5, externalBetween: 3 });
  });
  it('flags under an hour or under five external deposits, and small sets', () => {
    expect(thin({ gapSecs: 3599, between: 9, externalBetween: 9 })).toBe(true);
    expect(thin({ gapSecs: 3600, between: 9, externalBetween: 4 })).toBe(true);
    expect(thin({ gapSecs: 3600, between: 9, externalBetween: 5 })).toBe(false);
    expect(small(4)).toBe(true);
    expect(small(5)).toBe(false);
    expect(text.THRESHOLDS).toContain('1 hour');
    expect(text.THRESHOLDS).toContain('fewer than 5 external');
  });
  it('words the gap', () => {
    expect(text.describeGap(600)).toBe('10 minutes');
    expect(text.describeGap(7200)).toBe('2 hours');
  });
});

describe('the panel sentences', () => {
  const shared = JSON.parse(readFileSync(sharedPath, 'utf8')) as Record<string, string>;
  const panelSource = readFileSync(join(__dirname, '..', 'src/ui/Privacy.tsx'), 'utf8');

  it('says the open-note sentence with each way the leases can still be linked', () => {
    const open = shared.link_open!;
    expect(open).toContain('designed to leave no zero-knowledge link');
    expect(open).toContain('network address');
    expect(open).toContain('timing and usage metadata');
    expect(open).toContain('timing alone while the set is small');
    expect(shared.link_left).toContain('now tied to its note ID');
  });

  it('keeps every sentence free of em dashes and unlinkability claims, panel text included', () => {
    for (const t of Object.values(shared)) {
      expect(t).not.toContain('—');
      expect(t).not.toMatch(/cannot be linked|unlinkable|untraceable|anonymous|hidden from/i);
    }
    // Literal text in the panel source stays out of the claim too.
    expect(panelSource).not.toMatch(/cannot be linked|unlinkable|untraceable|anonymous/i);
  });

  it('draws the landing drawing and the observer list from the same sentences', () => {
    const drawing = renderToStaticMarkup(createElement(WhoSeesWhat)).replaceAll('&#x27;', "'");
    for (const key of ['who_operator', 'who_model', 'who_chain']) {
      expect(drawing, key).toContain(shared[key]);
      expect(panelSource, key).toContain(`'${key}'`);
    }
  });

  it('lists every observer the ticket names', () => {
    for (const label of [
      'Chain',
      'Operator',
      'public entry',
      'second machine',
      'NEAR AI',
      'RPC providers',
      'Web host',
      'Attestation services',
      'Certificate logs',
      '1Click and the Intents explorer',
    ]) {
      expect(panelSource, label).toContain(label);
    }
    expect(shared.who_rpc).toContain('receipt of your own deposit');
  });

  it('never asks a public node about the nullifier and never calls is_nullifier_used', () => {
    for (const dir of ['src/ui', 'src/privacy', 'src/chat', 'src/chain', 'src/note']) {
      for (const f of readdirSync(join(__dirname, '..', dir))) {
        if (!/\.tsx?$/.test(f)) continue;
        const source = readFileSync(join(__dirname, '..', dir, f), 'utf8');
        expect(source, `${dir}/${f}`).not.toContain('is_nullifier_used');
      }
    }
    // The status goes to the operator's own route and no RPC client is used in the check.
    expect(panelSource).toContain('operator.nullifierStatus');
    expect(panelSource).not.toMatch(/\brpc\b[^;]*nullifier/i);
  });
});

describe('operator client, reuse check', () => {
  const N = '0x' + '1'.repeat(64);

  it('reads the nullifier status from the operator route', async () => {
    const seen: string[] = [];
    const fetchImpl = (async (url: string) => {
      seen.push(url);
      return new Response(JSON.stringify({ nullifier_status: 'finalized', status: 'finalized' }));
    }) as unknown as typeof fetch;
    const client = new OperatorClient('https://op.test/', fetchImpl);
    expect(await client.nullifierStatus(N)).toBe('finalized');
    expect(seen).toEqual([`https://op.test/v2/nullifiers/${N}`]);
  });

  it('returns a refusal as given and an acceptance as accepted', async () => {
    const refuse = (async () =>
      new Response(
        JSON.stringify({ error: { code: 'nullifier_seen', message: '<a href="x">spent</a>' } }),
        { status: 409 },
      )) as unknown as typeof fetch;
    const answer = await new OperatorClient('https://op.test', refuse).resend('{"r":1}');
    expect(answer).toBeInstanceOf(OperatorError);
    expect((answer as OperatorError).code).toBe('nullifier_seen');
    expect((answer as OperatorError).message).toBe('<a href="x">spent</a>');
    const accept = (async () => new Response('{}', { status: 201 })) as unknown as typeof fetch;
    expect(await new OperatorClient('https://op.test', accept).resend('{"r":1}')).toEqual({
      accepted: true,
    });
  });
});
