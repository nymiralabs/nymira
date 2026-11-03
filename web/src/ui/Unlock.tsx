import { useEffect, useState } from 'react';
import { ARGON2, MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import {
  createPassphraseWrap,
  createPrfWrap,
  prfPossible,
  unlockWithPassphrase,
  unlockWithPrf,
  wrapMethod,
} from '../crypto/wrap';

type Method = 'passphrase' | 'webauthn-prf' | null;

export const ARGON2_SENTENCE = `Argon2id, ${ARGON2.m / 1024} MiB memory, ${ARGON2.t} passes, ${ARGON2.p} lane`;

/** Creates or unlocks the key that wraps note secrets. `onReady` fires with the key in memory. */
export function Unlock({ onReady }: { onReady: () => void }) {
  const [method, setMethod] = useState<Method | undefined>(undefined);
  const [passphrase, setPassphrase] = useState('');
  const [confirm, setConfirm] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  useEffect(() => {
    wrapMethod().then(setMethod, () => setError('IndexedDB is not available in this browser.'));
  }, []);

  const run = async (action: () => Promise<boolean | void>) => {
    setBusy(true);
    setError('');
    try {
      if ((await action()) === false) setError('Wrong passphrase.');
      else onReady();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  if (method === undefined) return <p>Loading…</p>;

  if (method === 'webauthn-prf') {
    return (
      <section data-testid="unlock">
        <h2>Unlock your notes</h2>
        <p>Your notes are protected by a passkey on this device.</p>
        <button
          type="button"
          disabled={busy}
          data-testid="unlock-prf"
          onClick={() => run(unlockWithPrf)}
        >
          Unlock with passkey
        </button>
        {error && <p className="error">{error}</p>}
      </section>
    );
  }

  if (method === 'passphrase') {
    return (
      <section data-testid="unlock">
        <h2>Unlock your notes</h2>
        <label>
          Passphrase
          <input
            type="password"
            autoComplete="current-password"
            value={passphrase}
            data-testid="unlock-passphrase"
            onChange={(e) => setPassphrase(e.target.value)}
          />
        </label>
        <button
          type="button"
          disabled={busy || passphrase === ''}
          data-testid="unlock-submit"
          onClick={() => run(() => unlockWithPassphrase(passphrase))}
        >
          {busy ? 'Unlocking…' : 'Unlock'}
        </button>
        {error && <p className="error">{error}</p>}
      </section>
    );
  }

  const longEnough = passphraseLength(passphrase) >= MIN_PASSPHRASE_LENGTH;
  return (
    <section data-testid="setup-wrap">
      <h2>Protect your notes</h2>
      <p>
        The note secret is created in this browser and never leaves it. It is encrypted before it is
        stored, with a key that cannot be exported from the browser. Choose how to derive that key.
        It lives in memory while this page is open.
      </p>
      <label>
        Passphrase (at least {MIN_PASSPHRASE_LENGTH} characters)
        <input
          type="password"
          autoComplete="new-password"
          value={passphrase}
          data-testid="setup-passphrase"
          onChange={(e) => setPassphrase(e.target.value)}
        />
      </label>
      <label>
        Repeat the passphrase
        <input
          type="password"
          autoComplete="new-password"
          value={confirm}
          data-testid="setup-confirm"
          onChange={(e) => setConfirm(e.target.value)}
        />
      </label>
      <p className="hint">
        {ARGON2_SENTENCE}. If you lose the passphrase and the backup, the note cannot be recovered.
      </p>
      <button
        type="button"
        className="primary"
        disabled={busy || !longEnough || passphrase !== confirm}
        data-testid="setup-submit"
        onClick={() => run(() => createPassphraseWrap(passphrase))}
      >
        {busy ? 'Deriving the key…' : 'Use this passphrase'}
      </button>
      {prfPossible() && (
        <>
          <p>Or use a passkey on this device, if its authenticator supports the PRF extension.</p>
          <button
            type="button"
            disabled={busy}
            data-testid="setup-prf"
            onClick={() => run(createPrfWrap)}
          >
            Use a passkey
          </button>
        </>
      )}
      {error && (
        <p className="error" data-testid="setup-error">
          {error}
        </p>
      )}
    </section>
  );
}
