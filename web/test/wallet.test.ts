import { generateKeyPairSync, sign } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import { appCsp, headersFor, walletFrameCsp } from '../headers';
import { b64encode } from '../src/crypto/bytes';
import { profile as mainnet } from '../src/profiles/mainnet';
import { profile as mock } from '../src/profiles/mock';
import { profile as testnet } from '../src/profiles/testnet';
import { base58Decode, nep413Preimage, verifyNep413 } from '../src/wallet/nep413';
import { pinForPath } from '../src/wallet/pinned-fetch';
import { isAccountId } from '../src/wallet/client';
import executors from '../pins/executors.json';

const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
function base58Encode(bytes: Uint8Array): string {
  let n = 0n;
  for (const b of bytes) n = (n << 8n) | BigInt(b);
  let out = '';
  while (n > 0n) {
    out = ALPHABET[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const b of bytes) {
    if (b !== 0) break;
    out = '1' + out;
  }
  return out;
}

describe('NEP-413 sign-in check', () => {
  const nonce = crypto.getRandomValues(new Uint8Array(32));
  const params = { message: 'hello', recipient: 'nymira.pages.dev', nonce };
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const raw = new Uint8Array(publicKey.export({ format: 'der', type: 'spki' }).subarray(-32));
  const pk = 'ed25519:' + base58Encode(raw);

  it('decodes base58', () => {
    expect([...base58Decode('2g')]).toEqual([0x61]);
    expect([...base58Decode('11')]).toEqual([0, 0]);
    expect(base58Decode(base58Encode(raw))).toEqual(raw);
  });

  it('accepts a valid signature and refuses a changed message, recipient or nonce', async () => {
    const hash = new Uint8Array(
      await crypto.subtle.digest('SHA-256', nep413Preimage(params) as BufferSource),
    );
    const signature = b64encode(new Uint8Array(sign(null, Buffer.from(hash), privateKey)));
    expect(await verifyNep413(params, pk, signature)).toBe('valid');
    expect(await verifyNep413({ ...params, message: 'other' }, pk, signature)).toBe('invalid');
    expect(await verifyNep413({ ...params, recipient: 'evil.example' }, pk, signature)).toBe(
      'invalid',
    );
    expect(await verifyNep413({ ...params, nonce: new Uint8Array(32) }, pk, signature)).toBe(
      'invalid',
    );
    expect(await verifyNep413(params, pk, 'AAAA')).toBe('invalid');
    expect(await verifyNep413(params, 'secp256k1:xyz', signature)).toBe('invalid');
  });
});

describe('executor pins', () => {
  it('map a request path to its pin and nothing else', () => {
    expect(pinForPath('/executors/meteor-wallet.js')?.sha256).toBe(
      executors.wallets['meteor-wallet'].sha256,
    );
    expect(pinForPath('/executors/intear-wallet.js')?.name).toBe('Intear Wallet');
    expect(pinForPath('/executors/unknown.js')).toBeNull();
    expect(pinForPath('/executors/../index.html')).toBeNull();
    expect(pinForPath('/other/meteor-wallet.js')).toBeNull();
  });

  it('are SHA-256 digests', () => {
    for (const pin of Object.values(executors.wallets))
      expect(pin.sha256).toMatch(/^[0-9a-f]{64}$/);
  });
});

describe('account ids', () => {
  it('accepts NEAR account ids and refuses the rest', () => {
    for (const ok of ['alice.near', 'a-b_c.testnet', 'x'.repeat(64), 'ab'])
      expect(isAccountId(ok)).toBe(true);
    for (const bad of ['', 'a', 'Alice.near', 'a..b', '.a', 'a.', 'a b', 'x'.repeat(65)]) {
      expect(isAccountId(bad)).toBe(false);
    }
  });
});

describe('profiles and headers', () => {
  it('the deployable profiles name no loopback address and fix their network', () => {
    for (const p of [testnet, mainnet]) {
      expect(JSON.stringify(p)).not.toMatch(/127\.0\.0\.1|localhost/);
      expect(p.appOrigin).toBe('https://nymira.pages.dev');
    }
    expect(testnet.network).toBe('testnet');
    expect(mainnet.network).toBe('mainnet');
    expect(testnet.tokenAccount).toBe(
      '3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af',
    );
    expect(testnet.faucetUrl).toBe('https://faucet.circle.com');
    expect(mainnet.faucetUrl).toBeNull();
  });

  it('the app policy has no inline script, no eval and connects only where the profile says', () => {
    const csp = appCsp(testnet);
    expect(csp).not.toContain("'unsafe-eval'");
    expect(csp).toContain("script-src 'self' 'wasm-unsafe-eval'");
    expect(csp).not.toMatch(/script-src[^;]*unsafe-inline/);
    expect(csp).not.toMatch(/style-src[^;]*unsafe-inline/);
    expect(csp).toContain("default-src 'none'");
    expect(csp).toContain('https://rpc.testnet.near.org');
    expect(csp).toContain('https://1click.chaindefuser.com');
    expect(appCsp(mainnet)).not.toContain('rpc.testnet');
    expect(csp).toContain("frame-ancestors 'none'");
  });

  it('only the wallet frame gets the relaxed policy, and it cannot be embedded elsewhere', () => {
    expect(walletFrameCsp(mock)).toContain("'unsafe-inline'");
    expect(walletFrameCsp(mock)).toContain("frame-ancestors 'self'");
    expect(headersFor(mock, '/wallet-host.html')['Content-Security-Policy']).toBe(
      walletFrameCsp(mock),
    );
    expect(headersFor(mock, '/')['Content-Security-Policy']).toBe(appCsp(mock));
    expect(headersFor(mock, '/assets/x.js')['Access-Control-Allow-Origin']).toBe('*');
    expect(headersFor(mock, '/')['Access-Control-Allow-Origin']).toBeUndefined();
  });
});
