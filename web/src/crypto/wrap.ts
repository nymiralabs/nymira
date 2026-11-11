// The wrapping key: a non-extractable AES-GCM key that seals every note secret before it reaches
// IndexedDB. Two ways to get it:
//   passphrase     Argon2id in the note worker (same parameters as backups)
//   webauthn-prf   the PRF extension of a WebAuthn credential, through HKDF
// The key lives in memory for the session; a tab can opt in to keeping it across reloads. Only
// how to derive it (salt, parameters or credential id) and a check value are stored.
import { open, seal } from './aead';
import { b64decode, b64encode } from './bytes';
import { ARGON2, SALT_BYTES, checkPassphrase } from './kdf';
import { getWrapMeta, putWrapMeta } from '../store/db';
import { WRAP_AAD, WRAP_CHECK_TEXT, type WrapMeta } from '../store/types';
import { callWorker } from '../worker/client';

let sessionKey: CryptoKey | null = null;

// "Stay unlocked in this tab" is opt-in. When it is on, the non-extractable key is also kept in a
// second IndexedDB next to an id that sits in sessionStorage. A reload of the same tab finds both
// and stays unlocked; closing the tab drops sessionStorage, so the stored key is deleted and never
// opens again. The key never leaves the browser's key store. `vite dev` with the mock profile
// keeps it by default, because every saved edit reloads the page.
const TAB_MARK = 'nymira.tab-key';
const keepSupported = typeof indexedDB !== 'undefined' && typeof sessionStorage !== 'undefined';
const devKeeps = import.meta.env.DEV && import.meta.env.MODE === 'mock';
let keepOn = keepSupported && devKeeps;

/** Chooses whether the next unlock is kept for this tab. */
export function keepUnlockedInTab(on: boolean) {
  keepOn = keepSupported && (on || devKeeps);
}
/** True when this tab keeps the key across reloads. */
export const keepsUnlocked = () => keepOn && sessionKey !== null;

function tabMark(): string | null {
  try {
    return sessionStorage.getItem(TAB_MARK);
  } catch {
    return null;
  }
}
function tabStore(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open('nymira-tab-key', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('k');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}
type Kept = { id: string; key: CryptoKey };
async function tabRead(): Promise<Kept | null> {
  const db = await tabStore();
  return new Promise((resolve) => {
    const get = db.transaction('k').objectStore('k').get('wrap');
    get.onsuccess = () => {
      db.close();
      resolve((get.result as Kept | undefined) ?? null);
    };
    get.onerror = () => {
      db.close();
      resolve(null);
    };
  });
}
async function tabWrite(kept: Kept | null): Promise<void> {
  try {
    const db = await tabStore();
    const store = db.transaction('k', 'readwrite').objectStore('k');
    if (kept) store.put(kept, 'wrap');
    else store.delete('wrap');
    db.close();
  } catch {
    // Not kept: the next reload asks for the passphrase.
  }
}
function keepKey(key: CryptoKey | null) {
  if (!keepSupported) return;
  if (!key) {
    try {
      sessionStorage.removeItem(TAB_MARK);
    } catch {
      // Nothing to remove.
    }
    keepOn = false;
    void tabWrite(null);
    return;
  }
  if (!keepOn) return;
  const id = tabMark() ?? crypto.randomUUID();
  try {
    sessionStorage.setItem(TAB_MARK, id);
  } catch {
    return;
  }
  void tabWrite({ id, key });
}
if (keepSupported) {
  const kept = await tabRead().catch(() => null);
  if (kept && kept.id === tabMark()) {
    sessionKey = kept.key;
    keepOn = true;
  } else if (kept) {
    // From a tab that is gone: it never opens again.
    await tabWrite(null);
  }
}

const keyListeners = new Set<() => void>();
const setKey = (key: CryptoKey | null) => {
  sessionKey = key;
  keepKey(key);
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
// The passphrase typed to create or open this session's key, kept in memory only so the backup
// step can offer it again instead of asking a second time. Dropped on lock and on a reload.
let typedPassphrase: string | null = null;
export const typedUnlockPassphrase = () => typedPassphrase;
export function forgetTypedPassphrase() {
  typedPassphrase = null;
}
export function lock() {
  typedPassphrase = null;
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
  typedPassphrase = passphrase;
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
  typedPassphrase = passphrase;
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
