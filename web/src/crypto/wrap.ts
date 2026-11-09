// The wrapping key: a non-extractable AES-GCM key that seals every note secret before it reaches
// IndexedDB. Two ways to get it:
//   passphrase     Argon2id in the note worker (same parameters as backups)
//   webauthn-prf   the PRF extension of a WebAuthn credential, through HKDF
// The key lives in memory for the session only. Only how to derive it (salt, parameters or
// credential id) and a check value are stored.
import { open, seal } from './aead';
import { b64decode, b64encode } from './bytes';
import { ARGON2, SALT_BYTES, checkPassphrase } from './kdf';
import { getWrapMeta, putWrapMeta } from '../store/db';
import { WRAP_AAD, WRAP_CHECK_TEXT, type WrapMeta } from '../store/types';
import { callWorker } from '../worker/client';

let sessionKey: CryptoKey | null = null;

const keyListeners = new Set<() => void>();
const setKey = (key: CryptoKey | null) => {
  sessionKey = key;
  keyListeners.forEach((l) => l());
};
/** For the app shell, which shows whether the notes are unlocked. */
export const subscribeUnlocked = (l: () => void) => {
  keyListeners.add(l);
  return () => void keyListeners.delete(l);
};

export const isUnlocked = () => sessionKey !== null;
export function requireKey(): CryptoKey {
  if (!sessionKey) throw new Error('the note store is locked');
  return sessionKey;
}
export function lock() {
  setKey(null);
}

export async function wrapMethod(): Promise<WrapMeta['method'] | null> {
  return (await getWrapMeta())?.method ?? null;
}

async function checkValue(key: CryptoKey) {
  return seal(key, new TextEncoder().encode(WRAP_CHECK_TEXT), WRAP_AAD);
}

async function verify(key: CryptoKey, meta: WrapMeta): Promise<boolean> {
  try {
    return new TextDecoder().decode(await open(key, meta.check, WRAP_AAD)) === WRAP_CHECK_TEXT;
  } catch {
    return false;
  }
}

export async function createPassphraseWrap(passphrase: string): Promise<void> {
  checkPassphrase(passphrase);
  const salt = b64encode(crypto.getRandomValues(new Uint8Array(SALT_BYTES)));
  const key = await callWorker({ op: 'deriveKey', passphrase, salt, params: ARGON2 });
  await putWrapMeta({
    v: 1,
    method: 'passphrase',
    kdf: { ...ARGON2, salt },
    check: await checkValue(key),
  });
  setKey(key);
}

export async function unlockWithPassphrase(passphrase: string): Promise<boolean> {
  const meta = await getWrapMeta();
  if (meta?.method !== 'passphrase') throw new Error('this browser does not use a passphrase');
  const key = await callWorker({
    op: 'deriveKey',
    passphrase,
    salt: meta.kdf.salt,
    params: meta.kdf,
  });
  if (!(await verify(key, meta))) return false;
  setKey(key);
  return true;
}

// ---- WebAuthn PRF ----

export function prfPossible(): boolean {
  return typeof PublicKeyCredential !== 'undefined' && !!navigator.credentials;
}

const utf8 = (s: string) => new TextEncoder().encode(s);

async function keyFromPrf(output: ArrayBuffer): Promise<CryptoKey> {
  const hkdf = await crypto.subtle.importKey('raw', output, 'HKDF', false, ['deriveKey']);
  return crypto.subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(32), info: utf8('nymira wrap key v1') },
    hkdf,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt'],
  );
}

type PrfResults = { prf?: { enabled?: boolean; results?: { first?: ArrayBuffer } } };

async function prfAssert(credentialId: Uint8Array, salt: Uint8Array): Promise<ArrayBuffer> {
  const assertion = (await navigator.credentials.get({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      allowCredentials: [{ type: 'public-key', id: credentialId as BufferSource }],
      userVerification: 'required',
      extensions: {
        prf: { eval: { first: salt as BufferSource } },
      } as AuthenticationExtensionsClientInputs,
    },
  })) as PublicKeyCredential | null;
  const first = (assertion?.getClientExtensionResults() as PrfResults | undefined)?.prf?.results
    ?.first;
  if (!first) throw new Error('this authenticator did not return a PRF value');
  return first;
}

/** Creates a credential and a wrapping key from its PRF output. Throws when PRF is unsupported. */
export async function createPrfWrap(): Promise<void> {
  if (!prfPossible()) throw new Error('WebAuthn is not available in this browser');
  const prfSalt = crypto.getRandomValues(new Uint8Array(32));
  const created = (await navigator.credentials.create({
    publicKey: {
      rp: { name: 'Nymira' },
      user: {
        id: crypto.getRandomValues(new Uint8Array(16)),
        name: 'nymira-note-key',
        displayName: 'Nymira note key',
      },
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      pubKeyCredParams: [
        { type: 'public-key', alg: -7 },
        { type: 'public-key', alg: -257 },
      ],
      authenticatorSelection: { residentKey: 'preferred', userVerification: 'required' },
      extensions: { prf: { eval: { first: prfSalt } } } as AuthenticationExtensionsClientInputs,
    },
  })) as PublicKeyCredential | null;
  if (!created) throw new Error('no credential was created');
  const results = (created.getClientExtensionResults() as PrfResults).prf;
  if (!results?.enabled && !results?.results?.first) {
    throw new Error('this authenticator or browser does not support the WebAuthn PRF extension');
  }
  // Some authenticators return the PRF value only on a later assertion.
  const output =
    results.results?.first ?? (await prfAssert(new Uint8Array(created.rawId), prfSalt));
  const key = await keyFromPrf(output);
  await putWrapMeta({
    v: 1,
    method: 'webauthn-prf',
    credentialId: b64encode(new Uint8Array(created.rawId)),
    prfSalt: b64encode(prfSalt),
    check: await checkValue(key),
  });
  setKey(key);
}

export async function unlockWithPrf(): Promise<boolean> {
  const meta = await getWrapMeta();
  if (meta?.method !== 'webauthn-prf') throw new Error('this browser does not use a passkey');
  const output = await prfAssert(b64decode(meta.credentialId), b64decode(meta.prfSalt));
  const key = await keyFromPrf(output);
  if (!(await verify(key, meta))) return false;
  setKey(key);
  return true;
}
