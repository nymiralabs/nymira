// Every path that retires a lease asks for a backup, including the ones that leave the Chat screen.
import { describe, expect, it } from 'vitest';
import { retireBackup } from '../src/chat/retire-backup';

describe('the backup prompt after a retire', () => {
  it('stays set until the backup is made, for any screen that listens', () => {
    retireBackup.clear();
    let seen = 0;
    const off = retireBackup.subscribe(() => seen++);
    // Leaving the Chat screen retires the lease without knowing the balance: the prompt is due.
    retireBackup.set({ needed: true, leases: 0 });
    expect(retireBackup.state).toEqual({ needed: true, leases: 0 });
    retireBackup.clear();
    expect(retireBackup.state).toBeNull();
    expect(seen).toBe(2);
    off();
  });
});
