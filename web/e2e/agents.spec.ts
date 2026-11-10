import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { NearTestWallet, installTestWallet } from '@testkit';
import { verifyNep413 } from '../src/wallet/nep413';
import { RPC, SENDER, connectWallet, rpcRequests, scriptChain, watch, workdir } from './support';

const NOTE = '0x' + 'ab'.repeat(32);
const REPO = join(import.meta.dirname, '..', '..');

async function setUp(context: Parameters<typeof installTestWallet>[0]) {
  const wallet = new NearTestWallet({ accountId: SENDER, rpcUrl: RPC, network: 'testnet' });
  await scriptChain(wallet);
  await installTestWallet(context, wallet);
  return wallet;
}

async function openAgents(page: Page) {
  await page.goto('/');
  await page.getByTestId('open-app').click();
  await page.getByTestId('nav-agents').click();
  await expect(page.getByTestId('agents')).toBeVisible();
}

async function connect(page: Page) {
  await connectWallet(page);
}

async function fillAgent(page: Page) {
  await page.getByTestId('agent-name').fill('cursor');
  await page.getByTestId('agent-commitment').fill(NOTE);
}

/** nymirad's own code reads the file the page produced: `cargo test` with the file in the environment. */
function nymiradReads(kind: 'policy' | 'approval', file: string, message?: string) {
  const run = spawnSync(
    'cargo',
    ['test', '--locked', '-q', '-p', 'nymirad', '--test', 'web_signed', '--', '--nocapture'],
    {
      cwd: REPO,
      encoding: 'utf8',
      env: {
        ...process.env,
        NYMIRA_WEB_SIGNED_KIND: kind,
        NYMIRA_WEB_SIGNED_FILE: file,
        NYMIRA_WEB_AGENT: 'cursor',
        NYMIRA_WEB_NOTE: NOTE,
        NYMIRA_WEB_ACCOUNT: SENDER,
        NYMIRA_WEB_RPC: RPC,
        ...(message ? { NYMIRA_WEB_MESSAGE: message } : {}),
      },
    },
  );
  expect(run.stdout + run.stderr).toContain('test result: ok. 1 passed');
  expect(run.stdout + run.stderr).not.toContain('is not set');
}

test.describe('Agents screen', () => {
  test.setTimeout(900_000);

  test('a policy signed in the browser is accepted by nymirad', async ({ context, page }) => {
    await setUp(context);
    await openAgents(page);
    await connect(page);
    await fillAgent(page);
    await page.getByTestId('model-z-ai/glm-5.3-flash').check();
    await page.getByTestId('p-lease').fill('0.5');
    await page.getByTestId('p-call').fill('0.05');
    await page.getByTestId('p-daily').fill('2');
    await page.getByTestId('p-confirm').fill('0.03');
    await page.getByTestId('p-rate').fill('30');
    await page.getByTestId('p-version').fill('1');
    await expect(page.getByTestId('policy-yaml')).toHaveText(
      'allowed_models:\n  - z-ai/glm-5.3-flash\nmax_lease_usd: 0.5\nmax_call_usd: 0.05\nmax_daily_usd: 2\nconfirm_above_usd: 0.03\nrate_limit_per_min: 30\nrequire_tee: true\n',
    );
    await expect(page.getByTestId('p-tee')).toContainText('require_tee: true');
    await expect(page.getByTestId('policy-recipient')).toHaveText(`nymirad:cursor:${NOTE}`);
    await page.getByTestId('policy-sign').click();
    const text = await page.getByTestId('policy-file').innerText();
    const file = JSON.parse(text) as Record<string, string>;
    expect(file.account_id).toBe(SENDER);
    expect(file.recipient).toBe(`nymirad:cursor:${NOTE}`);
    expect(await page.getByTestId('policy-signature').innerText()).toBe(file.signature);
    expect(
      await verifyNep413(
        {
          message: file.message!,
          recipient: file.recipient!,
          nonce: Buffer.from(file.nonce!, 'base64'),
        },
        file.public_key!,
        file.signature!,
      ),
    ).toBe('valid');
    const download = page.waitForEvent('download');
    await page.getByTestId('policy-download').click();
    const saved = join(workdir(), (await download).suggestedFilename());
    await (await download).saveAs(saved);
    expect((await download).suggestedFilename()).toBe('cursor-policy.signed.json');
    expect(readFileSync(saved, 'utf8')).toBe(text.endsWith('\n') ? text : text + '\n');
    nymiradReads('policy', saved);
    await page.getByTestId('p-version').fill('2');
    await expect(page.getByTestId('policy-file')).toHaveCount(0);
    await expect(page.getByTestId('policy-download')).toHaveCount(0);
  });

  test('an approval signed in the browser carries the values copied from nymirad', async ({
    context,
    page,
  }) => {
    await setUp(context);
    await openAgents(page);
    await connect(page);
    await fillAgent(page);
    const held = {
      id: 'apv_' + '1f'.repeat(16),
      hash: 'ab'.repeat(32),
      amount: '42000',
      nonce: 'cd'.repeat(32),
    };
    await page.getByTestId('a-id').fill(held.id);
    await page.getByTestId('a-hash').fill(held.hash);
    await page.getByTestId('a-amount').fill(held.amount);
    await page.getByTestId('a-nonce').fill('not hex');
    await expect(page.getByTestId('approval-sign')).toHaveCount(0);
    await page.getByTestId('a-nonce').fill(held.nonce);
    await expect(page.getByTestId('approval-recipient')).toHaveText(
      `nymirad-approval:cursor:${NOTE}`,
    );
    await page.getByTestId('approval-sign').click();
    const text = await page.getByTestId('approval-file').innerText();
    const file = JSON.parse(text) as Record<string, string>;
    const message = JSON.parse(file.message!) as Record<string, string>;
    expect(message).toEqual({
      kind: 'nymira-approval',
      id: held.id,
      request_hash: held.hash,
      amount_micro: held.amount,
      nonce: held.nonce,
    });
    expect(await page.getByTestId('approval-signature').innerText()).toBe(file.signature);
    expect(
      await verifyNep413(
        {
          message: file.message!,
          recipient: file.recipient!,
          nonce: Buffer.from(file.nonce!, 'base64'),
        },
        file.public_key!,
        file.signature!,
      ),
    ).toBe('valid');
    const path = join(workdir(), 'approval.signed.json');
    writeFileSync(path, text);
    nymiradReads('approval', path, file.message);
    await page.getByTestId('a-amount').fill('43000');
    await expect(page.getByTestId('approval-file')).toHaveCount(0);
    await expect(page.getByTestId('approval-download')).toHaveCount(0);
  });

  test('the page makes no request to nymirad, creates no note and shows no token', async ({
    context,
    page,
  }) => {
    await setUp(context);
    const seen = watch(page);
    await openAgents(page);
    await connect(page);
    await fillAgent(page);
    await page.getByTestId('c-token').fill('/srv/agent/cursor.token');
    await page.getByTestId('c-key').fill('/home/me/key.json');
    await page.getByTestId('c-amount').fill('1000000');
    await page.getByTestId('c-host').fill('agent.example');
    await expect(page.getByTestId('block-create')).toBeVisible();
    await expect(page.getByTestId('block-forward')).toHaveText(
      'ssh -N -R 8788:localhost:8788 agent.example',
    );
    await page.getByTestId('copy-create').click();
    await expect(page.getByTestId('agents-scope')).toContainText('creates no budget note');
    // The same checks as above, with a policy signed on the way.
    await page.getByTestId('model-z-ai/glm-5.3-flash').check();
    for (const [id, v] of [
      ['p-lease', '0.5'],
      ['p-call', '0.05'],
      ['p-daily', '2'],
      ['p-confirm', '0.03'],
      ['p-rate', '30'],
      ['p-version', '1'],
    ] as const) {
      await page.getByTestId(id).fill(v);
    }
    await page.getByTestId('policy-sign').click();
    await expect(page.getByTestId('policy-file')).toBeVisible();

    const toNymirad = seen.requests.filter((r) =>
      /:8788|\/v1\/nymira|\/v1\/chat|localhost:\d+\/v1/.test(r),
    );
    expect(toNymirad).toEqual([]);
    // Only the fake chain (the vault check, the wallet) and this page's own files are asked.
    const hosts = new Set(seen.requests.map((r) => new URL(r.split(' ')[1]!).host));
    expect([...hosts].filter((h) => h !== 'localhost:4173' && h !== '127.0.0.1:18545')).toEqual([]);
    const posts = seen.requests.filter((r) => r.startsWith('POST'));
    expect(posts.every((r) => r.includes('127.0.0.1:18545'))).toBe(true);
    const methods = (await rpcRequests()).map((r) => r.method);
    expect(methods.filter((m) => /send_tx|broadcast/.test(m))).toEqual([]);
    const body = await page.locator('body').innerText();
    expect(body).not.toMatch(/nyma_|nymo_/);
    expect(await page.locator('input[type=password]').count()).toBe(0);
    expect(seen.violations).toEqual([]);
  });

  test('copy blocks quote hostile input, refuse control characters and read nothing from the URL', async ({
    context,
    page,
  }) => {
    await setUp(context);
    await page.goto(
      '/?name=evil&token-file=/etc/passwd&agent-host=evil.example&token=nyma_abc#c-token=/x&agent-name=evil',
    );
    await page.getByTestId('open-app').click();
    await page.getByTestId('nav-agents').click();
    for (const id of [
      'agent-name',
      'agent-commitment',
      'c-token',
      'c-key',
      'c-amount',
      'c-host',
      'c-endpoint',
    ]) {
      await expect(page.getByTestId(id)).toHaveValue('');
    }
    await expect(page.getByTestId('block-create')).toHaveCount(0);

    await page.getByTestId('agent-name').fill('cursor');
    await page.getByTestId('c-key').fill('/k');
    await page.getByTestId('c-amount').fill('1000000');
    const hostile = "/tmp/a'b $(id) `id`; rm -rf ~";
    await page.getByTestId('c-token').fill(hostile);
    await page.getByTestId('c-host').fill("h'; id #");
    const create = await page.getByTestId('block-create').innerText();
    expect(create).toBe(
      "nymirad agent create --name cursor --transport tcp --token-file '/tmp/a'\\''b $(id) `id`; rm -rf ~' --key-file /k --amount 1000000",
    );
    // A shell reads the quoted words back as the typed ones.
    const words = execFileSync('/bin/sh', [
      '-c',
      `set -- ${create}; for a; do printf '%s\\n' "$a"; done`,
    ]).toString('utf8');
    expect(words.split('\n')).toContain(hostile);
    const forward = await page.getByTestId('block-forward').innerText();
    expect(forward).toBe("ssh -N -R 8788:localhost:8788 'h'\\''; id #'");

    await page.getByTestId('c-token').fill('/tmp/a\u0007b');
    await expect(page.getByTestId('command-errors')).toContainText(
      'control characters are refused',
    );
    await expect(page.getByTestId('block-create')).toHaveCount(0);
    await page.getByTestId('c-token').fill('nyma_0123456789abcdef');
    await expect(page.getByTestId('command-errors')).toContainText('give a path, not a token');
    await page.getByTestId('c-token').fill('/srv/t');
    await page.getByTestId('c-host').fill('-oProxyCommand=id');
    await expect(page.getByTestId('command-errors')).toContainText('must not start with a dash');
  });
  test('the recipes are filled from the fields, have copy buttons and hold no token', async ({
    context,
    page,
  }) => {
    await setUp(context);
    await openAgents(page);
    await page.getByTestId('agent-name').fill('worker');
    await page.getByTestId('c-token').fill('/srv/agent/worker.token');
    await page.getByTestId('c-key').fill('/home/me/key');
    await page.getByTestId('c-amount').fill('1000000');
    await page.getByTestId('c-host').fill('agent.example');
    const ironclaw = page.getByTestId('recipe-ironclaw');
    const cursor = page.getByTestId('recipe-cursor');
    await expect(ironclaw.locator('pre').first()).toContainText('cat /srv/agent/worker.token');
    await expect(ironclaw).toContainText('nymirad agent disable --name worker');
    await expect(ironclaw.locator('pre').last()).toContainText('-R 8788:');
    await expect(ironclaw.locator('pre').last()).toContainText('agent.example');
    await expect(cursor.locator('pre').first()).toContainText('/srv/agent/worker.token');
    for (const recipe of [ironclaw, cursor]) {
      await recipe.locator('summary').first().click();
      await expect(recipe.getByRole('button', { name: 'Copy' }).first()).toBeVisible();
      expect(await recipe.innerText()).not.toMatch(/ny(?:ma|mo)_[A-Za-z0-9]/);
    }
    await page.getByTestId('c-token').fill('nyma_0123456789abcdef');
    await expect(page.getByTestId('recipe-errors')).toContainText('give a path, not a token');
    await expect(ironclaw).toHaveCount(0);
  });
});
