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
      signMessage: wallet.manifest.features?.signMessage === true,
    }));
  const announce = () => send({ type: 'wallets', wallets: list(), skipped });
  connector.on('selector:walletsChanged', announce);

  // Tell the app when a wallet popup is on screen, so it can let clicks through to it.
  const popupVisible = () =>
    [...document.querySelectorAll<HTMLElement>('.hot-connector-popup')].some(
      (el) => el.style.display !== 'none',
    );
  let visible = false;
  // True while the frame asks for the click that opens a late wallet window (askForClick).
  let asking = false;
  const syncUi = () => {
    const now = popupVisible() || asking;
    if (now !== visible) {
      visible = now;
      send({ type: 'ui', visible });
    }
  };
  new MutationObserver(syncUi).observe(document.body, {
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
          // Some wallets (Meteor) return the key only inside the signed message.
          publicKey: first?.publicKey || first?.signedMessage?.publicKey || '',
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

  /**
   * Shows a button in this frame and resolves true when it is clicked, false on Cancel. The click
   * is the user activation the wallet's `window.open` needs, so the wallet is called straight from
   * it: nothing slow may sit between the click and `signAndSendTransaction`.
   */
  function askForClick(walletName: string): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      // Dark by default, light when the system is: the app's own two palettes, square corners.
      const light = matchMedia('(prefers-color-scheme: light)').matches;
      const ink = light ? '#111' : '#fafafa';
      const paper = light ? '#fff' : '#0a0a0a';
      const line = light ? '#d4d4d4' : '#262626';
      const box = document.createElement('div');
      box.style.cssText =
        'position:fixed;inset:0;display:flex;align-items:center;justify-content:center;background:rgba(0,0,0,.6);font:15px/22px system-ui,sans-serif';
      const card = document.createElement('div');
      card.style.cssText = `background:${paper};color:${ink};border:1px solid ${line};padding:24px;max-width:380px;width:calc(100vw - 32px)`;
      const heading = document.createElement('p');
      heading.style.cssText = 'margin:0 0 6px;font-size:17px';
      heading.textContent = `Approve in ${walletName}`;
      const text = document.createElement('p');
      text.style.cssText = `margin:0 0 20px;color:${light ? '#525252' : '#a1a1a1'}`;
      text.textContent = 'Your request is ready. Continue to open the wallet and confirm it.';
      const row = document.createElement('div');
      row.style.cssText = 'display:flex;justify-content:flex-end;gap:8px';
      const open = document.createElement('button');
      open.type = 'button';
      open.dataset.testid = 'approve';
      open.textContent = 'Continue';
      const cancel = document.createElement('button');
      cancel.type = 'button';
      cancel.dataset.testid = 'cancel';
      cancel.textContent = 'Cancel';
      const base = 'padding:9px 16px;font:inherit;border-radius:0;cursor:pointer';
      open.style.cssText = `${base};background:${ink};color:${paper};border:1px solid ${ink}`;
      cancel.style.cssText = `${base};background:transparent;color:${ink};border:1px solid ${line}`;
      const done = (answer: boolean) => {
        box.remove();
        asking = false;
        syncUi();
        // The app keeps the frame on top only while this says so: say it again after the dust
        // settles, so a state that drifted cannot leave an invisible layer over the page.
        setTimeout(() => {
          visible = popupVisible() || asking;
          send({ type: 'ui', visible });
        }, 250);
        resolve(answer);
      };
      open.addEventListener('click', () => done(true));
      cancel.addEventListener('click', () => done(false));
      row.append(cancel, open);
      card.append(heading, text, row);
      box.append(card);
      document.body.append(box);
      asking = true;
      syncUi();
    });
  }

  async function signMessage(p: Extract<FrameRequest, { method: 'signMessage' }>['params']) {
    const wallet = await connector.wallet();
    if (PROFILE.clickBeforeSend.includes(wallet.manifest.id)) {
      if (!(await askForClick(wallet.manifest.name)))
        throw new Error('cancelled in the wallet frame');
    }
    const signed = await wallet.signMessage({
      message: p.message,
      recipient: p.recipient,
      nonce: Uint8Array.from(p.nonce),
      network: PROFILE.network,
    });
    return JSON.parse(JSON.stringify(signed)) as unknown;
  }

  async function sendTransaction(receiverId: string, actions: FunctionCallAction[]) {
    // Resolved before the user is asked, so the click only has to start the wallet.
    const wallet = await connector.wallet();
    if (PROFILE.clickBeforeSend.includes(wallet.manifest.id)) {
      if (!(await askForClick(wallet.manifest.name)))
        throw new Error('cancelled in the wallet frame');
    }
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
        case 'signMessage':
          return signMessage(params);
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
