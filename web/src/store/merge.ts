import type { NoteRecord } from './types';

/**
 * Which records of a restored backup may replace what the browser already holds.
 *
 * A note's wallet state only moves forward: every chat spends some of its balance. A backup
 * made earlier holds an older state, and putting it over the newer one would let the operator
 * defeat an exit with a state the user no longer has. So a record is taken only when the browser
 * has none for that note, or holds an older one that has not started to leave; everything else
 * is skipped and counted, and the screen says so.
 */
export function mergeRestored(
  existing: readonly NoteRecord[],
  incoming: readonly NoteRecord[],
): { take: NoteRecord[]; skipped: NoteRecord[] } {
  const held = new Map(existing.map((n) => [n.id, n]));
  const take: NoteRecord[] = [];
  const skipped: NoteRecord[] = [];
  for (const record of incoming) {
    const have = held.get(record.id);
    if (have === undefined) take.push(record);
    else if (have.leave === undefined && record.updatedAt > have.updatedAt) take.push(record);
    else skipped.push(record);
  }
  return { take, skipped };
}
