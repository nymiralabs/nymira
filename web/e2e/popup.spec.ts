// The wallet frame needs its own trusted click: a parent message carries no user activation.
// These tests check actual activation before native window.open, rather than treating a message
// as a click. Real Meteor and Intear services still need a manual test before release.
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
    window.open = (...args: Parameters<typeof window.open>) =>
      navigator.userActivation.isActive ? native(...args) : null;
  });
  await openFund(page);
  await page.getByTestId(`connect-${walletId}`).click();
  await expect(page.getByTestId('connected')).toContainText('sandbox.test');
}

/** What a click handler does: `arm` at once, then work for a while, then `send`. */
async function sendLikeAClick(page: Page, walletId: string, workMs: number) {
  await page.evaluate(
    ({ walletId, workMs }) => {
      const button = document.createElement('button');
      button.dataset.testid = 'send-like-click';
      button.textContent = 'Send test transaction';
      button.onclick = () => {
        const frame = document.querySelector<HTMLIFrameElement>('iframe.wallet-frame')!;
        const post = (message: Record<string, unknown>) =>
          frame.contentWindow!.postMessage(message, '*');
        window.addEventListener('message', (event) => {
          if (
            event.source === frame.contentWindow &&
            event.data?.type === 'reply' &&
            event.data.id === 901
          ) {
            (window as unknown as Record<string, unknown>).walletReply = event.data;
          }
        });
        post({ id: 900, method: 'arm', params: { walletId } });
        setTimeout(
          () =>
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
            }),
          workMs,
        );
      };
      document.body.append(button);
    },
    { walletId, workMs },
  );
  await page.getByTestId('send-like-click').click();
  await page.waitForFunction(() => 'walletReply' in window);
  return page.evaluate(
    () =>
      (window as unknown as { walletReply: { ok: boolean; result?: unknown; error?: string } })
        .walletReply,
  );
}

test('a wallet that opens late gets the window from a trusted click inside the frame', async ({
  context,
  page,
}) => {
  await setUp(context, page, 'popup-wallet');
  const opened: string[] = [];
  context.on('page', (p) => opened.push(p.url()));
  const started = Date.now();
  const result = sendLikeAClick(page, 'popup-wallet', 800);

  // The blank window is there at once, long before the executor's own 1.5 s of work is over.
  const popupOpened = context.waitForEvent('page');
  await page
    .frameLocator('iframe.wallet-frame')
    .getByRole('button', { name: 'Open wallet window' })
    .click();
  const popup = await popupOpened;
  expect(Date.now() - started).toBeLessThan(2000);

  const answer = await result;
  expect(answer.ok).toBe(true);
  expect(answer.result).toMatchObject({ transaction: { hash: 'popup-test-hash' } });
  // One window in all, and it is the one that was opened first, now showing the approval page.
  expect(opened).toHaveLength(1);
  expect(popup.url()).toBe('http://localhost:4173/popup-approve.html');
  expect(context.pages()).toHaveLength(2);
});

test('without the hand-off an open after browser activation expires is refused', async ({
  context,
  page,
}) => {
  await setUp(context, page, 'blocked-wallet');
  const answer = await sendLikeAClick(page, 'blocked-wallet', 6000);
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
  await page
    .frameLocator('iframe.wallet-frame')
    .getByRole('button', { name: 'Open wallet window' })
    .click();
  const blank = await popup;
  const closed = blank.waitForEvent('close');
  await page.evaluate(() => {
    const frame = document.querySelector<HTMLIFrameElement>('iframe.wallet-frame')!;
    frame.contentWindow!.postMessage({ id: 901, method: 'release', params: {} }, '*');
  });
  await closed;
  expect(context.pages()).toHaveLength(1);
});

test('the frame stays sandboxed without allow-same-origin', async ({ context, page }) => {
  await setUp(context, page, 'popup-wallet');
  const sandbox = await page.locator('iframe.wallet-frame').getAttribute('sandbox');
  expect(sandbox).toContain('allow-popups');
  expect(sandbox).not.toContain('allow-same-origin');
});
