import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { decryptBackup } from '../src/backup/format';
import {
  endConversation,
  expect,
  fundNote,
  openChat,
  say,
  startConversation,
  test,
} from './chat-support';
import {
  BACKUP_PASSPHRASE,
  WRAP_PASSPHRASE,
  expectNoSecret,
  pageSurfaces,
  rpcRequests,
  watch,
  workdir,
  enterApp,
} from './support';
import type { Page } from '@playwright/test';

test.describe.configure({ timeout: 300_000 });

/** A stand-in for the File System Access picker: the page "writes" into `window.__written`. */
const FAKE_PICKER = () => {
  (window as unknown as { __written: number[][] }).__written = [];
  (window as unknown as { showSaveFilePicker: () => Promise<unknown> }).showSaveFilePicker =
    async () => ({
      createWritable: async () => {
        let bytes: number[] = [];
        return {
          write: async (data: Blob | Uint8Array) => {
            const buf = data instanceof Blob ? new Uint8Array(await data.arrayBuffer()) : data;
            bytes = Array.from(buf);
          },
          close: async () => {
            (window as unknown as { __written: number[][] }).__written.push(bytes);
          },
        };
      },
    });
};

const NO_PICKER = () => {
  delete (window as unknown as { showSaveFilePicker?: unknown }).showSaveFilePicker;
};

const written = (page: Page) =>
  page.evaluate(() => (window as unknown as { __written: number[][] }).__written);

async function newConversation(page: Page) {
  await page.getByRole('button', { name: 'New conversation' }).click();
  await startOn(page);
}
const startOn = (page: Page) => startConversation(page);

test('chat end to end: attested, encrypted, both signature kinds, token, cost, backup record, no leak', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await page.addInitScript(FAKE_PICKER);
  const seen = watch(page);
  const { backupFile } = await fundNote(page, context);
  void backupFile;
  await openChat(page);
  await startConversation(page);
  // The first proof is made while the user types: the tree was read and the nonce checked before
  // any message was sent, and nothing reached the operator's lease route yet.
  await expect
    .poll(async () => JSON.stringify(await rpcRequests()), { timeout: 60_000 })
    .toContain('get_leaves');
  expect(stack.seen.some((r) => r.path === '/v1/status')).toBe(true);
  expect(stack.seen.some((r) => r.method === 'POST')).toBe(false);
  await say(page, 'What is two plus two? my secret question');

  // The answer came back decrypted; the badge waits for the lease to end.
  await expect(page.getByTestId('conversation')).toContainText('echo: What is two plus two?');
  await expect(page.getByTestId('answer')).toHaveAttribute('data-trust', 'pending');
  await expect(page.getByTestId('attestation-line')).toContainText('Intel TDX quote verified');
  await expect(page.getByTestId('attestation-line')).toContainText('not verified in a browser');

  await endConversation(page);
  await expect(page.getByTestId('badge-text')).toHaveText('model-signed response');
  await expect(page.getByTestId('badge')).toHaveAttribute('data-kind', 'model_signed');
  await expect(page.getByTestId('encryption-line')).toContainText('end-to-end encrypted');
  await expect(page.getByTestId('tls-note')).toContainText('cannot bind the gateway’s TLS');
  await expect(page.getByTestId('cost-line')).toContainText('Lease cost');
  await expect(page.getByTestId('charge-warning')).toHaveCount(0);

  // The operator saw an encrypted body: no message text on the wire.
  expect(stack.wire.length).toBe(1);
  expect(stack.wire.join('')).not.toContain('two plus two');
  expect(stack.wire.join('')).not.toContain('secret question');

  // The lease token travelled on the chat call and the lease status call.
  const lease = [...stack.leases.values()][0]!;
  const withToken = stack.seen.filter(
    (r) => r.path === '/v1/chat/completions' || r.path === `/v2/openrouter/leases/${lease.id}`,
  );
  expect(withToken.length).toBeGreaterThanOrEqual(3);
  for (const r of withToken) expect(r.headers['x-nymira-lease-token']).toBe(lease.token);

  // The proof's time sits on the 60 s grid of the operator's signed time.
  expect(stack.requestTimes).toHaveLength(1);
  expect(stack.requestTimes[0]! % 60).toBe(0);
  expect(stack.statusTimes.some((t) => Math.floor(t / 60) * 60 === stack.requestTimes[0])).toBe(
    true,
  );

  // The backup prompt (a file picker exists here): choose once, later retires write by themselves.
  await expect(page.getByTestId('backup-prompt')).toBeVisible();
  await page.getByTestId('backup-passphrase').fill(BACKUP_PASSPHRASE);
  await page.getByTestId('backup-choose').click();
  // The file is written at once, not after the next lease, and the screen says so.
  await expect(page.getByTestId('backup-saved')).toBeVisible();
  await expect.poll(async () => (await written(page)).length).toBeGreaterThanOrEqual(1);

  // A second conversation, streamed: expected to come back gateway-signed.
  await newConversation(page);
  await page.getByTestId('stream').check();
  await say(page, 'Stream me a second question');
  await expect(page.getByTestId('conversation')).toContainText('echo: Stream me a second question');
  await endConversation(page);
  await expect(page.getByTestId('badge-text')).toHaveText('gateway-signed response');
  await expect(page.getByTestId('badge')).toHaveAttribute('data-kind', 'gateway_signed');
  await expect(page.getByTestId('tls-note')).toBeVisible();

  // The file written after the second retire carries both lease records, metadata only. The first
  // write came when the file was chosen, so the second one is the next.
  await expect.poll(async () => (await written(page)).length).toBeGreaterThanOrEqual(2);
  const files = await written(page);
  const plain = await decryptBackup(new Uint8Array(files.at(-1)!), BACKUP_PASSPHRASE);
  const note = plain.notes.find((n) => n.kind === 'state')!;
  const extra = JSON.parse(note.extra!) as {
    leases: Record<string, unknown>[];
  };
  expect(extra.leases).toHaveLength(2);
  const [first, second] = extra.leases as Record<string, any>[];
  expect(first).toMatchObject({
    leaseId: lease.id,
    leafCount: 1,
    boundClass: '100000',
    model: 'z-ai/glm-5.3-flash',
    trust: 'model_signed',
    sampling: { stream: false },
    requests: 1,
    messageCount: 1,
  });
  expect(first!.root).toMatch(/^0x[0-9a-f]{1,64}$/);
  expect(first!.snapshotBlock).toBeTruthy();
  expect(first!.promptTokens).toBeGreaterThan(0);
  expect(first!.charge).toMatch(/^\d+$/);
  expect(second).toMatchObject({ trust: 'gateway_signed', sampling: { stream: true } });
  expect(JSON.stringify(extra)).not.toContain('second question');
  expect(JSON.stringify(extra)).not.toContain('two plus two');

  // After a full run no plain note secret is in storage, the page, the console or any request.
  const state = JSON.parse(note.state!) as { secret_s: string };
  const surfaces = await pageSurfaces(page);
  expectNoSecret([...surfaces, ...seen.console, ...seen.requests, ...stack.wire], state.secret_s);
  expect(seen.violations).toEqual([]);
});

test('several messages in one conversation: each answer cap fits the operator\u2019s reservation', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await page.addInitScript(FAKE_PICKER);
  await fundNote(page, context);
  await openChat(page);
  await startConversation(page);
  for (const text of ['hi', 'and now a second one', 'a third, after two answers']) {
    await say(page, text);
    await expect(page.getByTestId('conversation')).toContainText(`echo: ${text}`);
  }
  await expect(page.getByTestId('chat-error')).toHaveCount(0);
  // The disabled switch keeps its real input out of sight: no stray box over the text.
  await expect(page.getByTestId('long-job-side')).toBeDisabled();
  await expect(page.getByTestId('long-job-side')).toHaveCSS('opacity', '0');
  await endConversation(page);
  // The sidebar's Save leads to the backup prompt and puts the cursor in it.
  await page.getByTestId('backup-save').click();
  await expect(page.locator('[data-testid="backup-prompt"] :focus')).toHaveCount(1);
  // On another screen the owed backup shows as a strip that leads to the backup form.
  await page.getByTestId('nav-notes').click();
  await expect(page.getByTestId('backup-owed')).toBeVisible();
  await page.getByTestId('backup-owed').getByRole('button', { name: 'Back up now' }).click();
  await expect(page.getByTestId('notes-backup-passphrase')).toBeFocused();
  // Downloading the backup there pays the debt: the strip and the sidebar follow.
  await page.getByTestId('notes-backup-passphrase').fill('a long passphrase for the file');
  await page.getByTestId('notes-backup-download').click();
  await expect(page.getByTestId('backup-owed')).toHaveCount(0);
  await expect(page.locator('.nc-backup[data-saved="true"]')).toBeVisible();
});

test('nothing is encrypted, no proof is made and no lease opens when a check fails', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await fundNote(page, context);
  await openChat(page);
  await startConversation(page);

  const leaseRequests = () =>
    stack.seen.filter((r) => r.method === 'POST' && r.path === '/v2/openrouter/leases').length;
  const attempt = async (text: string, expected: RegExp) => {
    await page.getByTestId('chat-input').fill(text);
    await page.getByTestId('chat-send').click();
    await expect(page.getByTestId('chat-error')).toContainText(expected, { timeout: 60_000 });
    expect(stack.wire).toHaveLength(0);
    expect(leaseRequests()).toBe(0);
    expect(stack.leases.size).toBe(0);
  };

  // Attestation: an out-of-date TCB, a quote that does not bind the model's key.
  stack.set({ tcb: 'OutOfDate' });
  await attempt('tcb', /attestation was refused \(tcb_status\)/);
  stack.set({ tcb: 'UpToDate', unboundKey: true });
  await attempt('unbound', /attestation was refused \(signer_binding\)/);
  await expect(page.getByTestId('chat-error')).toContainText('Nothing was encrypted');
  stack.set({ unboundKey: false });

  // The status nonce must verify under the vault's state key before a proof is made.
  stack.set({ badStatusSignature: true });
  await attempt('nonce', /does not verify against the vault’s state key/);
  stack.set({ badStatusSignature: false });

  // The operator's lease parameters must equal the vault's.
  stack.set({ params: { charge_step: '1' } });
  await attempt('params', /lease parameters differ from the vault’s \(charge_step\)/);
  stack.set({ params: { bound_classes: ['100000'] } });
  await attempt('classes', /lease parameters differ from the vault’s \(bound_classes\)/);
  stack.set({ params: {} });

  // An emergency notice stops new leases.
  stack.set({ emergency: 'state key leaked' });
  await attempt('emergency', /emergency notice: state key leaked/);
  stack.set({ emergency: '' });
});

test('puzzle, root_stale retry, charge warning, not verified, capacity and provider limits', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await page.addInitScript(NO_PICKER);
  await fundNote(page, context);
  await openChat(page);
  await startConversation(page);

  // Lease 1: hashcash, one root_stale answer, an inflated charge, a bad signature.
  stack.set({ puzzleDifficulty: 8, rootStale: 1, inflate: 30_000, signature: 'bad' });
  await say(page, 'first');
  const posts = stack.seen.filter((r) => r.method === 'POST' && r.path === '/v2/openrouter/leases');
  expect(posts).toHaveLength(2);
  for (const p of posts) expect(String(p.headers['x-nymira-puzzle'])).toMatch(/^[0-9a-f]{32}:\d+$/);
  // The retry is a new proof (a different request), made after the tree was read again.
  expect(posts[0]!.body).not.toBe(posts[1]!.body);
  expect(stack.requestTimes).toHaveLength(2);
  await endConversation(page);
  await expect(page.getByTestId('badge')).toHaveAttribute('data-kind', 'not_verified');
  await expect(page.getByTestId('badge-text')).toHaveText('not verified');
  await expect(page.getByTestId('badge')).toContainText('the signature does not verify');
  await expect(page.getByTestId('charge-warning')).toContainText('more than one charge step');

  // A backup download is prompted (no file picker in this run), and it carries the record.
  await expect(page.getByTestId('backup-prompt')).toContainText('nymirad or a Chromium browser');
  await page.getByTestId('backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('backup-download').click(),
  ]);
  const file = join(workdir(), 'after.nymira-backup');
  await download.saveAs(file);
  const plain = await decryptBackup(new Uint8Array(readFileSync(file)), BACKUP_PASSPHRASE);
  const extra = JSON.parse(plain.notes.find((n) => n.kind === 'state')!.extra!) as {
    leases: { warnings: string[]; trust: string }[];
  };
  expect(extra.leases).toHaveLength(1);
  expect(extra.leases[0]!.trust).toBe('not_verified');
  expect(extra.leases[0]!.warnings.join(' ')).toContain('more than one charge step');

  // Lease 2: no signature at all is "not verified" too; capacity and provider limits differ.
  stack.set({ puzzleDifficulty: 0, rootStale: 0, inflate: 0, signature: 'none' });
  await page.getByRole('button', { name: 'New conversation' }).click();
  await startConversation(page);
  stack.set({ chatLimit: 'operator' });
  await page.getByTestId('chat-input').fill('limit one');
  await page.getByTestId('chat-send').click();
  await expect(page.getByTestId('chat-error')).toContainText('at capacity');
  await expect(page.getByTestId('chat-error')).toContainText('not the model provider');
  stack.set({ chatLimit: 'provider' });
  await page.getByTestId('chat-input').fill('limit two');
  await page.getByTestId('chat-send').click();
  await expect(page.getByTestId('chat-error')).toContainText('rate limiting');
  await expect(page.getByTestId('chat-error')).toContainText('not the operator’s capacity');
  stack.set({ chatLimit: '' });
  await say(page, 'finally');
  await endConversation(page);
  await expect(page.getByTestId('badge-text')).toHaveText('not verified');
  await expect(page.getByTestId('badge')).toContainText('no signature for this answer');
  await expect(page.getByTestId('charge-warning')).toHaveCount(0);
});

test('@webkit chat on WebKit: model-signed badge and the backup download', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await fundNote(page, context);
  await openChat(page);
  await startConversation(page);
  await say(page, 'hello from webkit');
  await endConversation(page);
  await expect(page.getByTestId('badge-text')).toHaveText('model-signed response');
  // WebKit has no File System Access: the download prompt and the line that points elsewhere.
  const picker = await page.evaluate(() => 'showSaveFilePicker' in window);
  await expect(page.getByTestId('backup-prompt')).toContainText(
    picker ? 'Choose a file once' : 'nymirad or a Chromium browser',
  );
});

test('a lease left open by a closed or reloaded page is settled on the next visit', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await fundNote(page, context);
  await openChat(page);
  await startConversation(page);
  await say(page, 'left open on purpose');
  const lease = [...stack.leases.values()][0]!;

  // The page goes away without ending the conversation: it still asks the operator to end it.
  await page.reload();
  await expect.poll(() => lease.retired).toBe(true);

  await enterApp(page);
  await page.getByTestId('nav-chat').click();
  // A dev build with the mock profile keeps the tab unlocked across a reload: then no prompt shows.
  const field = page.getByTestId('unlock-passphrase');
  if (await field.isVisible({ timeout: 3000 }).catch(() => false)) {
    await field.fill(WRAP_PASSPHRASE);
    await page.getByTestId('unlock-submit').click();
  }
  await expect(page.getByTestId('note-pending')).toBeVisible();
  await expect(page.getByTestId('chat-use-note')).toHaveCount(0);
  await page.getByTestId('chat-settle').click();
  await expect(page.getByTestId('settled')).toBeVisible({ timeout: 120_000 });
  await expect(page.getByTestId('badge-text')).toHaveText('not verified');
  await expect(page.getByTestId('badge')).toContainText('was not open in this page');
  await expect(page.getByTestId('cost-line')).toContainText('Lease cost');
  // The charge was recomputed from the stored usage: no false overcharge warning.
  await expect(page.getByTestId('charge-warning')).toHaveCount(0);
});
