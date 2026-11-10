// Mock build only (`vite --mode mock`): adds accepted sample notes to the unlocked store so the
// Notes screen can be looked at without a chain. Nothing is sent anywhere.
import { acceptFound, prepareNote } from '../funding/flow';
import { PROFILE } from '../profile';

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
  ];
  for (const s of samples) {
    const record = await prepareNote({
      denomination: s.denomination,
      payout: 'alice.testnet',
      sender: 'alice.testnet',
    });
    await acceptFound(record, {
      noteId: s.noteId,
      denomination: s.denomination,
      expiry: now + s.days * DAY,
    });
  }
}
