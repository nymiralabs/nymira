import { join } from 'node:path';
import { expect, test } from '@playwright/test';
import {
  VAULT,
  backUpAndTest,
  connectWallet,
  control,
  newFundingWallet,
  openFund,
  prepare,
  setUpPassphrase,
  workdir,
  type VaultScript,
} from './support';

const OTHER_HASH = '5RZrhDkCfWgrRyTnLAWvGP9aBFRFTLQ3tHQLphvcL5gH';

const CASES: [string, VaultScript, string][] = [
  ['code hash', { codeHash: OTHER_HASH }, 'the vault code hash'],
  [
    'verifying keys',
    { vkHashes: { request: '0x' + '00'.repeat(32), withdrawal: '0x' + '11'.repeat(32) } },
    'the verifying key hashes',
  ],
  [
    'access keys',
    { accessKeys: [{ public_key: 'ed25519:AAA', nonce: 0, permission: 'FullAccess' }] },
    'the access keys',
  ],
  [
    'state key',
    { configOverrides: { operator_pk: { x: '0x01', y: '0x02' } } },
    'config: operator_pk',
  ],
  [
    'clearance key',
    { configOverrides: { clearance_pk: { x: '0x03', y: '0x04' } } },
    'config: clearance_pk',
  ],
  [
    'bound classes',
    { configOverrides: { bound_classes: ['20000', '100000', '900000'] } },
    'config: bound_classes',
  ],
  ['charge step', { configOverrides: { charge_step: '20000' } }, 'config: charge_step'],
  ['minimum charge', { configOverrides: { min_charge: '1' } }, 'config: min_charge'],
  ['slot rate', { configOverrides: { slot_rate: 900 } }, 'config: slot_rate'],
  ['note fee', { configOverrides: { note_fee: '1' } }, 'config: note_fee'],
  ['token', { configOverrides: { token: 'fake-usdc.test' } }, 'config: token'],
];

for (const [name, script, expected] of CASES) {
  test(`tripwire: a changed ${name} stops funding and shows the warning`, async ({
    context,
    page,
  }) => {
    const wallet = await newFundingWallet(context, script);
    await openFund(page);
    await expect(page.getByTestId('vault-warning')).toContainText(expected);
    await expect(page.getByTestId('vault-warning')).toContainText('Funding and chat are stopped');
    await connectWallet(page);
    await setUpPassphrase(page).catch(() => undefined);
    await expect(page.getByTestId('prepare')).toBeDisabled();
    expect(wallet.sent).toHaveLength(0);
  });
}

test('tripwire: the TVL cap and current TVL may change without a warning', async ({
  context,
  page,
}) => {
  await newFundingWallet(context, { tvlCap: '50000000', currentTvl: '1000000' });
  await openFund(page);
  await expect(page.getByTestId('vault-ok')).toBeVisible();
});

test('tripwire: a vault that changes after load stops the deposit, before the wallet is asked', async ({
  context,
  page,
}) => {
  const wallet = await newFundingWallet(context);
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'late.nymira-backup'));
  // The vault is upgraded between page load and the click.
  await control({
    cmd: 'set_account',
    account: VAULT,
    spec: { code_hash: OTHER_HASH },
  });
  await page.getByTestId('send').click();
  await expect(page.getByTestId('fund-error')).toContainText(
    'the vault changed: the vault code hash',
  );
  expect(wallet.sent).toHaveLength(0);
});

test('an unreachable vault check stops funding too', async ({ context, page }) => {
  await newFundingWallet(context);
  await control({ cmd: 'set_failure', failure: { http: 503 } });
  await openFund(page);
  await expect(page.getByTestId('vault-warning')).toHaveAttribute('data-status', 'unreachable');
  await expect(page.getByTestId('prepare')).toHaveCount(0);
});

test('a full vault (TVL cap) stops the deposit with its own message', async ({ context, page }) => {
  const wallet = await newFundingWallet(context, { tvlCap: '1000000', currentTvl: '0' });
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'full.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('fund-error')).toContainText('vault is full');
  expect(wallet.sent).toHaveLength(0);
});
