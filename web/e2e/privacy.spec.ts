// The privacy panel against the mock stack. The pinned team list of the mock build is notes 0, 1
// and 2 (test accounts); the note under test is note 0, so it is on the team list too.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { decryptBackup } from '../src/backup/format';
import {
  endConversation,
  expect,
  fundNote,
  openChat,
  say,
  scriptVault,
  startConversation,
  test,
  type VaultRow,
} from './chat-support';
import { BACKUP_PASSPHRASE, rpcRequests, watch, workdir } from './support';

test.describe.configure({ timeout: 420_000 });

const nowSecs = () => Math.floor(Date.now() / 1000);
const active = (n: number): VaultRow[] =>
  Array.from({ length: n }, () => ({ status: 'Active' as const }));

async function lease(page: Page, text: string, first = false) {
  if (!first) await page.getByRole('button', { name: 'New conversation' }).click();
  await startConversation(page);
  await say(page, text);
  await endConversation(page);
}

/** Moves the time the app saw the deposit (a public field of the note record). */
function setDepositedAt(page: Page, at: number) {
  return page.evaluate(async (at) => {
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const r = indexedDB.open('nymira');
      r.onsuccess = () => resolve(r.result);
      r.onerror = () => reject(r.error);
    });
    const tx = db.transaction('notes', 'readwrite');
    const store = tx.objectStore('notes');
    const all = await new Promise<{ id: string }[]>((resolve) => {
      const q = store.getAll();
      q.onsuccess = () => resolve(q.result);
    });
    for (const n of all) store.put({ ...n, depositedAt: at });
    await new Promise((resolve) => (tx.oncomplete = resolve));
    db.close();
  }, at);
}

async function openPanel(page: Page) {
  await page.getByTestId('chat-privacy').click();
  await expect(page.getByTestId('privacy-screen')).toBeVisible();
}

const closePanel = (page: Page) => page.getByTestId('privacy-back').click();

test('six notes, three on the team: set size 3 (external) and 6; drops, later close and later deposits', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  stack.set({ params: { team_note_ids: [0, 1, 2] } });
  const { wallet } = await fundNote(page, context);
  await scriptVault(wallet, active(6));
  await openChat(page);
  await lease(page, 'first lease', true);

  // Two more notes join, and the vault now holds an expired note (4), a note in a pending exit
  // (5) and a closed one (6) beside the later deposit (7).
  const past = nowSecs() - 3600;
  await scriptVault(wallet, [
    ...active(4),
    { status: 'Active', expiry: past },
    { status: 'PendingExit' },
    { status: 'Closed' },
    { status: 'Active' },
  ]);
  await lease(page, 'second lease');

  // A close lands after both leases: note 3 leaves.
  await scriptVault(wallet, [
    ...active(3),
    { status: 'Closed' },
    { status: 'Active', expiry: past },
    { status: 'PendingExit' },
    { status: 'Closed' },
    { status: 'Active' },
  ]);

  await openPanel(page);
  const rows = page.getByTestId('lease-row');
  await expect(rows).toHaveCount(2);
  // The 6-note case: notes 0 to 5 under the proof's root, 0, 1 and 2 on the team.
  await expect(rows.nth(0).getByTestId('set-size')).toHaveText('set size: 3 (external), total 6');
  const [a, b] = [...stack.leases.keys()];
  await expect(rows.nth(0).getByTestId('lease-id')).toHaveText(a!);
  await expect(rows.nth(1).getByTestId('lease-id')).toHaveText(b!);
  await expect(rows.nth(0)).toContainText('z-ai/glm-5.3-flash');
  await expect(rows.nth(0)).toContainText('1 messages');
  await expect(rows.nth(0)).toContainText('tokens in');
  await expect(rows.nth(0).getByTestId('lease-bound')).toHaveText('0.10 USDC');
  await expect(rows.nth(0).getByTestId('lease-time')).toContainText('UTC');
  await expect(rows.nth(0).getByTestId('lease-charge')).toContainText('USDC');
  // Expired, pending-exit and closed notes drop out; notes 6 and 7 were not there for lease 1.
  await expect(rows.nth(1).getByTestId('set-size')).toHaveText('set size: 2 (external), total 5');
  await expect(rows.nth(1).getByTestId('small-set')).toContainText(
    'matching a lease to a deposit is easy',
  );
  await expect(rows.nth(0).getByTestId('small-set')).toBeVisible();
  // The raw open-note count never stands alone.
  await expect(rows.nth(0)).not.toContainText(/open notes/i);
  // The later close lowers "now" and leaves the stored figures alone.
  await expect(page.getByTestId('now-text')).toHaveText('now: set size: 1 (external), total 4');
  await expect(rows.nth(0).getByTestId('set-size')).toContainText('total 6');
  // Both mock endpoints answer from the same chain, so there is no provider difference to show,
  // and the panel does not say it read one provider only.
  await expect(rows.nth(0).getByTestId('set-difference')).toHaveCount(0);
  const reads = (await rpcRequests()).filter(
    (r) => (r.params as { method_name?: string }).method_name === 'note_records',
  );
  expect(reads.length).toBeGreaterThanOrEqual(2);
  // Deposits in between come from leaf counts, with the team removed.
  await expect(rows.nth(0).getByTestId('cover')).toContainText(
    '5 deposits joined the tree in between, 3 external',
  );
  await expect(rows.nth(0).getByTestId('timing-warning')).toBeVisible();

  // The figure is part of the lease record the backup carries (metadata only).
  await closePanel(page);
  await page.getByTestId('nav-notes').click();
  await page.getByTestId('notes-backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('notes-backup-download').click(),
  ]);
  const file = join(workdir(), 'privacy.nymira-backup');
  await download.saveAs(file);
  const plain = await decryptBackup(new Uint8Array(readFileSync(file)), BACKUP_PASSPHRASE);
  const state = plain.notes.find((n) => n.kind === 'state')!;
  const extra = JSON.parse(state.extra!) as { leases: { setSize: Record<string, unknown> }[] };
  expect(extra.leases.map((l) => [l.setSize.external, l.setSize.total])).toEqual([
    [3, 6],
    [2, 5],
  ]);
  expect(state.depositedAt).toBeGreaterThan(0);
  expect(state.extra).not.toContain('first lease');
});

test('a pinned ID missing from the operator list does not raise N; an extra ID lowers it', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { wallet } = await fundNote(page, context);
  await scriptVault(wallet, active(6));
  await openChat(page);
  // The operator leaves pinned ID 2 out.
  stack.set({ params: { team_note_ids: [0, 1] } });
  await lease(page, 'omitting lease', true);
  // The operator adds ID 3.
  stack.set({ params: { team_note_ids: [0, 1, 2, 3] } });
  await lease(page, 'adding lease');
  // A long job uses the 0.50 class, which few leases use.
  await page.getByRole('button', { name: 'New conversation' }).click();
  await page.getByTestId('long-job').check();
  await lease(page, 'long job', true);

  await openPanel(page);
  const rows = page.getByTestId('lease-row');
  await expect(rows.nth(0).getByTestId('set-size')).toHaveText('set size: 3 (external), total 6');
  await expect(rows.nth(0).getByTestId('set-difference').first()).toHaveText(
    "team IDs only in this release's pinned list: 2",
  );
  await expect(rows.nth(1).getByTestId('set-size')).toHaveText('set size: 2 (external), total 6');
  await expect(rows.nth(1).getByTestId('set-difference').first()).toHaveText(
    "team IDs only in the operator's list: 3",
  );
  // The long-job lease stands out; the default class does not.
  await expect(rows.nth(2).getByTestId('lease-bound')).toHaveText('0.50 USDC');
  await expect(rows.nth(2).getByTestId('stands-out')).toContainText('it stands out');
  await expect(rows.nth(0).getByTestId('stands-out')).toHaveCount(0);
  await expect(rows.nth(1).getByTestId('stands-out')).toHaveCount(0);
  // "now" uses the last list the operator gave.
  await expect(page.getByTestId('now-text')).toHaveText('now: set size: 2 (external), total 6');
});

test('a lease ten minutes after the deposit warns; an hour and five external deposits clear it', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  stack.set({ params: { team_note_ids: [0, 1, 2] } });
  const { wallet } = await fundNote(page, context);
  await scriptVault(wallet, active(12));
  // The deposit was seen ten minutes before the lease.
  await setDepositedAt(page, Date.now() - 600_000);
  await openChat(page);
  await lease(page, 'ten minutes', true);

  await openPanel(page);
  const row = page.getByTestId('lease-row').first();
  await expect(row.getByTestId('set-size')).toHaveText('set size: 9 (external), total 12');
  await expect(row.getByTestId('cover')).toContainText('minutes after the deposit');
  await expect(row.getByTestId('cover')).toContainText(
    '11 deposits joined the tree in between, 9 external',
  );
  await expect(row.getByTestId('timing-warning')).toContainText('Timing can match');
  // Nine external notes: no small-set sentence, but 5 is still not called safe.
  await expect(row.getByTestId('small-set')).toHaveCount(0);
  await expect(row.getByTestId('five-not-safe')).toContainText('5 is still not a safe number');
  await expect(page.getByTestId('thresholds')).toContainText('less than 1 hour');
  await expect(page.getByTestId('thresholds')).toContainText('fewer than 5 external deposits');

  // With the deposit two hours before the lease and 9 external deposits since, there is no warning.
  await closePanel(page);
  await setDepositedAt(page, Date.now() - 7_200_000);
  await openPanel(page);
  await expect(page.getByTestId('cover').first()).toContainText('hours after the deposit');
  await expect(page.getByTestId('timing-warning')).toHaveCount(0);
});

test('the panel from the notes and chat screens: chain column from the record, honest text, no address', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const seen = watch(page);
  const { wallet } = await fundNote(page, context);
  await scriptVault(wallet, active(1));
  await page.getByTestId('nav-notes').click();
  await expect(page.getByTestId('notes-privacy')).toBeVisible();
  const before = (await rpcRequests()).length;
  const requestsBefore = seen.requests.length;
  await page.getByTestId('notes-privacy').click();
  await expect(page.getByTestId('privacy-screen')).toBeVisible();
  await expect(page.getByTestId('now-text')).toContainText('now: set size: 0 (external), total 1');

  // Left column: the record, a link to the explorer, and no lookup for it.
  await expect(page.getByTestId('chain-note-id')).toHaveText('0');
  await expect(page.getByTestId('chain-denomination')).toHaveText('1.00 USDC');
  await expect(page.getByTestId('chain-payout')).toHaveText('alice.test');
  await expect(page.getByTestId('chain-fee')).toHaveText('0.05 USDC');
  await expect(page.getByTestId('chain-commitment')).toHaveText(/^0x[0-9a-f]{64}$/);
  await expect(page.getByTestId('chain-expiry')).toContainText('UTC');
  const link = page.getByTestId('explorer-link');
  await expect(link).toHaveAttribute(
    'href',
    new RegExp(`^https://testnet\\.nearblocks\\.io/txns/${wallet.lastHash}$`),
  );
  await expect(link).toHaveAttribute('rel', 'noopener noreferrer');
  const calls = (await rpcRequests()).slice(before).map((r) => r.method);
  expect(calls).not.toContain('tx');
  expect(calls).not.toContain('EXPERIMENTAL_tx_status');
  const newRequests = seen.requests.slice(requestsBefore).join('\n');
  expect(newRequests).not.toContain('nearblocks');
  expect(newRequests).not.toContain(':18546'); // the operator is not asked either
  // The link opens only when clicked.
  await context.route('https://testnet.nearblocks.io/**', (route) =>
    route.fulfill({ status: 200, contentType: 'text/html', body: '<title>explorer</title>' }),
  );
  const [popup] = await Promise.all([page.waitForEvent('popup'), link.click()]);
  expect(popup.url()).toContain('testnet.nearblocks.io/txns/');
  await popup.close();

  // Right column: no lease yet, and the operator's published policy, labelled as such.
  await expect(page.getByTestId('no-leases')).toBeVisible();
  const policy = page.getByTestId('policy');
  await expect(policy).toContainText('published retention policy');
  await expect(policy).toContainText('The client cannot check it');
  await expect(policy).toContainText('nullifier');
  await expect(policy).toContainText('rounded to the hour');
  await expect(policy).toContainText('44 days');
  await expect(policy).toContainText('second machine');

  // The address sentence, always; no address anywhere on the page.
  const address = page.getByTestId('address-line');
  await expect(address).toContainText('see your network address and the time of every lease');
  await expect(address).toContainText('Tor Browser');
  await expect(address).toContainText('nymirad');
  const shown = await page.getByTestId('privacy-screen').innerText();
  expect(shown).not.toMatch(/\b\d{1,3}(\.\d{1,3}){3}\b/);
  expect(shown).not.toMatch(/[0-9a-f]{1,4}(:[0-9a-f]{1,4}){4,}/i);

  // The honesty sentences and the testnet text, shared with `nymirad status`.
  const honesty = page.getByTestId('honesty');
  await expect(honesty).toContainText('Note balances are private');
  await expect(honesty).toContainText('N includes your own note');
  await expect(honesty).toContainText('One person can hold several notes');
  await expect(honesty).toContainText('the team’s own declaration');
  await expect(honesty).toContainText(
    'N includes notes the operator or the team control and did not list',
  );
  await expect(page.getByTestId('testnet-text')).toHaveText(
    'This is a testnet build. The notes are test notes funded with free testnet USDC, anyone can add them at no cost, and the set is small.',
  );
  expect(shown).not.toMatch(/real people|cannot be linked|unlinkable|anonymous/i);

  // The same panel opens from the chat screen.
  await page.getByTestId('privacy-back').click();
  await openChat(page);
  await expect(page.getByTestId('chat-privacy')).toBeVisible();
  await openPanel(page);
});
