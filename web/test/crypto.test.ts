import { describe, expect, it } from 'vitest';
import { importAesKey, open, openJson, sealJson } from '../src/crypto/aead';
import { b64encode } from '../src/crypto/bytes';
import {
  ARGON2,
  MIN_PASSPHRASE_LENGTH,
  checkPassphrase,
  deriveKeyBytes,
  passphraseLength,
} from '../src/crypto/kdf';
import {
  BACKUP_FORMAT,
  BackupError,
  decryptBackup,
  encryptBackup,
  type BackupPlain,
} from '../src/backup/format';

const plain: BackupPlain = {
  format: BACKUP_FORMAT,
  version: 1,
  network: 'testnet',
  vault: 'vault.test',
  createdAt: 1,
  notes: [],
};

describe('passphrase rules', () => {
  it('uses the stated Argon2id parameters', () => {
    expect(ARGON2).toEqual({ alg: 'argon2id', m: 65536, t: 3, p: 1 });
  });

  it('refuses a passphrase under 12 characters and counts characters, not bytes', () => {
    expect(() => checkPassphrase('x'.repeat(MIN_PASSPHRASE_LENGTH - 1))).toThrow(/12 characters/);
    expect(() => checkPassphrase('x'.repeat(MIN_PASSPHRASE_LENGTH))).not.toThrow();
    // 6 emoji are 24 bytes but 6 characters.
    expect(passphraseLength('😀😀😀😀😀😀')).toBe(6);
    expect(() => checkPassphrase('😀😀😀😀😀😀')).toThrow();
  });

  it('derives the same key for the same inputs and another for another salt', async () => {
    const salt = new Uint8Array(16).fill(1);
    const a = await deriveKeyBytes('correct horse battery', salt);
    const b = await deriveKeyBytes('correct horse battery', salt);
    const c = await deriveKeyBytes('correct horse battery', new Uint8Array(16).fill(2));
    expect(b64encode(a)).toBe(b64encode(b));
    expect(b64encode(a)).not.toBe(b64encode(c));
    expect(a).toHaveLength(32);
  });
});

describe('sealing', () => {
  it('opens only with the same key and bound context', async () => {
    const key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    const other = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    const sealed = await sealJson(key, { secret: '0xabc' }, 'ctx-1');
    expect(await openJson(key, sealed, 'ctx-1')).toEqual({ secret: '0xabc' });
    await expect(open(key, sealed, 'ctx-2')).rejects.toThrow();
    await expect(open(other, sealed, 'ctx-1')).rejects.toThrow();
  });

  it('the app keeps non-extractable keys', async () => {
    const key = await importAesKey(crypto.getRandomValues(new Uint8Array(32)));
    expect(key.extractable).toBe(false);
    await expect(crypto.subtle.exportKey('raw', key)).rejects.toThrow();
  });
});

describe('backup file', () => {
  it('round-trips, states its parameters and holds no plaintext', async () => {
    const withNote: BackupPlain = {
      ...plain,
      notes: [
        {
          id: 'n1',
          network: 'testnet',
          vault: 'vault.test',
          token: 'usdc.test',
          denomination: '1000000',
          payout: 'alice.test',
          payoutExplicit: false,
          sender: 'alice.test',
          commitment: '0x' + '11'.repeat(32),
          phase: 'backed_up',
          createdAt: 1,
          updatedAt: 1,
          kind: 'pending',
          secret: '0x' + '22'.repeat(32),
        },
      ],
    };
    const file = await encryptBackup(withNote, 'a long enough passphrase');
    const text = new TextDecoder().decode(file);
    const envelope = JSON.parse(text);
    expect(envelope.format).toBe('nymira-backup');
    expect(envelope.kdf).toMatchObject({ alg: 'argon2id', m: 65536, t: 3, p: 1 });
    expect(text).not.toContain('22'.repeat(32));
    expect(await decryptBackup(file, 'a long enough passphrase')).toEqual(withNote);
  });

  it('rejects a wrong passphrase, a short one and a damaged file with clear errors', async () => {
    const file = await encryptBackup(plain, 'a long enough passphrase');
    await expect(decryptBackup(file, 'another long passphrase')).rejects.toThrow(BackupError);
    await expect(decryptBackup(file, 'short')).rejects.toThrow(/12 characters/);
    await expect(
      decryptBackup(new TextEncoder().encode('{"not":"it"}'), 'a long enough passphrase'),
    ).rejects.toThrow(/not a Nymira backup/);
  });

  it('refuses key derivation settings far above or below the app’s', async () => {
    const file = await encryptBackup(plain, 'a long enough passphrase');
    const envelope = JSON.parse(new TextDecoder().decode(file));
    for (const kdf of [{ m: ARGON2.m * 100 }, { m: 1024 }, { t: 1 }, { p: 8 }]) {
      const bad = { ...envelope, kdf: { ...envelope.kdf, ...kdf } };
      await expect(
        decryptBackup(new TextEncoder().encode(JSON.stringify(bad)), 'a long enough passphrase'),
      ).rejects.toThrow(/key derivation settings/);
    }
  });
});
