// What each vendored wallet executor needs from the wallet frame's policy beyond the frame's own
// origin. The frame is sandboxed with an opaque origin and has its own header set (headers.ts), so
// these hosts are open to the wallet's code and not to the app.
//
// How this list was made: each real executor was loaded in the frame under the policy and its
// violations read from the console (2026-11-03). That shows what a wallet asks for as it loads, not
// what a full sign-in and a transaction need. Meteor was checked by hand through a transaction on
// testnet on 2026-11-05 (connectOn below); Intear still needs the same check.
export interface WalletHosts {
  /** Pages the executor embeds in a frame. */
  frame?: string[];
  /** Hosts its code connects to. */
  connect?: string[];
  /** Hosts it loads images from. */
  img?: string[];
  /** Hosts its code connects to on one network only (the RPC it reads before it signs). */
  connectOn?: Partial<Record<'testnet' | 'mainnet', string[]>>;
}

export const WALLET_HOSTS: Record<string, WalletHosts> = {
  'intear-wallet': {
    frame: ['https://wallet.intear.tech'],
    connect: ['https://wallet.intear.tech'],
  },
  'meteor-wallet': {
    connect: ['https://mc.meteorwallet.app', 'wss://mc.meteorwallet.app'],
    // Seen on testnet (2026-11-05): while it prepares a transaction Meteor reads the chain through
    // FastNEAR. Refused, it waits and never opens its approval window. The mainnet host is the one
    // its code names; it has not been tried by hand yet.
    connectOn: {
      testnet: ['https://test.rpc.fastnear.com'],
      mainnet: ['https://free.rpc.fastnear.com'],
    },
    img: ['https://meteorwallet.app'],
  },
};
