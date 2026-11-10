import { join } from 'node:path';
import { expect, test } from '@playwright/test';
import {
  OTHER,
  SENDER,
  TOKEN,
  VAULT,
  backUpAndTest,
  connectWallet,
  control,
  newFundingWallet,
  openFund,
  openNotes,
  prepare,
  rpcRequests,
  setUpPassphrase,
  workdir,
} from './support';

test('another payout account: fingerprint step, registration and freshness warnings, withdraw_to in the message', async ({
  context,
  page,
}) => {
  const wallet = await newFundingWallet(context, {
    registered: [SENDER, VAULT, 'carol.test'],
    balances: { [OTHER]: '0', 'carol.test': '3000000' },
  });
  await openFund(page);
  await connectWallet(page);
  await expect(page.getByTestId('payout-account')).toHaveText(SENDER);
  await expect(page.getByTestId('payout-default-note')).toContainText('nothing in it to alter');

  // An unregistered account: warning, and the note cannot be prepared before the fingerprint.
  await page.getByTestId('payout-other').click();
  await page.getByTestId('payout-input').fill(OTHER);
  await expect(page.getByTestId('fingerprint')).toHaveText(/^[0-9a-f]{4} [0-9a-f]{4} [0-9a-f]{4}$/);
  await expect(page.getByTestId('warn-unregistered')).toContainText('not registered');
  await expect(page.getByTestId('fingerprint-box')).toContainText(OTHER);
  await setUpPassphrase(page);
  await expect(page.getByTestId('prepare')).toBeDisabled();
  await page.getByTestId('fingerprint-confirm').check();
  await expect(page.getByTestId('prepare')).toBeEnabled();

  // Changing the account takes the confirmation back; a non-fresh account is warned about.
  await page.getByTestId('payout-input').fill('carol.test');
  await expect(page.getByTestId('fingerprint-confirm')).not.toBeChecked();
  await expect(page.getByTestId('warn-not-fresh')).toContainText('already holds USDC');
  await expect(page.getByTestId('warn-unregistered')).toHaveCount(0);

  // A fresh, registered account has no warning; use it.
  await control({
    cmd: 'set_view',
    account: TOKEN,
    method: 'storage_balance_of',
    args: { json: { account_id: OTHER } },
    result: { json: { total: '1250000000000000000000', available: '0' } },
  });
  await page.getByTestId('payout-input').fill(OTHER);
  await expect(page.getByTestId('fingerprint')).toBeVisible();
  await expect(page.getByTestId('warn-unregistered')).toHaveCount(0);
  await expect(page.getByTestId('warn-not-fresh')).toHaveCount(0);
  await page.getByTestId('fingerprint-confirm').check();
  await page.getByTestId('prepare').click();
  await backUpAndTest(page, join(workdir(), 'other.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('accepted')).toBeVisible();
  const msg = JSON.parse(wallet.calls[0]!.args.msg as string);
  expect(msg.withdraw_to).toBe(OTHER);
  await expect(page.getByTestId('accepted')).toContainText(`payout account is ${OTHER}`);
  // The checks named the payout account to the RPC provider, which the screen said.
  const views = JSON.stringify(await rpcRequests());
  expect(views).toContain('storage_balance_of');
});

test('a mismatched payout remains pending and its note is retained', async ({ context, page }) => {
  const wallet = await newFundingWallet(context);
  wallet.eventOverride = { withdraw_to: 'mallory.test' };
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'tamper.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('pending')).toContainText('payout account');
  await expect(page.getByTestId('pending')).toContainText('do not assume a refund');
  await expect(page.getByTestId('accepted')).toHaveCount(0);
  await openNotes(page);
  await expect(page.getByTestId('note-row')).toHaveAttribute('data-phase', 'sent');
});

test('a refused transfer (no deposit event) shows as not accepted', async ({ context, page }) => {
  const wallet = await newFundingWallet(context);
  wallet.mode = 'refused';
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'refused.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('rejected')).toContainText('did not accept');
});

test('a wallet that returns only the hash makes the app read the receipt from the RPC, and says so', async ({
  context,
  page,
}) => {
  const wallet = await newFundingWallet(context);
  wallet.mode = 'bare';
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'bare.nymira-backup'));
  const before = (await rpcRequests()).length;
  await page.getByTestId('send').click();
  await expect(page.getByTestId('accepted')).toContainText('from the RPC provider');
  await expect(page.getByTestId('rpc-read-notice')).toContainText(
    'IP address, account, the transaction hash and the time',
  );
  const after = (await rpcRequests()).slice(before);
  expect(after.some((r) => r.method === 'EXPERIMENTAL_tx_status')).toBe(true);
  expect(JSON.stringify(after)).not.toMatch(/note_status|nullifier/);
});
