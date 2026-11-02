// What each vendored wallet executor needs from the wallet frame's policy beyond the frame's own
// origin. The frame is sandboxed with an opaque origin and has its own header set (headers.ts), so
// these hosts are open to the wallet's code and not to the app.
//
// How this list was made: each real executor was loaded in the frame under the policy and its
// violations read from the console (2026-11-03). That shows what a wallet asks for as it loads, not
// what a full sign-in and a transaction need. Check Meteor and Intear by hand on testnet and extend
// the list from the violations (the checklist is in the pull request).
export interface WalletHosts {
  /** Pages the executor embeds in a frame. */
  frame?: string[];
  /** Hosts its code connects to. */
  connect?: string[];
  /** Hosts it loads images from. */
  img?: string[];
}

export const WALLET_HOSTS: Record<string, WalletHosts> = {
  'intear-wallet': {
    frame: ['https://wallet.intear.tech'],
    connect: ['https://wallet.intear.tech'],
  },
  'meteor-wallet': {
    connect: ['https://mc.meteorwallet.app', 'wss://mc.meteorwallet.app'],
    img: ['https://meteorwallet.app'],
  },
};
