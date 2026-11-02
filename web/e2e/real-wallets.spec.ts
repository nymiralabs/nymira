import { expect, test } from '@playwright/test';
import { newFundingWallet, openFund } from './support';

// Opt-in: these load the real, vendored Meteor and Intear executors and reach the wallets' own
// servers over the internet, so they are not part of CI.
//   PROBE_REAL_WALLETS=1 npx playwright test --project=chromium real-wallets
// They go as far as the wallet's own login page. Signing in needs a real account and a person:
// that part is a manual check (web/README.md).
test.skip(!process.env.PROBE_REAL_WALLETS, 'set PROBE_REAL_WALLETS=1 to run');

test('Intear: the connect chooser shows inside the sandboxed frame under the production policy', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  const violations: string[] = [];
  page.on('console', (m) => {
    if (/Content Security Policy/.test(m.text())) violations.push(m.text());
  });
  await openFund(page);
  await page.getByTestId('connect-intear-wallet').click();
  // wallet frame > NEAR Connect's srcdoc frame > Intear's own page
  const frame = page
    .frameLocator('iframe.wallet-frame')
    .frameLocator('iframe')
    .frameLocator('iframe');
  await expect(frame.getByText('Connect Intear Wallet', { exact: false })).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.locator('iframe.wallet-frame')).toHaveClass(/active/);
  expect(violations.filter((v) => !/rsms\.me|cdnfonts/.test(v))).toEqual([]);
});

test('Meteor: the connect dialog shows, and Meteor Web opens the testnet login in a popup', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  const popups: string[] = [];
  context.on('page', (p) => popups.push(p.url()));
  await openFund(page);
  await page.getByTestId('connect-meteor-wallet').click();
  const frame = page.frameLocator('iframe.wallet-frame').frameLocator('iframe');
  await expect(frame.getByText('Meteor Web')).toBeVisible({ timeout: 30_000 });
  await frame.getByText('Meteor Web').click();
  await expect
    .poll(() => popups.join(' '), { timeout: 30_000 })
    .toContain('wallet.meteorwallet.app/connect/testnet/login');
});
