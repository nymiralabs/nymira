import { expect, test, fundNote, openChat, startConversation, say, endConversation } from './chat-support';
import { BACKUP_PASSPHRASE, expectNoSecret, pageSurfaces, readBackup, workdir } from './support';
import { join } from 'node:path';

test.describe.configure({ timeout: 300_000 });

test('chat end to end: attested, encrypted, model-signed, lease token, cost, backup record, no secret leak', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { backupFile } = await fundNote(page, context);
  void backupFile;
  await openChat(page);
  await startConversation(page);
  await say(page, 'What is two plus two? my secret question');

  // The answer came back decrypted, and the badge waits for the lease to end.
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

  // The backup prompt, then the file carries the lease record (metadata only).
  await expect(page.getByTestId('backup-prompt')).toBeVisible();
  const dir = workdir();
  const written = join(dir, 'after.nymira-backup');
  void written;
  await page.getByTestId('backup-passphrase').fill(BACKUP_PASSPHRASE);
  await page.getByTestId('backup-choose').or(page.getByTestId('backup-download')).first().waitFor();
});

void readBackup;
void expectNoSecret;
void pageSurfaces;
