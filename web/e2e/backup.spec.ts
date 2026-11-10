import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '@playwright/test';
import { installTestWallet } from '@testkit';
import {
  BACKUP_PASSPHRASE,
  VAULT,
  backUpAndTest,
  expectNoSecret,
  newFundingWallet,
  openFund,
  openNotes,
  pageSurfaces,
  prepare,
  readBackup,
  watch,
  workdir,
  enterApp,
  lockAfterDevReload,
} from './support';

test('no transfer without a downloaded and test-restored backup; a wrong file does not count', async ({
  context,
  page,
}) => {
  const wallet = await newFundingWallet(context);
  await openFund(page);
  await prepare(page);
  const dir = workdir();
  await page.getByTestId('backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('backup-download').click(),
  ]);
  const good = join(dir, 'good.nymira-backup');
  await download.saveAs(good);
  await expect(page.getByTestId('send')).toBeDisabled();

  // A file that is not a backup, then a backup under another passphrase: both refused.
  const junk = join(dir, 'junk.nymira-backup');
  writeFileSync(junk, 'not a backup');
  await page.getByTestId('backup-file').setInputFiles(junk);
  await expect(page.getByTestId('fund-error')).toContainText('not a Nymira backup');
  await expect(page.getByTestId('send')).toBeDisabled();
  await page.getByTestId('backup-passphrase').fill('some other passphrase!');
  await page.getByTestId('backup-file').setInputFiles(good);
  await expect(page.getByTestId('fund-error')).toContainText('wrong passphrase');
  await expect(page.getByTestId('send')).toBeDisabled();
  expect(wallet.sent).toHaveLength(0);

  // The right passphrase and the file: the transfer is offered.
  await page.getByTestId('backup-passphrase').fill(BACKUP_PASSPHRASE);
  await page.getByTestId('backup-file').setInputFiles(good);
  await expect(page.getByTestId('send')).toBeEnabled();
  expect(wallet.sent).toHaveLength(0);
});

test('the backup names its network and vault', async ({ context, page }) => {
  await newFundingWallet(context);
  await openFund(page);
  await prepare(page);
  const file = await backUpAndTest(page, join(workdir(), 'a.nymira-backup'));
  const plain = await readBackup(file);
  expect(plain.vault).toBe(VAULT);
  expect(plain.network).toBe('testnet');
});

test('after a crash the note is found again from its deposit receipt', async ({
  context,
  page,
}) => {
  const wallet = await newFundingWallet(context);
  wallet.mode = 'crash';
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'crash.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('fund-error')).toContainText('the wallet window was closed');
  expect(wallet.sent).toHaveLength(1); // the transaction did go out

  // The page is reloaded: the sealed note is still there, marked as interrupted.
  await page.reload();
  await enterApp(page);
  await openNotes(page, true);
  const row = page.getByTestId('note-row');
  await expect(row).toHaveAttribute('data-phase', 'sending');
  // The hash from the wallet's history finds the deposit.
  await page.getByTestId('lookup-hash').fill(wallet.lastHash);
  await page.getByTestId('lookup').click();
  await expect(page.getByTestId('lookup-result')).toContainText('Found');
  await expect(row).toHaveAttribute('data-phase', 'accepted');
  await expect(row).toContainText('#0');
});

test('@webkit restore from a backup file after the site data was purged', async ({
  browser,
  context,
  page,
}, info) => {
  const wallet = await newFundingWallet(context);
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'before.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('accepted')).toBeVisible();

  // The backup after acceptance holds the wallet state.
  await openNotes(page);
  await page.getByTestId('notes-backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('notes-backup-download').click(),
  ]);
  const file = join(workdir(), 'after.nymira-backup');
  await download.saveAs(file);
  const plain = await readBackup(file);
  const note = plain.notes[0]!;
  expect(note.kind).toBe('state');
  expect(note.phase).toBe('accepted');
  expect(note.noteId).toBe(0);
  const state = JSON.parse(note.state ?? '{}');
  expect(state.note_id).toBe(0);
  expect(state.deposit_amount).toBe(1000000);
  expect(note.state).toContain('7718972759422244814'); // the u64 chain id, not rounded

  // "Clear site data": a browser with nothing in it (a new context has no storage at all).
  const fresh = await browser.newContext({ acceptDownloads: true });
  await installTestWallet(fresh, wallet);
  const page2 = await fresh.newPage();
  const seen = watch(page2);
  await page2.goto('/');
  await page2.getByTestId('open-app').click();
  await page2.getByTestId('nav-notes').click();
  await expect(page2.getByTestId('setup-wrap')).toBeVisible();
  await page2.getByTestId('setup-passphrase').fill('a different wrapping passphrase');
  await page2.getByTestId('setup-confirm').fill('a different wrapping passphrase');
  await page2.getByTestId('setup-submit').click();
  await expect(page2.getByTestId('no-notes')).toBeVisible();

  // Restore needs the right passphrase.
  await page2.getByTestId('notes-backup-passphrase').fill('not the backup passphrase');
  await page2.getByTestId('restore-file').setInputFiles(file);
  await expect(page2.getByTestId('notes-error')).toContainText('wrong passphrase');
  await page2.getByTestId('notes-backup-passphrase').fill(BACKUP_PASSPHRASE);
  await page2.getByTestId('restore-file').setInputFiles(file);
  await expect(page2.getByTestId('notes-message')).toContainText('Restored 1 note');
  const row = page2.getByTestId('note-row');
  await expect(row).toHaveAttribute('data-phase', 'accepted');
  await expect(row).toContainText('#0');

  // The restored note is sealed under the new key: reload, unlock with the new passphrase, see it.
  await page2.reload();
  await enterApp(page2);
  await page2.getByTestId('nav-notes').click();
  await lockAfterDevReload(page2);
  await page2.getByTestId('unlock-passphrase').fill('a wrong guess at the passphrase');
  await page2.getByTestId('unlock-submit').click();
  await expect(page2.getByText('Wrong passphrase.')).toBeVisible();
  await page2.getByTestId('unlock-passphrase').fill('a different wrapping passphrase');
  await page2.getByTestId('unlock-submit').click();
  await expect(page2.getByTestId('note-row')).toHaveAttribute('data-phase', 'accepted');
  expectNoSecret(await pageSurfaces(page2), state.secret_s);
  expectNoSecret(seen.console, state.secret_s);
  expectNoSecret(seen.requests, state.secret_s);
  await fresh.close();
  info.annotations.push({ type: 'engine', description: browser.browserType().name() });
});
