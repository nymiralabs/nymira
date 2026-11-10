import 'fake-indexeddb/auto';
import { expect, it } from 'vitest';
import { getNote, patchNote, putNote, StaleNoteError } from '../src/store/db';
import type { NoteRecord } from '../src/store/types';

const note = (sealed: string): NoteRecord =>
  ({
    id: 'n1',
    phase: 'accepted',
    kind: 'state',
    updatedAt: 1,
    sealed: { iv: 'iv', ct: sealed },
  }) as NoteRecord;

/** Marks the stored note as last written by another page (what a second tab would do). */
function writtenByAnotherTab(id: string) {
  return new Promise<void>((resolve, reject) => {
    const open = indexedDB.open('nymira');
    open.onsuccess = () => {
      const tx = open.result.transaction('meta', 'readwrite');
      tx.objectStore('meta').put('another-tab', `writer:${id}`);
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
    };
    open.onerror = () => reject(open.error);
  });
}

it('a stale tab cannot overwrite a newer sealed state written by another tab', async () => {
  const first = note('one');
  await putNote(first);
  // Another tab advances the wallet state.
  await patchNote(first, { sealed: { iv: 'iv2', ct: 'two' } }, 2);
  await writtenByAnotherTab('n1');
  // This tab still holds the first copy.
  await expect(patchNote(first, { sealed: { iv: 'iv3', ct: 'three' } }, 3)).rejects.toBeInstanceOf(
    StaleNoteError,
  );
  expect((await getNote('n1'))?.sealed.ct).toBe('two');
});

it('this page may finish its own earlier save, whatever copy it holds', async () => {
  const first = { ...note('one'), id: 'n3' };
  await putNote(first);
  await patchNote(first, { sealed: { iv: 'iv2', ct: 'two' } }, 2);
  const merged = await patchNote(first, { sealed: { iv: 'iv3', ct: 'three' } }, 3);
  expect(merged.sealed.ct).toBe('three');
});

it('a public patch from a stale copy keeps the newer sealed state', async () => {
  const first = note('one');
  await putNote({ ...first, id: 'n2' });
  const stale = { ...first, id: 'n2' };
  await patchNote(stale, { sealed: { iv: 'iv2', ct: 'two' } }, 2);
  const merged = await patchNote(stale, { phase: 'sending' }, 3);
  expect(merged.sealed.ct).toBe('two');
  expect(merged.phase).toBe('sending');
  expect((await getNote('n2'))?.sealed.ct).toBe('two');
});
