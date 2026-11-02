// AES-256-GCM through WebCrypto. Keys are CryptoKeys; the ones the app keeps are not extractable.
import { b64decode, b64encode } from './bytes';

export interface Sealed {
  iv: string;
  ct: string;
}

const utf8 = (text: string) => new TextEncoder().encode(text);

/** A non-extractable AES-GCM key from raw bytes. */
export function importAesKey(raw: Uint8Array): Promise<CryptoKey> {
  return crypto.subtle.importKey('raw', raw as BufferSource, 'AES-GCM', false, [
    'encrypt',
    'decrypt',
  ]);
}

export async function seal(key: CryptoKey, plain: Uint8Array, aad: string): Promise<Sealed> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv, additionalData: utf8(aad) },
    key,
    plain as BufferSource,
  );
  return { iv: b64encode(iv), ct: b64encode(new Uint8Array(ct)) };
}

/** Throws when the key, the data or the bound context is wrong. */
export async function open(key: CryptoKey, sealed: Sealed, aad: string): Promise<Uint8Array> {
  const plain = await crypto.subtle.decrypt(
    { name: 'AES-GCM', iv: b64decode(sealed.iv) as BufferSource, additionalData: utf8(aad) },
    key,
    b64decode(sealed.ct) as BufferSource,
  );
  return new Uint8Array(plain);
}

export const sealJson = (key: CryptoKey, value: unknown, aad: string) =>
  seal(key, utf8(JSON.stringify(value)), aad);

export async function openJson<T>(key: CryptoKey, sealed: Sealed, aad: string): Promise<T> {
  return JSON.parse(new TextDecoder().decode(await open(key, sealed, aad))) as T;
}
