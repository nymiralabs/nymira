import { describe, expect, it } from 'vitest';
import { ed25519 } from '@noble/curves/ed25519.js';
import {
  ClientKeys,
  E2eeError,
  decrypt,
  encrypt,
  openResponse,
  openSseLine,
  sealRequest,
} from '../src/chat/e2ee';

const modelSeed = new Uint8Array(32).fill(5);
const model = new ClientKeys(modelSeed);
const modelPub = ed25519.getPublicKey(modelSeed);

describe('e2ee', () => {
  it('a round trip needs the right secret and uses a fresh ephemeral key', () => {
    const sealed = encrypt(modelPub, new TextEncoder().encode('hello'));
    expect(decrypt(model, sealed)).toBe('hello');
    expect(() => decrypt(new ClientKeys(), sealed)).toThrow(E2eeError);
    expect(sealed).not.toBe(encrypt(modelPub, new TextEncoder().encode('hello')));
  });

  it('seals every sensitive field, sets max_tokens and always asks for usage', () => {
    const sealed = sealRequest(
      {
        model: 'z-ai/glm-5.3-flash',
        messages: [
          { role: 'system', content: 'be brief' },
          { role: 'user', content: 'secret question', name: 'alice' },
        ],
        stream: false,
        temperature: 0.5,
        max_tokens: 1,
        tools: [{ type: 'function', function: { name: 'f', description: 'd', parameters: { a: 1 } } }],
      },
      modelPub,
      123,
    ) as {
      model: string;
      max_tokens: number;
      stream_options: unknown;
      messages: { role: string; content: string; name?: string }[];
      tools: { function: { name: string; parameters: string } }[];
    };
    expect(sealed.model).toBe('z-ai/glm-5.3-flash');
    expect(sealed.max_tokens).toBe(123);
    expect(sealed.stream_options).toEqual({ include_usage: true });
    expect(JSON.stringify(sealed)).not.toContain('secret question');
    expect(JSON.stringify(sealed)).not.toContain('alice');
    expect(decrypt(model, sealed.messages[1]!.content)).toBe('secret question');
    expect(decrypt(model, sealed.messages[1]!.name!)).toBe('alice');
    expect(JSON.parse(decrypt(model, sealed.tools[0]!.function.parameters))).toEqual({ a: 1 });
  });

  it('refuses a field it cannot encrypt instead of sending it in the clear', () => {
    expect(() => sealRequest({ model: 'm', messages: [], logit_bias: {} }, modelPub)).toThrow(
      /not supported/,
    );
    expect(() =>
      sealRequest({ model: 'm', messages: [{ role: 'user', content: ['x'] }] }, modelPub),
    ).toThrow(/one string/);
  });

  it('opens an answer and a streamed chunk, and refuses ciphertext it cannot open', () => {
    const client = new ClientKeys();
    const pub = new Uint8Array(Buffer.from(client.publicHex, 'hex'));
    const answer = {
      choices: [
        {
          message: {
            role: 'assistant',
            content: encrypt(pub, new TextEncoder().encode('hi there')),
            reasoning_content: encrypt(pub, new TextEncoder().encode('thinking')),
          },
        },
      ],
    };
    openResponse(client, answer);
    expect(answer.choices[0]!.message.content).toBe('hi there');
    expect(answer.choices[0]!.message.reasoning_content).toBe('thinking');

    const line = `data: ${JSON.stringify({ choices: [{ delta: { content: encrypt(pub, new TextEncoder().encode('x')) } }] })}`;
    expect(openSseLine(client, line)).toContain('"content":"x"');
    expect(openSseLine(client, 'data: [DONE]')).toBe('data: [DONE]');
    expect(openSseLine(client, '')).toBe('');
    expect(() => openSseLine(client, 'text without a field')).toThrow(E2eeError);
    expect(() => openSseLine(client, 'data: {"choices":[{"delta":{"content":"zz11"}}]}')).toThrow(
      E2eeError,
    );
  });
});
