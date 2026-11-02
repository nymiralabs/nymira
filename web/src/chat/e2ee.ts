// End-to-end encryption of a chat to the model's attested key and the reading of its answer. The
// suite is `nymirad/src/e2ee.rs` (the one the live gateway accepted): Ed25519 public keys on the
// wire, X25519 through the Montgomery map, HKDF-SHA256 (no salt, info `ed25519_encryption`),
// XChaCha20-Poly1305, `[ephemeral key 32][nonce 24][ciphertext and tag]`, hex.
//
// Every field the model reads is encrypted: message `content` and `name`, tool-call names and
// arguments, and tool names, descriptions and parameters. Unknown fields are refused, not
// forwarded in the clear.
import { xchacha20poly1305 } from '@noble/ciphers/chacha.js';
import { ed25519, x25519 } from '@noble/curves/ed25519.js';
import { hkdf } from '@noble/hashes/hkdf.js';
import { sha256 } from '@noble/hashes/sha2.js';
import { fromHex, toHex } from '../crypto/bytes';

const INFO = new TextEncoder().encode('ed25519_encryption');
const FIELDS_TOP = new Set([
  'model',
  'messages',
  'stream',
  'stream_options',
  'max_tokens',
  'max_completion_tokens',
  'n',
  'temperature',
  'top_p',
  'frequency_penalty',
  'presence_penalty',
  'seed',
  'stop',
  'tools',
]);

export class E2eeError extends Error {
  constructor(
    readonly kind: 'unsupported' | 'model_key' | 'decrypt',
    message: string,
  ) {
    super(message);
    this.name = 'E2eeError';
  }
}

/** The client's key pair for one conversation. */
export class ClientKeys {
  private readonly seed: Uint8Array;
  constructor(seed: Uint8Array = crypto.getRandomValues(new Uint8Array(32))) {
    this.seed = seed;
  }
  get publicHex(): string {
    return toHex(ed25519.getPublicKey(this.seed));
  }
  /** The X25519 scalar the answer is opened with. */
  secret(): Uint8Array {
    return ed25519.utils.toMontgomerySecret(this.seed);
  }
}

function cipher(shared: Uint8Array, nonce: Uint8Array) {
  const key = hkdf(sha256, shared, undefined, INFO, 32);
  return xchacha20poly1305(key, nonce);
}

/** Encrypts `plaintext` to the Ed25519 public key `modelPub`; hex. */
export function encrypt(modelPub: Uint8Array, plaintext: Uint8Array): string {
  let x: Uint8Array;
  try {
    x = ed25519.utils.toMontgomery(modelPub);
  } catch {
    throw new E2eeError('model_key', 'the model’s key is not a valid Ed25519 public key');
  }
  const eph = crypto.getRandomValues(new Uint8Array(32));
  const shared = x25519.getSharedSecret(eph, x);
  const nonce = crypto.getRandomValues(new Uint8Array(24));
  const ct = cipher(shared, nonce).encrypt(plaintext);
  return toHex(new Uint8Array([...x25519.getPublicKey(eph), ...nonce, ...ct]));
}

/** Opens a hex ciphertext addressed to `keys`. */
export function decrypt(keys: ClientKeys, text: string): string {
  let data: Uint8Array;
  try {
    data = fromHex(text);
  } catch {
    throw new E2eeError('decrypt', 'an encrypted field of the answer did not decrypt');
  }
  if (!/^[0-9a-f]*$/i.test(text) || data.length < 32 + 24 + 16) {
    throw new E2eeError('decrypt', 'an encrypted field of the answer did not decrypt');
  }
  try {
    const shared = x25519.getSharedSecret(keys.secret(), data.subarray(0, 32));
    const plain = cipher(shared, data.subarray(32, 56)).decrypt(data.subarray(56));
    return new TextDecoder('utf-8', { fatal: true }).decode(plain);
  } catch {
    throw new E2eeError('decrypt', 'an encrypted field of the answer did not decrypt');
  }
}

type Obj = Record<string, unknown>;
const unsupported = (why: string) => new E2eeError('unsupported', `the request cannot be sent end-to-end encrypted: ${why}`);
const enc = (v: unknown, key: Uint8Array, what: string): string => {
  if (typeof v !== 'string') throw unsupported(`${what} must be a string`);
  return encrypt(key, new TextEncoder().encode(v));
};

function sealCall(call: unknown, key: Uint8Array): Obj {
  if (typeof call !== 'object' || call === null) throw unsupported('a tool call is not an object');
  const out = { ...(call as Obj) };
  const fn = out.function;
  if (typeof fn !== 'object' || fn === null) throw unsupported('a tool call has no function');
  const f = { ...(fn as Obj) };
  for (const field of ['name', 'arguments']) if (field in f) f[field] = enc(f[field], key, field);
  out.function = f;
  return out;
}

function sealMessage(m: unknown, key: Uint8Array): Obj {
  if (typeof m !== 'object' || m === null || Array.isArray(m)) {
    throw unsupported('a message is not an object');
  }
  const out: Obj = {};
  for (const [k, v] of Object.entries(m as Obj)) {
    if (k === 'content') {
      if (v === null) out[k] = null;
      else if (typeof v === 'string') out[k] = enc(v, key, 'content');
      else throw unsupported('message content must be one string');
    } else if (k === 'name' && typeof v === 'string') out[k] = enc(v, key, 'name');
    else if (k === 'tool_calls' && Array.isArray(v)) out[k] = v.map((c) => sealCall(c, key));
    else if (k === 'role' || k === 'tool_call_id') out[k] = v;
    else throw unsupported(`the message field ${k} is not supported`);
  }
  return out;
}

function sealTool(t: unknown, key: Uint8Array): Obj {
  if (typeof t !== 'object' || t === null) throw unsupported('a tool is not an object');
  const out = { ...(t as Obj) };
  const fn = out.function;
  if (typeof fn !== 'object' || fn === null) throw unsupported('a tool has no function');
  const f = { ...(fn as Obj) };
  for (const field of ['name', 'description']) if (field in f) f[field] = enc(f[field], key, field);
  // The gateway reads an encrypted JSON string, not an object.
  if ('parameters' in f) f.parameters = encrypt(key, new TextEncoder().encode(JSON.stringify(f.parameters)));
  out.function = f;
  return out;
}

/**
 * The request body the operator and the gateway receive: OpenAI's body with the sensitive fields
 * encrypted to `modelPub`, `max_tokens` set and usage always requested.
 */
export function sealRequest(request: Obj, modelPub: Uint8Array, maxTokens?: number): Obj {
  for (const k of Object.keys(request)) {
    if (!FIELDS_TOP.has(k)) throw unsupported(`the field ${k} is not supported`);
  }
  const out: Obj = {};
  for (const [k, v] of Object.entries(request)) {
    if (k === 'messages') {
      if (!Array.isArray(v)) throw unsupported('messages must be a list');
      out[k] = v.map((m) => sealMessage(m, modelPub));
    } else if (k === 'tools') {
      if (!Array.isArray(v)) throw unsupported('tools must be a list');
      out[k] = v.map((t) => sealTool(t, modelPub));
    } else if (k === 'max_tokens' || k === 'max_completion_tokens' || k === 'stream_options') {
      continue;
    } else {
      out[k] = v;
    }
  }
  if (maxTokens !== undefined) out.max_tokens = maxTokens;
  out.stream_options = { include_usage: true };
  return out;
}

function openField(keys: ClientKeys, parent: Obj, field: string): void {
  const v = parent[field];
  // The empty string opens a stream (`"content": ""`); it holds nothing to decrypt.
  if (typeof v === 'string' && v !== '') parent[field] = decrypt(keys, v);
}

function openMessage(keys: ClientKeys, m: Obj): void {
  for (const field of ['content', 'reasoning_content', 'reasoning']) openField(keys, m, field);
  if (Array.isArray(m.tool_calls)) {
    for (const call of m.tool_calls as Obj[]) {
      if (call.function) for (const field of ['name', 'arguments']) openField(keys, call.function as Obj, field);
    }
  }
}

/** A non-streamed answer (or a streamed chunk) with its encrypted fields opened. */
export function openResponse(keys: ClientKeys, response: Obj): void {
  if (!Array.isArray(response.choices)) return;
  for (const choice of response.choices as Obj[]) {
    for (const part of ['message', 'delta']) {
      if (choice[part]) openMessage(keys, choice[part] as Obj);
    }
  }
}

/** One line of a streamed answer: `data: {chunk}` is opened, harmless lines pass, others are errors. */
export function openSseLine(keys: ClientKeys, line: string): string {
  if (!line.startsWith('data:')) {
    const harmless =
      line === '' || line.startsWith(':') || ['event:', 'id:', 'retry:'].some((f) => line.startsWith(f));
    if (harmless) return line;
    throw new E2eeError('decrypt', 'an encrypted field of the answer did not decrypt');
  }
  const data = line.slice(5).trim();
  if (data === '[DONE]') return line;
  let chunk: Obj;
  try {
    chunk = JSON.parse(data) as Obj;
  } catch {
    throw new E2eeError('decrypt', 'an encrypted field of the answer did not decrypt');
  }
  openResponse(keys, chunk);
  return `data: ${JSON.stringify(chunk)}`;
}
