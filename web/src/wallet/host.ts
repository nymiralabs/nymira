// The wallet frame. NEAR Connect 0.11.4 runs here, not in the app's page, because the library
// injects a <style> element, writes innerHTML and builds a srcdoc iframe with an inline <script>
// for every wallet: none of that can run under the app's strict policy. This page is served with
// its own, relaxed policy (headers.ts) and is embedded with `sandbox="allow-scripts allow-popups"`,
// which gives it an opaque origin: it cannot read the app's IndexedDB, storage or DOM. The app
// talks to it with postMessage (client.ts).
//
// What the frame is told by the app: which wallet to connect and what to send. What it is not
// told: the network, the wallet list or any URL; those are constants of this build.
import { IDBFactory } from 'fake-indexeddb';
import { NearConnector } from '@hot-labs/near-connect';
import { PROFILE } from '../profile';
import { manifestEntry } from './manifest';
import { installPinnedFetch } from './pinned-fetch';
import type {
  ConnectResult,
  FrameMessage,
  FrameRequest,
  FunctionCallAction,
  WalletInfo,
} from './protocol';

// An opaque-origin frame has no localStorage or IndexedDB: NEAR Connect and the executors it
// hosts use both, so give them in-memory ones. A wallet session therefore lasts as long as the
// page stays open; reloading means connecting again.
function memoryStorage(): Storage {
  const data = new Map<string, string>();
  return {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (key) => data.get(key) ?? null,
    key: (index) => [...data.keys()][index] ?? null,
    removeItem: (key) => void data.delete(key),
    setItem: (key, value) => void data.set(key, String(value)),
  } as Storage;
}
const shim = (name: string, value: unknown) =>
  Object.defineProperty(window, name, { value, configurable: true, writable: true });
shim('localStorage', memoryStorage());
shim('indexedDB', new IDBFactory());

installPinnedFetch();

const send = (message: FrameMessage) => window.parent.postMessage(message, '*');

/** Pinned executors that load and hash correctly right now; the rest are reported and skipped. */
async function usableWallets() {
  const entries = [];
  const skipped: { id: string; reason: string }[] = [];
  for (const id of PROFILE.walletIds) {
    const entry = manifestEntry(id);
    try {
      // Goes through the pinned fetch: it throws when the hash differs.
      const response = await fetch(entry.executor);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      await response.arrayBuffer();
      entries.push(entry);
    } catch (error) {
      skipped.push({ id, reason: error instanceof Error ? error.message : String(error) });
    }
  }
  return { entries, skipped };
}

async function main() {
  const { entries, skipped } = await usableWallets();
  // The manifest is an object: NEAR Connect does not fetch one.
  const connector = new NearConnector({
    network: PROFILE.network,
    manifest: { version: '1.0.0', wallets: entries },
    features: { signMessage: true },
    autoConnect: false,
    footerBranding: null,
    storage: {
      get: async (key: string) => window.localStorage.getItem(key),
      set: async (key: string, value: string) => window.localStorage.setItem(key, value),
      remove: async (key: string) => window.localStorage.removeItem(key),
    },
  });
  await connector.whenManifestLoaded;

  const list = (): WalletInfo[] =>
    connector.availableWallets.map((wallet) => ({
      id: wallet.manifest.id,
      name: wallet.manifest.name,
      injected: wallet.manifest.type !== 'sandbox',
    }));
  const announce = () => send({ type: 'wallets', wallets: list(), skipped });
  connector.on('selector:walletsChanged', announce);

  // Tell the app when a wallet popup is on screen, so it can let clicks through to it.
  const popupVisible = () =>
    [...document.querySelectorAll<HTMLElement>('.hot-connector-popup')].some(
      (el) => el.style.display !== 'none',
    );
  let visible = false;
  new MutationObserver(() => {
    const now = popupVisible();
    if (now !== visible) {
      visible = now;
      send({ type: 'ui', visible });
    }
  }).observe(document.body, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ['style'],
  });

  async function connect(
    p: Extract<FrameRequest, { method: 'connect' }>['params'],
  ): Promise<ConnectResult> {
    const signed = new Promise<ConnectResult>((resolve) => {
      connector.once('wallet:signInAndSignMessage', ({ accounts }) => {
        const first = accounts[0] as
          | { accountId: string; publicKey: string; signedMessage?: ConnectResult['signedMessage'] }
          | undefined;
        resolve({
          accountId: first?.accountId ?? '',
          publicKey: first?.publicKey ?? '',
          signedMessage: first?.signedMessage ?? null,
        });
      });
    });
    await connector.connect({
      walletId: p.walletId,
      signMessageParams: {
        message: p.message,
        recipient: p.recipient,
        nonce: Uint8Array.from(p.nonce),
      },
    });
    return signed;
  }

  async function sendTransaction(receiverId: string, actions: FunctionCallAction[]) {
    const wallet = await connector.wallet();
    const outcome = await wallet.signAndSendTransaction({
      receiverId,
      actions,
      network: PROFILE.network,
    });
    return JSON.parse(JSON.stringify(outcome ?? null)) as unknown;
  }

  window.addEventListener('message', (event: MessageEvent<{ id: number } & FrameRequest>) => {
    // Only the app that embeds this frame, from the one origin this build is made for.
    if (event.source !== window.parent || event.origin !== PROFILE.appOrigin) return;
    const { id, method, params } = event.data;
    const reply = (ok: boolean, payload: unknown) =>
      send(
        ok
          ? { type: 'reply', id, ok: true, result: payload }
          : { type: 'reply', id, ok: false, error: String(payload) },
      );
    const run = async (): Promise<unknown> => {
      switch (method) {
        case 'list':
          return list();
        case 'connect':
          return connect(params);
        case 'send':
          return sendTransaction(params.receiverId, params.actions);
        case 'disconnect':
          await connector.disconnect().catch(() => undefined);
          return null;
        default:
          throw new Error('unknown method');
      }
    };
    run().then(
      (result) => reply(true, result),
      (error: unknown) =>
        reply(
          false,
          error instanceof Error
            ? error.message
            : typeof error === 'string'
              ? error
              : JSON.stringify(error),
        ),
    );
  });

  send({ type: 'ready', network: PROFILE.network });
  announce();
}

main().catch((error: unknown) => {
  send({ type: 'ready', network: 'failed' });
  console.error('wallet frame failed to start', error instanceof Error ? error.message : error);
});
