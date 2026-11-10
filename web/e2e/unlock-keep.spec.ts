import { expect, test } from '@playwright/test';
import { WRAP_PASSPHRASE, enterApp } from './support';

async function openChat(page: import('@playwright/test').Page) {
  await enterApp(page);
  await page.getByTestId('nav-chat').click();
}

test('stay unlocked in this tab: a reload keeps the notes open, and the default does not', async ({
  browser,
}) => {
  const kept = await browser.newContext();
  const page = await kept.newPage();
  await page.goto('/');
  await openChat(page);
  await page.getByTestId('keep-unlocked').check();
  await page.getByTestId('setup-passphrase').fill(WRAP_PASSPHRASE);
  await page.getByTestId('setup-confirm').fill(WRAP_PASSPHRASE);
  await page.getByTestId('setup-submit').click();
  await expect(page.getByTestId('chat-screen')).toBeVisible();
  await page.reload();
  await openChat(page);
  await expect(page.getByTestId('chat-screen')).toBeVisible();
  await expect(page.getByTestId('unlock')).toHaveCount(0);
  // Locking by hand removes the kept key.
  await page.getByTestId('lock-notes').click();
  await page.reload();
  await openChat(page);
  await expect(page.getByTestId('unlock')).toBeVisible();
  await kept.close();

  const plain = await browser.newContext();
  const other = await plain.newPage();
  await other.goto('/');
  await openChat(other);
  await other.getByTestId('setup-passphrase').fill(WRAP_PASSPHRASE);
  await other.getByTestId('setup-confirm').fill(WRAP_PASSPHRASE);
  await other.getByTestId('setup-submit').click();
  await expect(other.getByTestId('chat-screen')).toBeVisible();
  await other.reload();
  await openChat(other);
  await expect(other.getByTestId('unlock')).toBeVisible();
  await plain.close();
});
