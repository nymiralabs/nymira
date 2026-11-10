// IndexedDB for notes and the wrapping metadata. Two stores:
//   meta   key -> value ('wrap': how the wrapping key is derived; never the key)
//   notes  id  -> NoteRecord (public fields plus one sealed payload)
// The database is per origin. Browsers can drop it (eviction, "clear site data", private windows),
// which is why the funding flow also produces a backup file before any transfer.
import type { Sealed } from '../crypto/aead';
import type { NoteRecord, WrapMeta } from './types';

const DB_NAME = 'nymira';
const VERSION = 1;

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains('meta')) db.createObjectStore('meta');
      if (!db.objectStoreNames.contains('notes')) db.createObjectStore('notes', { keyPath: 'id' });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error('IndexedDB is not available'));
  });
}

function run<T>(
  store: 'meta' | 'notes',
  mode: IDBTransactionMode,
  action: (s: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  return openDb().then(
    (db) =>
      new Promise<T>((resolve, reject) => {
        // Writes wait for the browser to flush to disk: the secret must be stored before the
        // transfer is handed to the wallet.
        const tx = db.transaction(store, mode, { durability: 'strict' });
        const request = action(tx.objectStore(store));
        tx.oncomplete = () => {
          db.close();
          resolve(request.result);
        };
        tx.onerror = () => {
          db.close();
          reject(tx.error ?? new Error('IndexedDB write failed'));
        };
        tx.onabort = tx.onerror;
      }),
  );
}

export const getWrapMeta = () =>
  run<WrapMeta | undefined>('meta', 'readonly', (s) => s.get('wrap'));
export const putWrapMeta = (meta: WrapMeta) => run('meta', 'readwrite', (s) => s.put(meta, 'wrap'));

/** Small public settings (the expiry guard's switches). Never a key or a secret. */
export const getSetting = <T>(name: string) =>
  run<T | undefined>('meta', 'readonly', (s) => s.get(`setting:${name}`));
export const putSetting = (name: string, value: unknown) =>
  run('meta', 'readwrite', (s) => s.put(value, `setting:${name}`));

export const putNote = (note: NoteRecord) => run('notes', 'readwrite', (s) => s.put(note));
/** Another tab or an earlier save changed this note's sealed payload since this copy was read. */
export class StaleNoteError extends Error {
  constructor() {
    super(
      'This note was changed in another tab or window. Reload this page; nothing was overwritten.',
    );
    this.name = 'StaleNoteError';
  }
}

const sameSealed = (a: Sealed, b: Sealed) => a.iv === b.iv && a.ct === b.ct;

/** Names this page load. A newer sealed payload written by the same page is its own earlier work. */
const WRITER = crypto.randomUUID();

/**
 * Applies `patch` to the note as it is stored now, in one transaction, and returns the result.
 * Public fields merge over the stored copy, so a stale copy cannot undo another save. A patch that
 * carries a new `sealed` payload is refused when another page wrote a different payload since
 * `base` was read: two tabs must never both advance the same wallet state. Writes from this page
 * itself always go through (an abandoned conversation may still finish its last save).
 */
export function patchNote(
  base: NoteRecord,
  patch: Partial<NoteRecord>,
  updatedAt: number,
): Promise<NoteRecord> {
  return openDb().then(
    (db) =>
      new Promise<NoteRecord>((resolve, reject) => {
        const tx = db.transaction(['notes', 'meta'], 'readwrite', { durability: 'strict' });
        const store = tx.objectStore('notes');
        const meta = tx.objectStore('meta');
        let result: NoteRecord | undefined;
        let failure: unknown;
        const read = store.get(base.id);
        const who = meta.get(`writer:${base.id}`);
        read.onsuccess = () => {
          const stored = read.result as NoteRecord | undefined;
          who.onsuccess = () => {
            if (
              stored &&
              patch.sealed &&
              !sameSealed(stored.sealed, base.sealed) &&
              who.result !== WRITER
            ) {
              failure = new StaleNoteError();
              tx.abort();
              return;
            }
            result = { ...(stored ?? base), ...patch, updatedAt };
            store.put(result);
            if (patch.sealed) meta.put(WRITER, `writer:${base.id}`);
          };
        };
        tx.oncomplete = () => {
          db.close();
          resolve(result as NoteRecord);
        };
        tx.onabort = tx.onerror = () => {
          db.close();
          reject(failure ?? tx.error ?? new Error('IndexedDB write failed'));
        };
      }),
  );
}

export const deleteNote = (id: string) => run('notes', 'readwrite', (s) => s.delete(id));
export const getNote = (id: string) =>
  run<NoteRecord | undefined>('notes', 'readonly', (s) => s.get(id));
export const listNotes = () => run<NoteRecord[]>('notes', 'readonly', (s) => s.getAll());
