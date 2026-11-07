// The app's side of the wallet frame: creates the sandboxed iframe, sends it requests and checks
// what comes back. The frame is the only place NEAR Connect runs (host.ts explains why).
import { PROFILE } from '../profile';
import { verifyNep413 } from './nep413';
import type {
  ConnectResult,
  FrameMessage,
  FrameRequest,
  FunctionCallAction,
  SignedMessage,
  WalletInfo,
} from './protocol';

export interface ConnectedWallet {
  accountId: string;
  publicKey: string;
  walletId: string;
  /** What the NEP-413 check said about the signed sign-in message. */
  signature: 'valid' | 'unsupported';
}

export interface SignedByWallet extends SignedMessage {
  nonce: Uint8Array;
  message: string;
  recipient: string;
}

const SIGN_IN_MESSAGE = 'Connect to Nymira. This signature only shows that you hold this account.';

type Listener = () => void;

// Which wallet the user last connected, for the "connect again" hint after a reload. sessionStorage
// (this tab only, gone when it closes) and no account id: the hint needs nothing more.
const LAST_WALLET = 'nymira.lastWallet';
const rememberedWallet = (): string | null => {
  try {
    return sessionStorage.getItem(LAST_WALLET);
  } catch {
    return null;
  }
};
const remember = (walletId: string | null) => {
  try {
    if (walletId) sessionStorage.setItem(LAST_WALLET, walletId);
    else sessionStorage.removeItem(LAST_WALLET);
  } catch {
    // No storage: the hint is lost, nothing else.
  }
};

export class WalletClient {
  private frame: HTMLIFrameElement | null = null;
  private ready: Promise<void> | null = null;
  private counter = 0;
  private readonly pending = new Map<
    number,
    { ok: (v: unknown) => void; fail: (e: Error) => void }
  >();
  private readonly listeners = new Set<Listener>();
  wallets: WalletInfo[] = [];
  skipped: { id: string; reason: string }[] = [];
  connected: ConnectedWallet | null = null;
  /**
   * The wallet connected before this page loaded, when it is not connected now. A wallet session
   * lives in the wallet frame's memory (host.ts) and does not survive a reload.
   */
  lostWallet: string | null = rememberedWallet();
  /** Changes whenever anything a screen shows changes; the snapshot for useSyncExternalStore. */
  version = 0;

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private emit() {
    this.version += 1;
    for (const l of this.listeners) l();
  }

  /** Creates the frame (once) and waits until it has built its wallet list. */
  start(): Promise<void> {
    if (this.ready) return this.ready;
    this.ready = new Promise<void>((resolve) => {
      const frame = document.createElement('iframe');
      frame.className = 'wallet-frame';
      frame.title = 'Wallet connection';
      // No allow-same-origin: the frame gets an opaque origin and cannot reach the app's storage.
      frame.setAttribute('sandbox', 'allow-scripts allow-popups allow-popups-to-escape-sandbox');
      frame.setAttribute('referrerpolicy', 'no-referrer');
      window.addEventListener('message', (event: MessageEvent<FrameMessage>) => {
        if (event.source !== frame.contentWindow) return;
        const message = event.data;
        if (message.type === 'ready') {
          if (message.network !== PROFILE.network) {
            console.error('the wallet frame reports a different network; ignoring it');
          }
          resolve();
        } else if (message.type === 'wallets') {
          this.wallets = message.wallets;
          this.skipped = message.skipped;
          this.emit();
        } else if (message.type === 'ui') {
          frame.classList.toggle('active', message.visible);
        } else if (message.type === 'reply') {
          const entry = this.pending.get(message.id);
          if (!entry) return;
          this.pending.delete(message.id);
          if (message.ok) entry.ok(message.result);
          else entry.fail(new Error(message.error));
        }
      });
      frame.src = '/wallet-host.html';
      document.body.append(frame);
      this.frame = frame;
    });
    return this.ready;
  }

  private request<T>(request: FrameRequest): Promise<T> {
    const frame = this.frame;
    if (!frame?.contentWindow) return Promise.reject(new Error('the wallet frame is not running'));
    const id = ++this.counter;
    return new Promise<T>((ok, fail) => {
      this.pending.set(id, { ok: ok as (v: unknown) => void, fail });
      frame.contentWindow?.postMessage({ id, ...request }, '*');
    });
  }

  async connect(walletId: string): Promise<ConnectedWallet> {
    await this.start();
    const nonce = [...crypto.getRandomValues(new Uint8Array(32))];
    const recipient = new URL(PROFILE.appOrigin).host;
    const result = await this.request<ConnectResult>({
      method: 'connect',
      params: { walletId, message: SIGN_IN_MESSAGE, recipient, nonce },
    });
    if (!isAccountId(result.accountId) || !result.publicKey.startsWith('ed25519:')) {
      throw new Error(
        `the wallet returned an account the app cannot use (account "${result.accountId}", key "${String(result.publicKey).slice(0, 12)}")`,
      );
    }
    const signed = result.signedMessage;
    if (!signed || signed.accountId !== result.accountId || signed.publicKey !== result.publicKey) {
      throw new Error('the wallet did not sign the sign-in message');
    }
    const verdict = await verifyNep413(
      { message: SIGN_IN_MESSAGE, recipient, nonce: Uint8Array.from(nonce) },
      signed.publicKey,
      signed.signature,
    );
    if (verdict === 'invalid') {
      await this.disconnect().catch(() => undefined);
      throw new Error('the wallet signature on the sign-in message does not verify');
    }
    remember(walletId);
    this.lostWallet = null;
    this.connected = {
      accountId: result.accountId,
      publicKey: result.publicKey,
      walletId,
      signature: verdict,
    };
    this.emit();
    return this.connected;
  }

  /**
   * Has the connected wallet sign one NEP-413 message. The signature is checked here, so a wallet
   * that returns another account's or a wrong signature is refused before the page shows a file.
   */
  async signMessage(message: string, recipient: string): Promise<SignedByWallet> {
    const connected = this.connected;
    if (!connected) throw new Error('connect a wallet first');
    const nonce = crypto.getRandomValues(new Uint8Array(32));
    const signed = await this.request<SignedMessage>({
      method: 'signMessage',
      params: { message, recipient, nonce: [...nonce] },
    });
    if (signed?.accountId !== connected.accountId || !signed.publicKey?.startsWith('ed25519:')) {
      throw new Error('the wallet signed for another account');
    }
    const verdict = await verifyNep413(
      { message, recipient, nonce },
      signed.publicKey,
      signed.signature,
    );
    if (verdict === 'invalid') throw new Error('the wallet signature does not verify');
    return { ...signed, nonce, message, recipient };
  }

  /** Sends one `ft_transfer_call`-style function call; returns the wallet's outcome object. */
  send(receiverId: string, action: FunctionCallAction): Promise<unknown> {
    return this.request({ method: 'send', params: { receiverId, actions: [action] } });
  }

  async disconnect(): Promise<void> {
    await this.request({ method: 'disconnect', params: {} });
    remember(null);
    this.lostWallet = null;
    this.connected = null;
    this.emit();
  }
}

/** NEAR account ids: 2 to 64 characters of lowercase letters, digits and `-_.`, or 64 hex digits. */
export function isAccountId(value: string): boolean {
  return (
    value.length >= 2 &&
    value.length <= 64 &&
    /^(([a-z\d]+[-_])*[a-z\d]+\.)*([a-z\d]+[-_])*[a-z\d]+$/.test(value)
  );
}

export const walletClient = new WalletClient();
