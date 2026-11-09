// What the app tells a person who picks a wallet that is known to misbehave, before it asks the
// wallet. The wording is plain on purpose: whose problem it is, what it looks like, what to do.
export interface WalletNote {
  text: string;
  /** The report on the wallet's own tracker. */
  issue: string;
}

export const WALLET_NOTES: Record<string, WalletNote> = {
  'intear-wallet': {
    text: 'Intear’s approval window may not report back to this page after you approve. This is a problem in Intear Wallet, not in Nymira: its window answers an origin that a sealed frame cannot receive. If this page times out, look the transaction up in Notes by its hash.',
    issue: 'https://github.com/INTEARnear/wallet/issues/23',
  },
};

/** The order wallets are listed in: the fake test wallet, Meteor, others, then Intear. */
export function walletRank(id: string): number {
  if (id === 'fake-wallet') return -1;
  if (id === 'meteor-wallet') return 0;
  if (id === 'intear-wallet') return 2;
  return 1;
}
