import { join } from 'node:path';
import { expect, test } from '@playwright/test';
import {
  ONECLICK,
  backUpAndTest,
  connectWallet,
  expectNoSecret,
  newFundingWallet,
  openFund,
  pageSurfaces,
  prepare,
  readBackup,
  watch,
  workdir,
} from './support';

test('testnet quotes: off by default, quote only, dummy refund, no note data, cached and rate limited', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  await fetch(`${ONECLICK}/requests`, { method: 'DELETE' });
  await openFund(page);
  await expect(page.getByTestId('quotes-notice')).toContainText('1Click');
  await expect(page.getByTestId('quotes-notice')).toContainText('IP address');
  await expect(page.getByTestId('quotes-notice')).toContainText('Tor Browser');
  await expect(page.getByTestId('quotes-toggle')).not.toBeChecked();
  await page.waitForTimeout(1500);
  expect(await (await fetch(`${ONECLICK}/requests`)).json()).toEqual([]);

  await page.getByTestId('quotes-toggle').check();
  const rows = page.getByTestId('quote-row');
  await expect(rows).toHaveCount(2, { timeout: 40_000 });
  for (const row of await rows.all()) await expect(row).toContainText('quote only');
  const sent = (await (await fetch(`${ONECLICK}/requests`)).json()) as {
    at: number;
    body: Record<string, unknown>;
  }[];
  expect(sent).toHaveLength(2);
  expect(sent[1]!.at - sent[0]!.at).toBeGreaterThanOrEqual(4900);
  for (const { body } of sent) {
    expect(body).toMatchObject({ dry: true, amount: '1050000' });
    expect(String(body.refundTo)).toMatch(/^0x0{39}1$|^1{32}$/);
    expect(JSON.stringify(body)).not.toMatch(/commitment|alice|vault|usdc\.test/);
  }
  // Turning it off and on again within a minute is answered from the cache.
  await page.getByTestId('quotes-toggle').uncheck();
  await page.getByTestId('quotes-toggle').check();
  await expect(rows).toHaveCount(2);
  expect(await (await fetch(`${ONECLICK}/requests`)).json()).toHaveLength(2);
  // No text says the cross-chain payment was tested on testnet.
  await expect(page.getByTestId('quotes')).toContainText('not available on testnet');
  await expect(page.getByTestId('quotes')).not.toContainText(/was tested|tested on testnet/);
});

test('a wallet executor loads in the sandboxed frame, and nothing is fetched at run time', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  const seen = watch(page);
  await openFund(page);
  await expect(page.getByTestId('connect-fake-wallet')).toBeVisible();
  // The frame is sandboxed without allow-same-origin.
  const sandbox = await page.locator('iframe.wallet-frame').getAttribute('sandbox');
  expect(sandbox).toContain('allow-scripts');
  expect(sandbox).not.toContain('allow-same-origin');

  // A real sandboxed executor: NEAR Connect loads it, the app checks its NEP-413 signature.
  await page.getByTestId('connect-fake-wallet').click();
  await expect(page.getByTestId('connected')).toContainText('sandbox.test');
  await expect(page.getByTestId('connected')).toContainText('sign-in signature checked');
  for (const url of seen.requests) {
    expect(url).not.toMatch(/raw\.githubusercontent|jsdelivr|near-selector|manifest\.json/);
  }
  expect(seen.violations).toEqual([]);
});

test('an executor whose bytes differ from the pin is not offered', async ({ context, page }) => {
  await newFundingWallet(context);
  await page.route('**/executors/fake-wallet.js', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'text/javascript',
      body: 'window.selector.ready({})',
    }),
  );
  await openFund(page);
  await expect(page.getByTestId('wallet-skipped')).toContainText(
    'does not match its pinned SHA-256',
  );
  await expect(page.getByTestId('connect-fake-wallet')).toHaveCount(0);
  // The injected test wallet is a different matter: it is the user's own extension.
  await expect(page.getByTestId('connect-nymira-test-wallet')).toBeVisible();
});

test('a wallet whose sign-in signature does not verify is refused', async ({ context, page }) => {
  await newFundingWallet(context);
  // The test wallet signs honestly; break the signature in the frame's reply.
  await page.addInitScript(() => {
    window.addEventListener(
      'message',
      (event) => {
        const data = event.data as {
          type?: string;
          ok?: boolean;
          result?: { signedMessage?: { signature: string } };
        };
        if (data?.type === 'reply' && data.ok && data.result?.signedMessage) {
          data.result.signedMessage.signature = 'AAAA';
        }
      },
      true,
    );
  });
  await openFund(page);
  await page.getByTestId('connect-nymira-test-wallet').click();
  await expect(page.getByTestId('connect-error')).toContainText('signature');
  await expect(page.getByTestId('connected')).toHaveCount(0);
});

test('the app refuses to run from any origin but its own', async ({ context, page }) => {
  await newFundingWallet(context);
  const requests: string[] = [];
  page.on('request', (r) => requests.push(r.url()));
  await page.goto('http://127.0.0.1:4173/');
  await expect(page.getByTestId('origin-refused')).toContainText('http://localhost:4173');
  await expect(page.getByTestId('open-app')).toHaveCount(0);
  expect(requests.filter((u) => u.includes(':18545'))).toEqual([]);
});

test('the network, vault, RPC and operator cannot be changed by URL or storage', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  const hosts = new Set<string>();
  page.on('request', (r) => hosts.add(new URL(r.url()).host));
  await page.addInitScript(() => {
    for (const key of [
      'network',
      'profile',
      'mode',
      'rpc',
      'rpcUrl',
      'operator',
      'operatorUrl',
      'vault',
    ]) {
      localStorage.setItem(key, key === 'network' ? 'mainnet' : 'https://evil.test');
      sessionStorage.setItem(key, 'https://evil.test');
    }
  });
  await page.goto(
    '/?network=mainnet&mode=mainnet&rpc=https://evil.test&operator=https://evil.test&vault=evil.near&token=evil.near',
  );
  await page.getByTestId('open-app').click();
  await expect(page.getByTestId('network-label')).toContainText('testnet');
  await expect(page.getByTestId('vault-ok')).toBeVisible();
  await page.getByTestId('quotes-toggle').check();
  await expect(page.getByTestId('quote-row').first()).toBeVisible({ timeout: 20_000 });
  expect([...hosts].filter((h) => /evil/.test(h))).toEqual([]);
  expect([...hosts].sort()).toEqual(['127.0.0.1:18545', '127.0.0.1:18547', 'localhost:4173']);
});

test('a passkey with PRF wraps the notes and unlocks them after a reload', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  const cdp = await context.newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: {
      protocol: 'ctap2',
      ctap2Version: 'ctap2_1',
      transport: 'internal',
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      hasPrf: true,
    },
  });
  await openFund(page);
  await connectWallet(page);
  await page.getByTestId('setup-prf').click();
  await expect(page.getByTestId('prepare')).toBeVisible();
  await page.getByTestId('prepare').click();
  await expect(page.getByTestId('backup-panel')).toBeVisible();
  const backup = await backUpAndTest(page, join(workdir(), 'prf.nymira-backup'));
  const secret = (await readBackup(backup)).notes[0]?.secret ?? '';
  expectNoSecret(await pageSurfaces(page), secret);

  // After a reload the same passkey unlocks the same note store.
  await page.reload();
  await page.getByTestId('open-app').click();
  await page.getByTestId('nav-notes').click();
  await page.getByTestId('unlock-prf').click();
  await expect(page.getByTestId('note-row')).toHaveAttribute('data-phase', 'backed_up');
  await cdp.send('WebAuthn.removeVirtualAuthenticator', { authenticatorId });
});

test('an authenticator without PRF is refused with a clear message', async ({ context, page }) => {
  await newFundingWallet(context);
  const cdp = await context.newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: {
      protocol: 'ctap2',
      ctap2Version: 'ctap2_1',
      transport: 'internal',
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      hasPrf: false,
    },
  });
  await openFund(page);
  await page.getByTestId('setup-prf').click();
  await expect(page.getByTestId('setup-error')).toContainText('PRF');
});
