// When the app asks for a backup after a lease ends (web spec stories 18 and 19). A lost latest
// state lets the operator defeat the user's exit and can cost the whole remaining balance, so the
// backup must not fall far behind: after every retire while the balance is above 0.50 USDC, after
// every tenth lease below that, and always once the balance is spent down to nothing.

/** 0.50 USDC in base units. */
export const BACKUP_EVERY_BALANCE = 500_000n;
export const BACKUP_EVERY_N_BELOW = 10;

/** `leases` counts the finished leases of the note, including the one that just ended. */
export function backupDue(balance: bigint, leases: number): boolean {
  if (balance > BACKUP_EVERY_BALANCE) return true;
  return balance === 0n || leases % BACKUP_EVERY_N_BELOW === 0;
}
