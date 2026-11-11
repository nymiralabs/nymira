// Mock build only (`vite --mode mock`): adds accepted sample notes to the unlocked store so the
// Notes screen can be looked at without a chain. Nothing is sent anywhere.
import { acceptFound, prepareNote } from '../funding/flow';
import { PROFILE } from '../profile';
import { putNote } from '../store/db';
import type { LeaveInfo } from '../store/types';

const DAY = 86_400;

export async function seedSampleNotes() {
  const pins = PROFILE.pins;
  if (!pins) throw new Error('this build has no vault pins');
  const now = Math.floor(Date.now() / 1000);
  const small = pins.config.denominations[0] ?? '1000000';
  const large = pins.config.denominations[1] ?? small;
  const samples = [
    { noteId: 41, denomination: small, days: 29 },
    { noteId: 27, denomination: large, days: 24 },
    { noteId: 12, denomination: small, days: 3 },
    // On their way out: the Notes list and the sidebar must lead these to the Close screen.
    {
      noteId: 19,
      denomination: small,
      days: 20,
      leave: { state: 'closing', via: 'wallet', at: now },
    },
    {
      noteId: 18,
      denomination: small,
      days: 18,
      leave: { state: 'exiting', deadline: now + 7 * DAY, at: now },
    },
  ] as { noteId: number; denomination: string; days: number; leave?: LeaveInfo }[];
  for (const s of samples) {
    const record = await prepareNote({
      denomination: s.denomination,
      payout: 'alice.testnet',
      sender: 'alice.testnet',
    });
    const done = await acceptFound(record, {
      noteId: s.noteId,
      denomination: s.denomination,
      expiry: now + s.days * DAY,
    });
    if (s.leave && done.status === 'accepted') await putNote({ ...done.record, leave: s.leave });
  }
}
