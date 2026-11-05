import { describe, expect, it } from 'vitest';
import { headerVerdict } from '../scripts/check-live.mjs';

describe('check-live header verdicts', () => {
  it('passes a header that matches the signed set', () => {
    expect(headerVerdict('/', 'content-security-policy', 'x', 'x')).toBeNull();
    expect(headerVerdict('/', 'access-control-allow-origin', null, null)).toBeNull();
  });

  it('accepts only the one difference Cloudflare Pages adds, as a note', () => {
    const v = headerVerdict('/', 'access-control-allow-origin', '*', null);
    expect(v).toHaveProperty('note');
    expect(
      headerVerdict('/wallet-host.html', 'access-control-allow-origin', '*', null),
    ).toHaveProperty('note');
  });

  it('still fails on every other difference', () => {
    // another value
    expect(
      headerVerdict('/', 'access-control-allow-origin', 'https://evil.example', null),
    ).toHaveProperty('problem');
    // another path
    expect(
      headerVerdict('/assets/app.js', 'access-control-allow-origin', null, '*'),
    ).toHaveProperty('problem');
    expect(headerVerdict('/other.html', 'access-control-allow-origin', '*', null)).toHaveProperty(
      'problem',
    );
    // the signed set names a value and the host answers another
    expect(
      headerVerdict('/', 'access-control-allow-origin', '*', 'https://nymira.pages.dev'),
    ).toHaveProperty('problem');
    // another header
    expect(headerVerdict('/', 'content-security-policy', 'x', 'y')).toHaveProperty('problem');
    expect(headerVerdict('/', 'content-security-policy', null, 'y')).toHaveProperty('problem');
    expect(headerVerdict('/', 'strict-transport-security', '*', null)).toHaveProperty('problem');
  });
});
