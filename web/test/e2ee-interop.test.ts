// The chat encryption of `src/chat/e2ee.ts` against `crates/nymirad/src/e2ee.rs`, byte for byte.
// `e2ee-rust.json` is made by the Rust side (`cargo test -p nymirad --lib e2ee`, which fails when
// it is stale): this file opens it. `e2ee-ts.json` is made here and committed; the Rust side opens
// it (`rust_opens_what_the_web_code_sealed`). Both use fixed keys and fixed randomness.
// Regenerate with NYMIRA_BLESS_FIXTURES=1.
import { readFileSync, writeFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ed25519 } from '@noble/curves/ed25519.js';
import {
  ClientKeys,
  decrypt,
  encryptWith,
  openResponse,
  openSseLine,
  sealRequest,
} from '../src/chat/e2ee';
import { OperatorClient } from '../src/chat/operator';
import { fromHex, toHex } from '../src/crypto/bytes';

const FIXTURES = new URL('../../crates/nymirad/tests/fixtures/', import.meta.url);
const read = (name: string) => JSON.parse(readFileSync(new URL(name, FIXTURES), 'utf8'));
const rust = read('e2ee-rust.json');
const recordedKey = (): Uint8Array =>
  fromHex(
    JSON.parse(
      readFileSync(
        new URL('../test-fixtures/near-ai-recorded/model-report.json', import.meta.url),
        'utf8',
      ),
    ).model_attestations[0].signing_public_key,
  );

const modelSeed = fromHex(rust.model_seed);
const clientSeed = fromHex(rust.client_seed);
const model = new ClientKeys(modelSeed);
const client = new ClientKeys(clientSeed);
const modelPub = ed25519.getPublicKey(modelSeed);
const clientPub = ed25519.getPublicKey(clientSeed);
const text = (s: string) => new TextEncoder().encode(s);

afterEach(() => vi.restoreAllMocks());

describe('Rust to web', () => {
  it('uses the keys the fixture names', () => {
    expect(toHex(modelPub)).toBe(rust.model_pub);
    expect(client.publicHex).toBe(rust.client_pub);
  });

  it('opens every field of a request that nymirad sealed', () => {
    const plain = rust.request;
    const sealed = rust.sealed_request;
    const open = (v: string) => decrypt(model, v);
    expect(sealed.max_tokens).toBe(256);
    expect(sealed.stream_options).toEqual({ include_usage: true });
    plain.messages.forEach((m: Record<string, any>, i: number) => {
      const s = sealed.messages[i];
      expect(s.role).toBe(m.role);
      expect(s.tool_call_id).toBe(m.tool_call_id);
      if (m.content === null) expect(s.content).toBeNull();
      else expect(open(s.content)).toBe(m.content);
      if (m.name) expect(open(s.name)).toBe(m.name);
      (m.tool_calls ?? []).forEach((c: any, j: number) => {
        expect(open(s.tool_calls[j].function.name)).toBe(c.function.name);
        expect(open(s.tool_calls[j].function.arguments)).toBe(c.function.arguments);
      });
    });
    const tool = sealed.tools[0].function;
    const want = plain.tools[0].function;
    expect(open(tool.name)).toBe(want.name);
    expect(open(tool.description)).toBe(want.description);
    expect(JSON.parse(open(tool.parameters))).toEqual(want.parameters);
  });

  it('opens an answer and a streamed chunk that nymirad encrypted to the client key', () => {
    const answer = structuredClone(rust.answer);
    openResponse(client, answer);
    expect(answer).toEqual(rust.answer_plain);
    expect(openSseLine(client, rust.sse_line)).toContain(JSON.stringify(rust.sse_plain_content));
  });

  it('seals to the model key NEAR AI attested with the same bytes', () => {
    const r = rust.recorded;
    expect(toHex(recordedKey())).toBe(r.model_pub);
    expect(
      encryptWith(recordedKey(), text(r.plaintext), fromHex(r.eph_secret), fromHex(r.nonce)),
    ).toBe(r.ciphertext);
  });

  it('sends the request headers nymirad sends', async () => {
    expect(await chatHeaders(rust.client_pub, rust.model_pub)).toEqual(rust.headers);
  });
});

async function chatHeaders(clientPubHex: string, modelPubHex: string) {
  let seen: Record<string, string> = {};
  const op = new OperatorClient('http://op.test', async (_url, init) => {
    seen = init?.headers as Record<string, string>;
    return new Response('{}');
  });
  await op.chat('{}', 'lease', 'token', clientPubHex, modelPubHex);
  return Object.fromEntries(
    Object.entries(seen).filter(([k]) => k.startsWith('x-') && k !== 'x-nymira-lease-id'),
  );
}

function tsFixture() {
  // Fixed randomness: the n-th encryption draws secret [n; 32] then nonce [n; 24].
  let calls = 0;
  vi.spyOn(crypto, 'getRandomValues').mockImplementation(((a: Uint8Array) => {
    a.fill(1 + Math.floor(calls++ / 2));
    return a;
  }) as never);
  const sealed_request = sealRequest(structuredClone(rust.request), modelPub, 256);
  vi.restoreAllMocks();

  const e = (s: string, n: number) =>
    encryptWith(clientPub, text(s), new Uint8Array(32).fill(n), new Uint8Array(24).fill(n));
  const p = rust.answer_plain.choices[0].message;
  const answer = {
    choices: [
      {
        message: {
          role: 'assistant',
          content: e(p.content, 1),
          reasoning_content: e(p.reasoning_content, 2),
          tool_calls: [
            {
              id: p.tool_calls[0].id,
              type: 'function',
              function: {
                name: e(p.tool_calls[0].function.name, 3),
                arguments: e(p.tool_calls[0].function.arguments, 4),
              },
            },
          ],
        },
      },
    ],
    usage: rust.answer_plain.usage,
  };
  const sse_line = `data: ${JSON.stringify({ choices: [{ delta: { content: e(rust.sse_plain_content, 5) } }] })}`;
  const r = rust.recorded;
  return {
    model_seed: rust.model_seed,
    model_pub: rust.model_pub,
    client_seed: rust.client_seed,
    client_pub: rust.client_pub,
    request: rust.request,
    max_tokens: 256,
    sealed_request,
    headers: null as unknown,
    answer_plain: rust.answer_plain,
    answer,
    sse_plain_content: rust.sse_plain_content,
    sse_line,
    recorded: {
      ...r,
      ciphertext: encryptWith(
        recordedKey(),
        text(r.plaintext),
        fromHex(r.eph_secret),
        fromHex(r.nonce),
      ),
    },
  };
}

describe('web to Rust', () => {
  it('regenerates the committed fixture that the Rust tests open', async () => {
    const fresh = tsFixture();
    fresh.headers = await chatHeaders(client.publicHex, toHex(modelPub));
    const out = JSON.stringify(fresh, null, 2) + '\n';
    const file = new URL('e2ee-ts.json', FIXTURES);
    if (process.env.NYMIRA_BLESS_FIXTURES) writeFileSync(file, out);
    expect(readFileSync(file, 'utf8')).toBe(out);
  });

  it('opens its own answer fixture (a self-check of the fixture)', () => {
    const f = read('e2ee-ts.json');
    const answer = structuredClone(f.answer);
    openResponse(client, answer);
    expect(answer).toEqual(f.answer_plain);
  });
});
