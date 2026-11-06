import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
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
  TOKEN,
  VAULT,
  control,
  expectNoSecret,
  expectedExpiry,
  newFundingWallet,
  openFund,
  pageSurfaces,
  readBackup,
  setUpPassphrase,
  workdir,
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
  await openFund(page);
  await expect(page.getByTestId('vault-ok')).toBeVisible();
  await expect(page.getByTestId('intents-tor')).toContainText('must not go through Tor');
  await expect(page.getByTestId('intents-privacy')).toContainText(
    'Wait a while between depositing and your first chat.',
  );
  await expect(page.getByTestId('intents-terms')).toContainText('Terms for this path');
  // The NEAR USDC flow stays beside it.
  await expect(page.getByTestId('send')).toBeVisible();
  await setUpPassphrase(page);

  // Sources come from signed dry quotes and nothing was asked before the click.
  expect(await sentToOneClick()).toEqual([]);
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
  // The quote record stays on this device.
  expect(surfaces).toContain(`"depositAddress":"${address}"`);

  // A chat on the new note.
  await scriptTree({ deposit: { commitment, note_id: 0, expiry: expectedExpiry() } });
  await openChat(page);
  await startConversation(page);
  await say(page, 'hello from an Intents-funded note');
  await expect(page.getByTestId('conversation')).toContainText('echo: hello from an Intents');
  await endConversation(page);
});
