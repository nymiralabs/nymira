// IndexedDB for notes and the wrapping metadata. Two stores:
//   meta   key -> value ('wrap': how the wrapping key is derived; never the key)
//   notes  id  -> NoteRecord (public fields plus one sealed payload)
// The database is per origin. Browsers can drop it (eviction, "clear site data", private windows),
// which is why the funding flow also produces a backup file before any transfer.
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
export const getNote = (id: string) =>
  run<NoteRecord | undefined>('notes', 'readonly', (s) => s.get(id));
export const listNotes = () => run<NoteRecord[]>('notes', 'readonly', (s) => s.getAll());
