import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { BrowserContext, Page } from '@playwright/test';
import {
  expect,
  openChat,
  say,
  endConversation,
  scriptTree,
  startConversation,
  test,
} from './chat-support';
import {
  BACKUP_PASSPHRASE,
  ONECLICK,
  OTHER,
  SENDER,
  TOKEN,
  VAULT,
  WRAP_PASSPHRASE,
  connectWallet,
  control,
  expectNoSecret,
  expectedExpiry,
  newFundingWallet,
  openFund,
  pageSurfaces,
  readBackup,
  setUpPassphrase,
  workdir,
  enterApp,
  lockAfterDevReload,
} from './support';

test.describe.configure({ timeout: 300_000 });

const DEST_TX = '9'.repeat(44);

type Sent = { at: number; body: Record<string, unknown> }[];
const sentToOneClick = async (): Promise<Sent> => (await fetch(`${ONECLICK}/requests`)).json();

test('Intents funding, direct path: quote, backup first, pay the fake address, status, acceptance, chat', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await newFundingWallet(context);
  await fetch(`${ONECLICK}/requests`, { method: 'DELETE' });
  // Counts the calls to navigator.storage.persist().
  await page.addInitScript(() => {
    const w = window as unknown as { __persist: number };
    w.__persist = 0;
    navigator.storage.persist = async () => ++w.__persist > 0;
  });
  await openFund(page);
  await expect(page.getByTestId('vault-ok')).toBeVisible();
  // The page opens on the NEAR route: nothing has been asked of 1Click.
  expect(await sentToOneClick()).toEqual([]);
  // Choosing the route asks 1Click nothing; only Show sources does.
  await page.getByTestId('fund-source-other').check({ force: true });
  await page.waitForTimeout(1500);
  expect(await sentToOneClick()).toEqual([]);
  await expect(page.getByTestId('intents-tor')).toContainText('must not go through Tor');
  await expect(page.getByTestId('intents-privacy')).toHaveText(
    'On this path 1Click and the Intents explorer pair your source wallet with this deposit. The proofs are designed to keep the vault and the operator from tying your spending to this deposit, but while few notes are open, and without Tor, your spending can still be matched to it by time and network address. Wait a while between depositing and your first chat.',
  );
  await expect(page.getByTestId('intents-terms')).toContainText('Terms for this path');
  // The NEAR USDC route is one switch away.
  await expect(page.getByTestId('fund-source-near')).toBeVisible();
  await setUpPassphrase(page, undefined, 'intents-prepare');

  // Sources come from signed dry quotes, asked when the person presses Show sources.
  await page.getByTestId('intents-load-sources').click();
  const sources = page.getByTestId('intents-source');
  await expect(sources).toHaveCount(2, { timeout: 40_000 });
  const dry = await sentToOneClick();
  for (const { body } of dry) {
    expect(body.dry).toBe(true);
    expect(body.recipient).toBe('nymira.near');
    expect(JSON.stringify(body)).not.toContain(OTHER);
    expect(JSON.parse(body.customRecipientMsg as string).commitment).toBe(`0x${'01'.repeat(32)}`);
  }
  await sources.first().check();
  await page.getByTestId('intents-refund').fill('0x00000000000000000000000000000000000000aa');

  // The payout account needs the fingerprint check; the vault and the Intents contract are refused.
  await page.getByTestId('intents-payout').fill(VAULT);
  await expect(page.getByTestId('intents-payout-problem')).toBeVisible();
  await expect(page.getByTestId('intents-prepare')).toBeDisabled();
  await page.getByTestId('intents-payout').fill(OTHER);
  await expect(page.getByTestId('intents-fingerprint')).not.toBeEmpty();
  await expect(page.getByTestId('intents-warn-unregistered')).toBeVisible();
  await expect(page.getByTestId('intents-costs')).toContainText('close fee');
  await expect(page.getByTestId('intents-prepare')).toBeDisabled();
  await page.getByTestId('intents-fingerprint-confirm').check();

  // No live quote before the pending note is stored and the backup is tested.
  await page.getByTestId('intents-prepare').click();
  await expect(page.getByTestId('intents-backup-panel')).toBeVisible();
  await expect(page.getByTestId('intents-quote')).toBeDisabled();
  expect(await page.evaluate(() => (window as unknown as { __persist: number }).__persist)).toBe(1);
  expect((await sentToOneClick()).filter((r) => r.body.dry === false)).toEqual([]);
  await page.getByTestId('intents-backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('intents-backup-download').click(),
  ]);
  const file = join(workdir(), 'intents.nymira-backup');
  await download.saveAs(file);
  await page.getByTestId('intents-backup-file').setInputFiles(file);
  await expect(page.getByTestId('intents-backup-tested')).toBeVisible();
  const backup = await readBackup(file);
  const secret = backup.notes[0]?.secret ?? '';
  const commitment = backup.notes[0]?.commitment ?? '';
  expect(secret).toMatch(/^0x[0-9a-f]{64}$/);

  // The signed quote: exact output to NEAR USDC for 1.05 USDC, the vault as recipient, the message.
  await page.getByTestId('intents-quote').click();
  await expect(page.getByTestId('intents-review')).toBeVisible();
  const live = (await sentToOneClick()).filter((r) => r.body.dry === false);
  expect(live).toHaveLength(1);
  expect(live[0]!.body).toMatchObject({
    swapType: 'EXACT_OUTPUT',
    amount: '1050000',
    recipient: VAULT,
    refundTo: '0x00000000000000000000000000000000000000aa',
  });
  expect(JSON.parse(live[0]!.body.customRecipientMsg as string)).toEqual({
    commitment,
    withdraw_to: OTHER,
  });
  await expect(page.getByTestId('intents-cost')).toContainText('25 basis points');
  await expect(page.getByTestId('intents-cost')).toContainText('1.065586');
  await expect(page.getByTestId('intents-fees')).toContainText('no other fees');
  await expect(page.getByTestId('intents-trust')).toContainText('hold your funds');
  await expect(page.getByTestId('intents-deadline')).not.toBeEmpty();
  await expect(page.getByTestId('intents-address')).toHaveCount(0);

  // The address appears only after the last checks, and the page sends nothing itself.
  await page.getByTestId('intents-show-address').click();
  const address = (await page.getByTestId('intents-address').textContent()) ?? '';
  expect(address).toMatch(/^fake-deposit-/);
  await expect(page.getByTestId('intents-progress')).toContainText('Waiting for your payment');

  // The "payment": the fake 1Click reports SUCCESS and the stand-in delivers the deposit to the vault.
  const event = {
    standard: 'nymira',
    version: '1.0.0',
    event: 'deposit',
    data: {
      note_id: 0,
      commitment,
      denomination: '1000000',
      expiry: expectedExpiry(),
      withdraw_to: OTHER,
    },
  };
  await control({
    cmd: 'set_tx_status',
    status: {
      tx_hash: DEST_TX,
      signer_id: 'intents.near',
      receiver_id: TOKEN,
      nonce: 1,
      receipts: [
        { executor_id: TOKEN, logs: [] },
        { executor_id: VAULT, logs: [`EVENT_JSON:${JSON.stringify(event)}`] },
        { executor_id: TOKEN, logs: [] },
      ],
    },
  });
  await fetch(`${ONECLICK}/mock/pay`, {
    method: 'POST',
    body: JSON.stringify({ depositAddress: address, txHash: DEST_TX }),
  });

  await expect(page.getByTestId('intents-accepted')).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId('intents-note-id')).toHaveText('#0');
  await expect(page.getByTestId('intents-explorer-link')).toHaveAttribute(
    'href',
    `https://testnet.nearblocks.io/txns/${DEST_TX}`,
  );
  await expect(page.getByTestId('intents-accepted')).toContainText(OTHER);
  const [ics] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('intents-calendar').click(),
  ]);
  const calendar = await readFile((await ics.path())!, 'utf8');
  expect(calendar).toContain('SUMMARY:Prepaid note expires today');
  for (const word of ['nymira', 'vault', 'usdc', OTHER, commitment]) {
    expect(calendar.toLowerCase()).not.toContain(word.toLowerCase());
  }

  // The secret is nowhere in the page, storage or any request, and 1Click never saw it.
  expectNoSecret(await pageSurfaces(page), secret);
  expectNoSecret([JSON.stringify(await sentToOneClick())], secret);
  const surfaces = (await pageSurfaces(page)).join('\n');
  expect(surfaces).toContain('"phase":"accepted"');
  // The quote record stays on this device, sealed: the stored copy does not show the address.
  const stored = (await pageSurfaces(page)).filter((s) => s.startsWith('idb:')).join('\n');
  expect(stored).not.toContain(address);

  // A chat on the new note.
  await scriptTree({ deposit: { commitment, note_id: 0, expiry: expectedExpiry() } });
  await openChat(page);
  await startConversation(page);
  await say(page, 'hello from an Intents-funded note');
  await expect(page.getByTestId('conversation')).toContainText('echo: hello from an Intents');
  await endConversation(page);

  // The privacy panel adds 1Click and the Intents explorer to the observers of this note.
  await page.getByTestId('chat-privacy').click();
  await expect(page.getByTestId('observers')).toContainText(
    'If the note was funded through Intents, 1Click and the Intents explorer can pair',
  );
});

// ---- expired quote, resume after a reload, restore from the backup, outcomes ----

const REFUND = '0x00000000000000000000000000000000000000aa';

/** Drives the flow up to the signed quote on screen; returns the backup file and the commitment. */
async function toReview(page: Page, context: BrowserContext, twoStep = false) {
  const wallet = await newFundingWallet(context);
  await fetch(`${ONECLICK}/requests`, { method: 'DELETE' });
  await openFund(page, 'other');
  await expect(page.getByTestId('vault-ok')).toBeVisible();
  if (twoStep) {
    await page.getByTestId('intents-path-two-step').check();
    await connectWallet(page);
  }
  await setUpPassphrase(page, undefined, 'intents-prepare');
  await page.getByTestId('intents-load-sources').click();
  const sources = page.getByTestId('intents-source');
  await expect(sources).toHaveCount(2, { timeout: 40_000 });
  await sources.first().check();
  await page.getByTestId('intents-refund').fill(REFUND);
  await page.getByTestId('intents-payout').fill(twoStep ? SENDER : OTHER);
  await expect(page.getByTestId('intents-fingerprint')).not.toBeEmpty();
  await page.getByTestId('intents-fingerprint-confirm').check();
  await page.getByTestId('intents-prepare').click();
  await page.getByTestId('intents-backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('intents-backup-download').click(),
  ]);
  const file = join(workdir(), 'intents.nymira-backup');
  await download.saveAs(file);
  await page.getByTestId('intents-backup-file').setInputFiles(file);
  await expect(page.getByTestId('intents-backup-tested')).toBeVisible();
  await page.getByTestId('intents-quote').click();
  await expect(page.getByTestId('intents-review')).toBeVisible();
  const commitment = (await readBackup(file)).notes[0]?.commitment ?? '';
  return { file, commitment, wallet };
}

/** The destination transaction the stand-in delivers, and 1Click's SUCCESS for the order. */
async function deliver(address: string, logs: string[]) {
  await control({
    cmd: 'set_tx_status',
    status: {
      tx_hash: DEST_TX,
      signer_id: 'intents.near',
      receiver_id: TOKEN,
      nonce: 1,
      receipts: [
        { executor_id: TOKEN, logs: [] },
        { executor_id: VAULT, logs },
        { executor_id: TOKEN, logs: [] },
      ],
    },
  });
  await fetch(`${ONECLICK}/mock/pay`, {
    method: 'POST',
    body: JSON.stringify({ depositAddress: address, txHash: DEST_TX }),
  });
}

const eventLine = (event: string, data: Record<string, unknown>) =>
  `EVENT_JSON:${JSON.stringify({ standard: 'nymira', version: '1.0.0', event, data })}`;

const depositLine = (commitment: string) =>
  eventLine('deposit', {
    note_id: 0,
    commitment,
    denomination: '1000000',
    expiry: expectedExpiry(),
    withdraw_to: OTHER,
  });

test('an expired quote stops before the address and a new quote goes on', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await toReview(page, context);
  // The deadline passes while the quote is on screen.
  await page.clock.setFixedTime(Date.now() + 2 * 3600 * 1000);
  await page.getByTestId('intents-show-address').click();
  await expect(page.getByTestId('intents-error')).toContainText('the quote expired');
  await expect(page.getByTestId('intents-address')).toHaveCount(0);
  await expect(page.getByTestId('intents-review')).toHaveCount(0);

  await page.getByTestId('intents-requote').click();
  await expect(page.getByTestId('intents-review')).toBeVisible();
  await expect(page.getByTestId('intents-error')).toHaveCount(0);
  expect((await sentToOneClick()).filter((r) => r.body.dry === false)).toHaveLength(2);
  await page.getByTestId('intents-show-address').click();
  await expect(page.getByTestId('intents-address')).toContainText('fake-deposit-');
});

test('Intents funding resumes after a reload and confirms the note', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { commitment } = await toReview(page, context);
  await page.getByTestId('intents-show-address').click();
  const address = (await page.getByTestId('intents-address').textContent()) ?? '';
  expect(address).toMatch(/^fake-deposit-/);

  // The tab is reloaded before the payment lands: the pending funding is sealed in IndexedDB.
  const stored = (await pageSurfaces(page)).filter((s) => s.startsWith('idb:')).join('\n');
  expect(stored).not.toContain(address);
  await page.reload();
  await enterApp(page);
  await lockAfterDevReload(page);
  await page.getByTestId('unlock-passphrase').fill(WRAP_PASSPHRASE);
  await page.getByTestId('unlock-submit').click();
  await expect(page.getByTestId('intents-address')).toHaveText(address, { timeout: 30_000 });
  await expect(page.getByTestId('intents-progress')).toContainText('Waiting for your payment');
  // Resuming asks for nothing new: no second live quote.
  expect((await sentToOneClick()).filter((r) => r.body.dry === false)).toHaveLength(1);

  await deliver(address, [depositLine(commitment)]);
  await expect(page.getByTestId('intents-accepted')).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId('intents-note-id')).toHaveText('#0');
});

test('Intents funding with local state cleared: restore the backup saved before payment', async ({
  browser,
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { file, commitment } = await toReview(page, context);
  await page.getByTestId('intents-show-address').click();
  const address = (await page.getByTestId('intents-address').textContent()) ?? '';
  await page.close();
  await deliver(address, [depositLine(commitment)]);

  // A browser with nothing in it, the backup file, and the destination transaction from 1Click.
  const fresh = await browser.newContext({ acceptDownloads: true });
  const page2 = await fresh.newPage();
  await page2.goto('/');
  await page2.getByTestId('open-app').click();
  await page2.getByTestId('nav-notes').click();
  await page2.getByTestId('setup-passphrase').fill(WRAP_PASSPHRASE);
  await page2.getByTestId('setup-confirm').fill(WRAP_PASSPHRASE);
  await page2.getByTestId('setup-submit').click();
  await expect(page2.getByTestId('no-notes')).toBeVisible();
  await page2.getByTestId('notes-backup-passphrase').fill(BACKUP_PASSPHRASE);
  await page2.getByTestId('restore-file').setInputFiles(file);
  await expect(page2.getByTestId('notes-message')).toContainText('Restored 1 note');
  await page2.getByTestId('lookup-hash').fill(DEST_TX);
  await page2.getByTestId('lookup').click();
  await expect(page2.getByTestId('lookup-result')).toContainText('Found: the deposit was accepted');
  await expect(page2.getByTestId('note-row')).toHaveAttribute('data-phase', 'accepted');
  await fresh.close();
});

test('SUCCESS with no deposit event confirms nothing; an unclaimed booking is explained', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await toReview(page, context);
  await page.getByTestId('intents-show-address').click();
  const address = (await page.getByTestId('intents-address').textContent()) ?? '';
  await expect(page.getByTestId('intents-route')).toContainText('fund with NEAR USDC directly');

  await deliver(address, []);
  await expect(page.getByTestId('intents-pending')).toContainText('no unique deposit event', {
    timeout: 60_000,
  });
  await expect(page.getByTestId('intents-accepted')).toHaveCount(0);

  // The vault could not read the message: the whole amount went to the treasury balance.
  await deliver(address, [
    eventLine('unclaimed', { amount: '1050000', reason: 'intents_bad_msg' }),
  ]);
  await expect(page.getByTestId('intents-booking')).toContainText('went to the treasury balance', {
    timeout: 60_000,
  });
  await expect(page.getByTestId('intents-booking')).toContainText('No note was confirmed');
  await expect(page.getByTestId('intents-claim')).toHaveCount(0);
  await expect(page.getByTestId('intents-accepted')).toHaveCount(0);
});

test('a payout the vault could not send shows the claim action', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await toReview(page, context);
  await page.getByTestId('intents-show-address').click();
  const address = (await page.getByTestId('intents-address').textContent()) ?? '';
  await deliver(address, [
    eventLine('payout_pending', {
      account: OTHER,
      amount: '1050000',
      reason: 'transfer_failed',
    }),
  ]);
  await expect(page.getByTestId('intents-claim')).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId('intents-booking')).toContainText(`for ${OTHER}`);
  await expect(page.getByTestId('intents-accepted')).toHaveCount(0);
});

// ---- two-step path: 1Click pays the user's own account, the wallet pays the vault ----

/** 1Click's SUCCESS for the first step, and the USDC (or none) in the user's account. */
async function firstStepDone(address: string, balance: string) {
  await fetch(`${ONECLICK}/mock/pay`, {
    method: 'POST',
    body: JSON.stringify({ depositAddress: address, txHash: DEST_TX }),
  });
  await control({
    cmd: 'set_view',
    account: TOKEN,
    method: 'ft_balance_of',
    args: { json: { account_id: SENDER } },
    result: { json: balance },
  });
}

test('Intents funding, two-step path: quote to my account, wait for the USDC, sign, accepted, chat', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { commitment, wallet } = await toReview(page, context, true);
  await expect(page.getByTestId('intents-two-step-notice')).toContainText(
    'needs a little NEAR for gas on your account and an account registered for USDC',
  );
  await expect(page.getByTestId('intents-privacy')).toContainText(
    'Your NEAR account appears on chain next to the deposit.',
  );
  // The live quote pays the account of the wallet with a plain transfer: no message.
  const live = (await sentToOneClick()).filter((r) => r.body.dry === false);
  expect(live).toHaveLength(1);
  expect(live[0]!.body).toMatchObject({ amount: '1050000', recipient: SENDER });
  expect('customRecipientMsg' in live[0]!.body).toBe(false);

  await page.getByTestId('intents-show-address').click();
  const address = (await page.getByTestId('intents-address').textContent()) ?? '';
  await expect(page.getByTestId('intents-sign')).toHaveCount(0);

  // 1Click delivered, but the USDC is not in the account yet.
  await firstStepDone(address, '0');
  await expect(page.getByTestId('intents-progress')).toContainText('Waiting for the USDC', {
    timeout: 30_000,
  });
  await expect(page.getByTestId('intents-sign')).toHaveCount(0);
  await control({
    cmd: 'set_view',
    account: TOKEN,
    method: 'ft_balance_of',
    args: { json: { account_id: SENDER } },
    result: { json: '1050000' },
  });
  await expect(page.getByTestId('intents-sign')).toBeVisible({ timeout: 30_000 });

  await page.getByTestId('intents-sign').click();
  await expect(page.getByTestId('intents-accepted')).toBeVisible({ timeout: 60_000 });
  // The wallet signed ft_transfer_call to the token for the vault; payout = sender, so no withdraw_to.
  expect(wallet.calls).toHaveLength(1);
  expect(wallet.calls[0]!.receiverId).toBe(TOKEN);
  expect(wallet.calls[0]!.args).toMatchObject({ receiver_id: VAULT, amount: '1050000' });
  expect(JSON.parse(wallet.calls[0]!.args.msg as string)).toEqual({ commitment });
  await expect(page.getByTestId('intents-explorer-link')).toHaveAttribute(
    'href',
    `https://testnet.nearblocks.io/txns/${wallet.lastHash}`,
  );

  await scriptTree({ deposit: { commitment, note_id: 0, expiry: expectedExpiry() } });
  await openChat(page);
  await startConversation(page);
  await say(page, 'hello from a two-step note');
  await expect(page.getByTestId('conversation')).toContainText('echo: hello from a two-step');
  await endConversation(page);
});

test('a refunded second step says the USDC is still in the account and deposits again', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { wallet } = await toReview(page, context, true);
  await page.getByTestId('intents-show-address').click();
  const address = (await page.getByTestId('intents-address').textContent()) ?? '';
  await firstStepDone(address, '1050000');
  await expect(page.getByTestId('intents-sign')).toBeVisible({ timeout: 30_000 });

  wallet.mode = 'refused';
  await page.getByTestId('intents-sign').click();
  await expect(page.getByTestId('intents-refunded')).toContainText(`still in ${SENDER}`, {
    timeout: 60_000,
  });
  await expect(page.getByTestId('intents-accepted')).toHaveCount(0);
  await expect(page.getByTestId('intents-sign')).toHaveText('Deposit again');

  wallet.mode = 'tree';
  await page.getByTestId('intents-sign').click();
  await expect(page.getByTestId('intents-accepted')).toBeVisible({ timeout: 60_000 });
  expect(wallet.calls).toHaveLength(2);
});

test('the two-step path is not offered to an account with no NEAR', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await newFundingWallet(context);
  await control({ cmd: 'set_account', account: SENDER, spec: { amount: '0' } });
  await openFund(page, 'other');
  await expect(page.getByTestId('vault-ok')).toBeVisible();
  await page.getByTestId('intents-path-two-step').check();
  await connectWallet(page);
  await setUpPassphrase(page, undefined, 'intents-prepare');
  await expect(page.getByTestId('intents-two-step-problem')).toContainText('too little NEAR');
  await expect(page.getByTestId('intents-load-sources')).toBeDisabled();
});
