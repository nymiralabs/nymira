// Verifies the NEP-413 signed message a wallet returns at sign-in: the wallet that connected holds
// the key it names. The signature is over SHA-256(u32le(2^31 + 413) || borsh(payload)) with
// payload = { message: string, nonce: [u8; 32], recipient: string, callbackUrl: Option<string> }.
import { b64decode } from '../crypto/bytes';

const TAG = 2 ** 31 + 413;
const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

export function base58Decode(text: string): Uint8Array {
  const bytes: number[] = [];
  for (const char of text) {
    let carry = ALPHABET.indexOf(char);
    if (carry < 0) throw new Error('invalid base58');
    for (let i = 0; i < bytes.length; i++) {
      carry += (bytes[i] as number) * 58;
      bytes[i] = carry & 0xff;
      carry >>= 8;
    }
    while (carry > 0) {
      bytes.push(carry & 0xff);
      carry >>= 8;
    }
  }
  for (const char of text) {
    if (char !== '1') break;
    bytes.push(0);
  }
  return Uint8Array.from(bytes.reverse());
}

function borshString(text: string): Uint8Array {
  const body = new TextEncoder().encode(text);
  const out = new Uint8Array(4 + body.length);
  new DataView(out.buffer).setUint32(0, body.length, true);
  out.set(body, 4);
  return out;
}

export interface SignedMessageParams {
  message: string;
  recipient: string;
  nonce: Uint8Array;
}

export function nep413Preimage(params: SignedMessageParams): Uint8Array {
  if (params.nonce.length !== 32) throw new Error('the nonce must be 32 bytes');
  const parts = [
    (() => {
      const tag = new Uint8Array(4);
      new DataView(tag.buffer).setUint32(0, TAG, true);
      return tag;
    })(),
    borshString(params.message),
    params.nonce,
    borshString(params.recipient),
    Uint8Array.of(0), // callbackUrl: None
  ];
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

export type Verification = 'valid' | 'invalid' | 'unsupported';

/** `publicKey` is `ed25519:<base58>`, `signature` is base64. */
export async function verifyNep413(
  params: SignedMessageParams,
  publicKey: string,
  signature: string,
): Promise<Verification> {
  if (!publicKey.startsWith('ed25519:')) return 'invalid';
  try {
    const raw = base58Decode(publicKey.slice('ed25519:'.length));
    const hash = await crypto.subtle.digest('SHA-256', nep413Preimage(params) as BufferSource);
    let key: CryptoKey;
    try {
      key = await crypto.subtle.importKey('raw', raw as BufferSource, 'Ed25519', false, ['verify']);
    } catch {
      return 'unsupported';
    }
    const ok = await crypto.subtle.verify(
      'Ed25519',
      key,
      b64decode(signature) as BufferSource,
      hash,
    );
    return ok ? 'valid' : 'invalid';
  } catch {
    return 'invalid';
  }
}
