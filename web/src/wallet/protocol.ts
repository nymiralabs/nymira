// Messages between the app and its wallet frame (wallet-host.html). The frame is sandboxed with an
// opaque origin; the app accepts a message only from that frame's window and treats every value in
// it as untrusted input.

export interface WalletInfo {
  id: string;
  name: string;
  /** Announced by a browser extension rather than one of the pinned executors. */
  injected: boolean;
}

export interface SignedMessage {
  accountId: string;
  publicKey: string;
  /** base64, NEP-413 */
  signature: string;
}

export interface ConnectResult {
  accountId: string;
  publicKey: string;
  signedMessage: SignedMessage | null;
}

export interface FunctionCallAction {
  type: 'FunctionCall';
  params: { methodName: string; args: unknown; gas: string; deposit: string };
}

export type FrameRequest =
  | { method: 'list'; params: Record<string, never> }
  | {
      method: 'connect';
      params: { walletId: string; message: string; recipient: string; nonce: number[] };
    }
  | { method: 'send'; params: { receiverId: string; actions: FunctionCallAction[] } }
  | { method: 'disconnect'; params: Record<string, never> };

export type FrameMessage =
  | { type: 'ready'; network: string }
  | { type: 'wallets'; wallets: WalletInfo[]; skipped: { id: string; reason: string }[] }
  | { type: 'ui'; visible: boolean }
  | { type: 'reply'; id: number; ok: true; result: unknown }
  | { type: 'reply'; id: number; ok: false; error: string };
