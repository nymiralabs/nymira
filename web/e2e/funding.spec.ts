import { join } from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import {
  BACKUP_PASSPHRASE,
  OTHER,
  SENDER,
  TOKEN,
  VAULT,
  backUpAndTest,
  connectWallet,
  expectNoSecret,
  newFundingWallet,
  openFund,
  pageSurfaces,
  prepare,
  readBackup,
  rpcRequests,
  watch,
  workdir,
} from './support';

test.describe('landing', () => {
  test('says what Nymira is, who sees what, and what it cannot do', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('description')).toContainText('Prepaid AI credit');
    await expect(page.getByTestId('who-sees-what')).toBeVisible();
    await expect(page.getByTestId('tor-notice')).toContainText('cannot use Tor');
    await expect(page.getByTestId('tor-notice')).toContainText('RPC provider sees your IP address');
    await expect(page.getByTestId('tor-notice')).toContainText('Tor Browser');
    await expect(page.getByTestId('tor-notice')).toContainText('nymirad');
    await expect(page.getByTestId('testnet-notice')).toContainText('anonymity set is small');
    await expect(page.getByTestId('testnet-notice')).toContainText('test notes');
    await expect(page.getByTestId('integrity-notice')).toContainText('compromised host');
    await page.getByTestId('open-app').click();
    await expect(page.getByTestId('fund')).toBeVisible();
    // The banner stays on every app screen.
    await expect(page.getByTestId('ip-banner')).toContainText('cannot hide your IP address');
  });
});

test('@webkit funds a note: backup first, then the transfer, then the receipt', async ({
  context,
  page,
}, info) => {
  const seen = watch(page);
  const wallet = await newFundingWallet(context);
  await openFund(page);

  // Vault pins match the chain, and the fund screen shows what the user pays.
  await expect(page.getByTestId('vault-ok')).toBeVisible();
  await expect(page.getByTestId('price-denomination')).toHaveText('1.00 USDC');
  await expect(page.getByTestId('price-fee')).toHaveText('0.05 USDC');
  await expect(page.getByTestId('price-total')).toHaveText('1.05 USDC');
  await expect(page.getByTestId('faucet')).toContainText('faucet.circle.com');
  await expect(page.getByTestId('faucet')).toContainText('20 USDC per request');
  await expect(page.getByTestId('faucet')).toContainText('every 2 hours');
  await expect(page.getByTestId('faucet')).toContainText('storage_deposit');
  await page.getByTestId('denomination-5000000').click();
  await expect(page.getByTestId('price-total')).toHaveText('5.05 USDC');
  await page.getByTestId('denomination-1000000').click();

  await prepare(page);
  // The secret is stored (sealed) before anything is sent, and nothing was sent.
  expect(wallet.sent).toHaveLength(0);
  await expect(page.getByTestId('send')).toBeDisabled();
  await expect(page.getByTestId('persist-status')).toContainText('Persistent storage:');
  await expect(page.getByTestId('backup-params')).toContainText(
    'Argon2id, 64 MiB memory, 3 passes',
  );
  await expect(page.getByTestId('backup-params')).toContainText('synced folder');

  // The backup must be downloaded and opened again before the transfer is offered.
  await page.getByTestId('backup-passphrase').fill('short');
  await expect(page.getByTestId('backup-download')).toBeDisabled();
  const file = await backUpAndTest(page, join(workdir(), 'first.nymira-backup'));
  expect(wallet.sent).toHaveLength(0);
  await expect(page.getByTestId('send')).toBeEnabled();

  // What the backup holds, and that the stored copy and the page hold none of it in the clear.
  const backup = await readBackup(file);
  expect(backup.notes).toHaveLength(1);
  const secret = backup.notes[0]?.secret ?? '';
  expect(secret).toMatch(/^0x[0-9a-f]{64}$/);
  expectNoSecret(await pageSurfaces(page), secret);
  const stored = (await pageSurfaces(page)).find((s) => s.startsWith('idb:notes:')) ?? '';
  expect(stored).toContain(backup.notes[0]?.commitment);
  expect(stored).toContain('"phase":"backed_up"');

  // The transfer: ft_transfer_call on the token, 1 yocto, a message with no withdraw_to.
  await page.getByTestId('send').click();
  await expect(page.getByTestId('accepted')).toBeVisible();
  expect(wallet.calls).toHaveLength(1);
  const call = wallet.calls[0]!;
  expect(call.receiverId).toBe(TOKEN);
  expect(call.args.receiver_id).toBe(VAULT);
  expect(call.args.amount).toBe('1050000');
  expect(JSON.parse(call.args.msg as string)).toEqual({ commitment: backup.notes[0]?.commitment });

  await expect(page.getByTestId('note-id')).toHaveText('#0');
  await expect(page.getByTestId('accepted')).toContainText('from your wallet');
  await expect(page.getByTestId('rpc-read-notice')).toHaveCount(0);
  const link = page.getByTestId('explorer-link');
  await expect(link).toHaveAttribute(
    'href',
    `https://testnet.nearblocks.io/txns/${wallet.lastHash}`,
  );
  await expect(page.getByTestId('accepted')).toContainText(`payout account is ${SENDER}`);
  await expect(page.getByTestId('wait-hint')).toContainText('Wait a while before your first chat');

  // The calendar file: generic title, dated by the event's expiry, no note data.
  const [ics] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('calendar').click(),
  ]);
  const calendar = await (await import('node:fs/promises')).readFile((await ics.path())!, 'utf8');
  expect(calendar).toContain('SUMMARY:Prepaid note expires today');
  for (const word of ['nymira', 'vault', 'usdc', 'alice', backup.notes[0]?.commitment ?? 'x']) {
    expect(calendar.toLowerCase()).not.toContain(word.toLowerCase());
  }

  // After a full run: no plain secret in IndexedDB, storage, the page, the console or any request.
  const surfaces = await pageSurfaces(page);
  expectNoSecret(surfaces, secret);
  expectNoSecret(seen.console, secret);
  expectNoSecret(seen.requests, secret);
  expectNoSecret([JSON.stringify(await rpcRequests())], secret);
  expectNoSecret([JSON.stringify(wallet.calls), JSON.stringify(wallet.sent)], secret);
  expect(surfaces.join('\n')).toContain('"phase":"accepted"');

  // note_status and nullifier reads never reach the RPC.
  const methods = (await rpcRequests()).map(
    (r) =>
      `${r.method} ${JSON.stringify((r.params as { method_name?: string }).method_name ?? '')}`,
  );
  expect(methods.join('\n')).not.toMatch(/note_status|nullifier/i);

  // The app ran under the production header set without a policy violation, and fetched no
  // wallet manifest or anything else from a third party.
  expect(seen.violations).toEqual([]);
  for (const url of seen.requests) {
    expect(url).not.toMatch(/raw\.githubusercontent|jsdelivr|fonts\.|analytics/);
  }
  info.annotations.push({ type: 'note', description: `${seen.requests.length} requests` });
});
