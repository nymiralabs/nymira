// The popup hand-off (docs/open-items.md rows 110 and 111). Meteor opens its approval window from
// its executor after async work, when the click that started the request has expired, and the
// browser refuses it. The wallet frame opens a blank window while the click is fresh and gives it
// to the executor's later `window.open`.
//
// What this proves and what it does not: Playwright's browsers do not block popups, so the test
// models the blocker. An init script in the wallet frame makes `window.open` return null unless
// the frame received a message from the app in the last 500 ms (an expired click). It cannot prove
// what Safari or Chrome do with a real click; the manual list in web/README.md covers that.
import { readFileSync } from 'node:fs';
import { expect, test, type BrowserContext, type Page } from '@playwright/test';
import { newFundingWallet, openFund } from './support';

const executor = readFileSync(
  new URL('../test-fixtures/executors/popup-wallet.js', import.meta.url),
  'utf8',
);
const approvalPage = readFileSync(
  new URL('../test-fixtures/popup-approve.html', import.meta.url),
  'utf8',
);

async function setUp(context: BrowserContext, page: Page, walletId: string) {
  await newFundingWallet(context);
  await context.route('**/executors/popup-wallet.js', (route) =>
    route.fulfill({ contentType: 'text/javascript', body: executor }),
  );
  await context.route('**/popup-approve.html', (route) =>
    route.fulfill({ contentType: 'text/html', body: approvalPage }),
  );
  await context.addInitScript(() => {
    if (location.pathname !== '/wallet-host.html') return;
    const native = window.open.bind(window);
    let lastMessage = 0;
    window.addEventListener(
      'message',
      (event) => {
        if (event.source === window.parent) lastMessage = Date.now();
      },
      true,
    );
    window.open = (...args: Parameters<typeof window.open>) =>
      Date.now() - lastMessage < 500 ? native(...args) : null;
  });
  await openFund(page);
  await page.getByTestId(`connect-${walletId}`).click();
  await expect(page.getByTestId('connected')).toContainText('sandbox.test');
}

/** What a click handler does: `arm` at once, then work for a while, then `send`. */
async function sendLikeAClick(page: Page, walletId: string, workMs: number) {
  return page.evaluate(
    async ({ walletId, workMs }) => {
      const frame = document.querySelector<HTMLIFrameElement>('iframe.wallet-frame')!;
      const post = (message: Record<string, unknown>) =>
        frame.contentWindow!.postMessage(message, '*');
      const reply = new Promise<{ ok: boolean; result?: unknown; error?: string }>((resolve) => {
        window.addEventListener('message', (event) => {
          if (
            event.source === frame.contentWindow &&
            event.data?.type === 'reply' &&
            event.data.id === 901
          ) {
            resolve(event.data);
          }
        });
      });
      post({ id: 900, method: 'arm', params: { walletId } });
      await new Promise((r) => setTimeout(r, workMs));
      post({
        id: 901,
        method: 'send',
        params: {
          receiverId: 'usdc.test',
          walletId,
          actions: [
            {
              type: 'FunctionCall',
              params: { methodName: 'ft_transfer_call', args: {}, gas: '1', deposit: '1' },
            },
          ],
        },
      });
      return reply;
    },
    { walletId, workMs },
  );
}

test('a wallet that opens its window late still gets it: the frame opens it while the click is fresh', async ({
  context,
  page,
}) => {
  await setUp(context, page, 'popup-wallet');
  const opened: string[] = [];
  context.on('page', (p) => opened.push(p.url()));
  const started = Date.now();
  const result = sendLikeAClick(page, 'popup-wallet', 800);

  // The blank window is there at once, long before the executor's own 1.5 s of work is over.
  const popup = await context.waitForEvent('page');
  expect(Date.now() - started).toBeLessThan(1200);

  const answer = await result;
  expect(answer.ok).toBe(true);
  expect(answer.result).toMatchObject({ transaction: { hash: 'popup-test-hash' } });
  // One window in all, and it is the one that was opened first, now showing the approval page.
  expect(opened).toHaveLength(1);
  expect(popup.url()).toBe('http://localhost:4173/popup-approve.html');
  expect(context.pages()).toHaveLength(2);
});

test('without the hand-off the same late open is refused (the model of an expired click)', async ({
  context,
  page,
}) => {
  await setUp(context, page, 'blocked-wallet');
  const answer = await sendLikeAClick(page, 'blocked-wallet', 800);
  expect(answer.ok).toBe(false);
  expect(answer.error).toContain('blocked the approval window');
  expect(context.pages()).toHaveLength(1);
});

test('a window nobody used is closed when the request ends', async ({ context, page }) => {
  await setUp(context, page, 'popup-wallet');
  const popup = context.waitForEvent('page');
  await page.evaluate(() => {
    const frame = document.querySelector<HTMLIFrameElement>('iframe.wallet-frame')!;
    frame.contentWindow!.postMessage(
      { id: 900, method: 'arm', params: { walletId: 'popup-wallet' } },
      '*',
    );
  });
  const blank = await popup;
  await page.evaluate(() => {
    const frame = document.querySelector<HTMLIFrameElement>('iframe.wallet-frame')!;
    frame.contentWindow!.postMessage({ id: 901, method: 'release', params: {} }, '*');
  });
  await blank.waitForEvent('close');
  expect(context.pages()).toHaveLength(1);
});

test('the frame stays sandboxed without allow-same-origin', async ({ context, page }) => {
  await setUp(context, page, 'popup-wallet');
  const sandbox = await page.locator('iframe.wallet-frame').getAttribute('sandbox');
  expect(sandbox).toContain('allow-popups');
  expect(sandbox).not.toContain('allow-same-origin');
});
