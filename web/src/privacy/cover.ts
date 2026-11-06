// How much cover a lease had: the time since the deposit and the deposits that joined the tree in
// between, taken from leaf counts, with the team removed to count external deposits.
import { teamOf } from './setsize';
import { SMALL_EXTERNAL_SET, WARN_EXTERNAL_DEPOSITS, WARN_GAP_SECS } from './text';

export interface Cover {
  /** Seconds from the deposit to the lease. */
  gapSecs: number;
  /** Notes with a higher ID than this one and an ID below the leaf count at the lease. */
  between: number;
  /** The same, without the team's notes. */
  externalBetween: number;
}

export function coverOf(
  noteId: number,
  depositedAt: number,
  lease: { openedAt: number; leafCount: number },
  team: Set<number>,
): Cover {
  let between = 0;
  let externalBetween = 0;
  for (let id = noteId + 1; id < lease.leafCount; id++) {
    between++;
    if (!team.has(id)) externalBetween++;
  }
  return { gapSecs: Math.max(0, lease.openedAt - depositedAt), between, externalBetween };
}

/** The team a lease record removed: the pinned list plus the operator's list it stored. */
export const teamFor = (pinned: number[], operator: number[] | null) => teamOf(pinned, operator);

export const thin = (c: Cover): boolean =>
  c.gapSecs < WARN_GAP_SECS || c.externalBetween < WARN_EXTERNAL_DEPOSITS;

export const small = (external: number): boolean => external < SMALL_EXTERNAL_SET;
