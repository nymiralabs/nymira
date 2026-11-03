import { expect, test } from '@playwright/test';
import { newFundingWallet, openFund } from './support';

// The app is served with the production header set (headers.ts). These tests show that the set
// does what it says (eval is refused by leaving `'unsafe-eval'` out, which the header test checks:
// code run through the test driver is exempt from it). The release build turns the same set into the Pages headers; integrity.spec.ts tests that file.

test('the policy blocks an injected inline script, an injected style and a request to an unlisted host', async ({
  context,
  page,
}) => {
  await newFundingWallet(context);
  await openFund(page);
  const result = await page.evaluate(async () => {
    const out: Record<string, string> = {};
    // Trusted Types refuses the assignment itself; the policy would refuse the script anyway.
    try {
      const script = document.createElement('script');
      script.textContent = 'window.__injected = true';
      document.head.append(script);
    } catch {
      /* blocked */
    }
    out.inline = (window as unknown as { __injected?: boolean }).__injected ? 'ran' : 'blocked';
    try {
      await fetch('https://example.com/', { mode: 'no-cors' });
      out.fetch = 'sent';
    } catch {
      out.fetch = 'blocked';
    }
    const style = document.createElement('style');
    style.textContent = 'body { background: red }';
    document.head.append(style);
    out.style =
      getComputedStyle(document.body).backgroundColor === 'rgb(255, 0, 0)' ? 'applied' : 'blocked';
    return out;
  });
  expect(result).toEqual({ inline: 'blocked', fetch: 'blocked', style: 'blocked' });
});

test('the app and the wallet frame get different header sets', async ({ request }) => {
  const app = await request.get('/');
  const csp = app.headers()['content-security-policy'] ?? '';
  expect(csp).toContain("default-src 'none'");
  expect(csp).not.toContain("'unsafe-eval'");
  expect(csp).not.toMatch(/script-src[^;]*'unsafe-inline'/);
  expect(app.headers()['referrer-policy']).toBe('no-referrer');
  expect(app.headers()['permissions-policy']).toContain('camera=()');
  expect(app.headers()['access-control-allow-origin']).toBeUndefined();

  const frame = await request.get('/wallet-host.html');
  const frameCsp = frame.headers()['content-security-policy'] ?? '';
  expect(frameCsp).toContain("'unsafe-inline'");
  expect(frameCsp).toContain("frame-ancestors 'self'");
  expect(csp).toContain("frame-ancestors 'none'");
  expect(csp).not.toContain("'unsafe-inline'");
});

test('the wallet frame cannot reach the app’s storage', async ({ context, page }) => {
  await newFundingWallet(context);
  await openFund(page);
  await page.evaluate(() => localStorage.setItem('probe', 'app-value'));
  const frame = page.frames().find((f) => f.url().endsWith('/wallet-host.html'))!;
  const seen = await frame.evaluate(() => {
    try {
      return String(window.parent.localStorage.getItem('probe'));
    } catch (error) {
      return `blocked: ${(error as Error).name}`;
    }
  });
  expect(seen).toMatch(/^blocked/);
});
