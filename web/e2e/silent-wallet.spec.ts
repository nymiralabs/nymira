// A wallet that never returns (issue 171). Intear showed "Couldn't preview these transactions",
// the user approved, the window stayed open and the app waited for ever, then said "User rejected
// the transaction" although the transaction had landed. The app now reads the chain while it waits.
// The test wallet plays the wallet; the fake chain shows what the vault would show afterwards.
import { join } from 'node:path';
import { installTestWallet } from '@testkit';
import type { BrowserContext, Page } from '@playwright/test';
import { expect, scriptTree, test } from './chat-support';
import { LeaveWallet, openClose, showActive } from './leave-support';
import {
  backUpAndTest,
  newFundingWallet,
  openFund,
  prepare,
  scriptChain,
  workdir,
  type WalletMode,
  enterApp,
} from './support';

test.describe.configure({ timeout: 240_000 });

async function readyToSend(page: Page, context: BrowserContext, mode: WalletMode) {
  const wallet = await newFundingWallet(context);
  wallet.mode = mode;
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), `silent-${mode}.nymira-backup`));
  return wallet;
}

test.describe('funding with a wallet that does not answer', () => {
  test('the wallet never returns, the deposit landed: the app finds it and says what it could not check', async ({
    context,
    page,
  }) => {
    await readyToSend(page, context, 'silent');
    await page.getByTestId('send').click();
    await expect(page.getByTestId('accepted')).toBeVisible({ timeout: 60_000 });
    await expect(page.getByTestId('note-id')).toHaveText('#0');
    await expect(page.getByTestId('accepted')).toContainText('found the note on the vault itself');
    await expect(page.getByTestId('chain-found-warning')).toContainText('payout account');
    await expect(page.getByTestId('fund-error')).toHaveCount(0);
    await expect(page.getByTestId('send')).toBeDisabled();
  });

  test('the user closes the wallet window after it sent: never "rejected" when the chain shows the deposit', async ({
    context,
    page,
  }) => {
    await readyToSend(page, context, 'silent-closed');
    await page.getByTestId('send').click();
    await expect(page.getByTestId('accepted')).toBeVisible({ timeout: 60_000 });
    await expect(page.getByTestId('fund-error')).toHaveCount(0);
    await expect(page.getByTestId('rejected')).toHaveCount(0);
  });

  test('the user really cancelled: "User rejected the transaction" after the grace period, nothing accepted', async ({
    context,
    page,
  }) => {
    const wallet = await readyToSend(page, context, 'cancel');
    await page.getByTestId('send').click();
    await expect(page.getByTestId('fund-error')).toContainText('User rejected the transaction', {
      timeout: 60_000,
    });
    expect(wallet.sent).toHaveLength(0);
    await expect(page.getByTestId('accepted')).toHaveCount(0);
    // The note can be sent again.
    await expect(page.getByTestId('send')).toBeEnabled();
  });

  test('nothing from the wallet and nothing on chain: a visible time-out that points to the Notes recovery', async ({
    context,
    page,
  }) => {
    await readyToSend(page, context, 'dead');
    await page.getByTestId('send').click();
    await expect(page.getByTestId('fund-error')).toContainText('did not answer', {
      timeout: 60_000,
    });
    await expect(page.getByTestId('fund-error')).toContainText('Notes tab');
    await expect(page.getByTestId('fund-error')).toContainText('transaction hash');
    await expect(page.getByTestId('send')).toBeEnabled();
  });
});

test.describe('closing with a wallet that does not answer', () => {
  async function fundThen(page: Page, context: BrowserContext, close: LeaveWallet['closeMode']) {
    const wallet = new LeaveWallet();
    await scriptChain(wallet);
    await installTestWallet(context, wallet);
    await openFund(page);
    await prepare(page);
    await backUpAndTest(page, join(workdir(), `silent-close-${close}.nymira-backup`));
    await page.getByTestId('send').click();
    await expect(page.getByTestId('accepted')).toBeVisible();
    await scriptTree(wallet);
    await showActive(wallet, wallet.lastDeposit.expiry as number);
    wallet.closeMode = close;
    return wallet;
  }

  test('the wallet never returns, the close landed: the normal result screen', async ({
    context,
    page,
    stack,
  }) => {
    stack.reset();
    const wallet = await fundThen(page, context, 'silent');
    await openClose(page);
    await page.getByTestId('close-wallet').click();
    await expect(page.getByTestId('leave-message')).toContainText('is closed', {
      timeout: 240_000,
    });
    await expect(page.getByTestId('leave-state')).toHaveText('closed');
    await expect(page.getByTestId('leave-error')).toHaveCount(0);
    expect(wallet.vaultCalls.map((c) => c.method)).toEqual(['mutual_close']);
  });

  test('the user really cancelled the close: "User rejected the transaction", nothing reached the vault', async ({
    context,
    page,
    stack,
  }) => {
    stack.reset();
    const wallet = await fundThen(page, context, 'cancel');
    await openClose(page);
    await page.getByTestId('close-wallet').click();
    await expect(page.getByTestId('leave-error')).toContainText('User rejected the transaction', {
      timeout: 240_000,
    });
    expect(wallet.vaultCalls).toHaveLength(0);
    // The close is recorded as started before the proof can leave, so a cancel keeps that mark
    // and the vault can still be asked; the note itself is not closed.
    await expect(page.getByTestId('leave-state')).toHaveText('close started, not confirmed');
    await expect(page.getByTestId('close-recheck')).toBeVisible();
  });

  test('nothing from the wallet and nothing on chain: a time-out with a pointer, and the vault can be asked', async ({
    context,
    page,
    stack,
  }) => {
    stack.reset();
    await fundThen(page, context, 'dead');
    await openClose(page);
    await page.getByTestId('close-wallet').click();
    await expect(page.getByTestId('leave-error')).toContainText('did not answer', {
      timeout: 240_000,
    });
    await expect(page.getByTestId('leave-error')).toContainText(
      'Ask the vault whether the close landed',
    );
    await expect(page.getByTestId('leave-state')).toHaveText('close started, not confirmed');
    await expect(page.getByTestId('close-recheck')).toBeVisible();
  });
});

test('after a reload the app says the wallet connection is gone and offers to connect again', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  await openFund(page);
  await page.getByTestId('connect-fake-wallet').click();
  await expect(page.getByTestId('connected')).toContainText('sandbox.test');
  await expect(page.getByTestId('session-lost')).toHaveCount(0);

  await page.reload();
  await enterApp(page);
  await expect(page.getByTestId('session-lost')).toContainText('not connected any more');
  await expect(page.getByTestId('session-lost')).toContainText('forgets the connection on purpose');
  await page.getByTestId('reconnect').click();
  await expect(page.getByTestId('connected')).toContainText('sandbox.test');
  await expect(page.getByTestId('session-lost')).toHaveCount(0);
});
