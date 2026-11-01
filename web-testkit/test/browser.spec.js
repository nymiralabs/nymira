// Self-test in a real browser: the test wallet signs through a page without an
// extension, and the codec WASM loaded in the page gives the Rust vectors.
// Needs `build-codec.sh` and `npx playwright install chromium` first.

import { expect, test } from '@playwright/test';
import { CODEC_ORIGIN, loadCodecInPage, serveCodec } from '../src/codec.js';
import { installTestWallet } from '../src/playwright.js';
import { NearTestWallet } from '../src/wallet.js';
import { decodeAndVerify, startRpcStub } from './rpc-stub.js';
import { compute, expected, vectors } from './vectors.js';

let rpc;
test.beforeAll(async () => {
  rpc = await startRpcStub({ nonce: 9 });
});
test.afterAll(() => rpc.close());

test('the page signs a transaction through the injected wallet', async ({ context, page }) => {
  const wallet = new NearTestWallet({ accountId: 'alice.testnet', rpcUrl: rpc.url });
  await serveCodec(context);
  await installTestWallet(context, wallet);
  await page.goto(`${CODEC_ORIGIN}/blank`);

  // NEAR Connect's discovery: the page says it is ready, the wallet announces itself.
  const announced = await page.evaluate(
    () =>
      new Promise((resolve) => {
        window.addEventListener('near-wallet-injected', (e) => resolve(e.detail.manifest.id), { once: true });
        window.dispatchEvent(new Event('near-selector-ready'));
      }),
  );
  expect(announced).toBe('nymira-test-wallet');

  const result = await page.evaluate(async () => {
    const wallet = window.nymiraTestWallet;
    const before = await wallet.getAccounts();
    const accounts = await wallet.signIn({ network: 'testnet' });
    const outcome = await wallet.signAndSendTransaction({
      receiverId: 'usdc.testnet',
      actions: [
        {
          type: 'FunctionCall',
          params: { methodName: 'ft_transfer_call', args: { amount: '1050000', msg: '{}' }, gas: '100000000000000', deposit: '1' },
        },
      ],
    });
    const message = await wallet.signMessage({
      message: 'hello',
      recipient: 'nymira.pages.dev',
      nonce: new Uint8Array(32).fill(5),
    });
    return { before, accounts, outcome, message, keys: Object.keys(wallet) };
  });

  expect(result.before).toEqual([]);
  expect(result.accounts).toEqual([{ accountId: 'alice.testnet', publicKey: wallet.publicKey }]);
  expect(result.outcome.status).toEqual({ SuccessValue: '' });
  expect(result.message.publicKey).toBe(wallet.publicKey);
  expect(result.keys).not.toContain('keyPair'); // the key stays in the runner

  expect(rpc.sent()).toHaveLength(1);
  const { transaction, valid } = decodeAndVerify(rpc.sent()[0].params.signed_tx_base64);
  expect(valid).toBe(true);
  expect(transaction.signerId).toBe('alice.testnet');
  expect(transaction.receiverId).toBe('usdc.testnet');
  expect(transaction.nonce).toBe(10n);
  expect(transaction.actions[0].functionCall.methodName).toBe('ft_transfer_call');
});

test('the codec WASM in the page gives the Rust vectors', async ({ context, page }) => {
  await serveCodec(context);
  await page.goto(`${CODEC_ORIGIN}/blank`);
  await loadCodecInPage(page);
  const produced = await page.evaluate(
    // `compute` is self-contained, so its source runs in the page.
    ({ source, table }) => new Function(`return (${source})`)()(window.nymiraCodec, table),
    { source: compute.toString(), table: vectors },
  );
  expect(produced).toEqual(expected());
});
