// Puts a `NearTestWallet` in every page of a Playwright browser context. The
// key stays in the test runner; the page gets a wallet object whose methods
// call back into it. The page announces it the way NEAR Connect expects an
// injected wallet (`near-wallet-injected`), and also as `window.nymiraTestWallet`.
//
// Call it before the first navigation: init scripts run on the next page load.

const METHODS = [
  'signIn',
  'signInAndSignMessage',
  'signOut',
  'getAccounts',
  'signMessage',
  'signAndSendTransaction',
  'signAndSendTransactions',
  'signDelegateActions',
];

// Uint8Array is not JSON; it crosses as { $u8: [...] }. The init script below
// has the same pair, since it cannot import this file.
const encode = (value) =>
  JSON.stringify(value ?? null, (_key, v) => (v instanceof Uint8Array ? { $u8: [...v] } : v));
const decode = (text) =>
  JSON.parse(text, (_key, v) => (v && typeof v === 'object' && '$u8' in v ? Uint8Array.from(v.$u8) : v));

/** @param {import('@playwright/test').BrowserContext | import('@playwright/test').Page} target */
export async function installTestWallet(target, wallet) {
  const context = typeof target.context === 'function' ? target.context() : target;
  await context.exposeFunction('__nymiraWalletCall', async (method, params) => {
    if (!METHODS.includes(method)) throw new Error(`test wallet: no method ${method}`);
    return encode(await wallet[method](decode(params)));
  });
  await context.addInitScript(
    ({ manifest, methods }) => {
      const encode = (value) =>
        JSON.stringify(value ?? null, (_key, v) => (v instanceof Uint8Array ? { $u8: [...v] } : v));
      const decode = (text) =>
        JSON.parse(text, (_key, v) => (v && typeof v === 'object' && '$u8' in v ? Uint8Array.from(v.$u8) : v));
      const testWallet = { manifest };
      for (const method of methods) {
        testWallet[method] = async (params) =>
          decode(await window.__nymiraWalletCall(method, encode(params)));
      }
      window.nymiraTestWallet = testWallet;
      const announce = () =>
        window.dispatchEvent(new CustomEvent('near-wallet-injected', { detail: testWallet }));
      window.addEventListener('near-selector-ready', announce);
      announce();
    },
    { manifest: wallet.manifest, methods: METHODS },
  );
}
