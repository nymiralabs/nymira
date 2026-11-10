import { useEffect, useState, type ReactNode } from 'react';
import { Button, Strip, Waiting } from '../marketing/kit';
import './unlock.css';
import { ARGON2, MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import {
  createPassphraseWrap,
  createPrfWrap,
  prfPossible,
  unlockWithPassphrase,
  unlockWithPrf,
  keepUnlockedInTab,
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
  const [keep, setKeep] = useState(false);

  useEffect(() => {
    wrapMethod().then(setMethod, () => setError('IndexedDB is not available in this browser.'));
  }, []);

  const run = async (action: () => Promise<boolean | void>) => {
    setBusy(true);
    setError('');
    keepUnlockedInTab(keep);
    try {
      if ((await action()) === false) setError('Wrong passphrase.');
      else onReady();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const keepBox = (
    <label className="nc-keep">
      <input
        type="checkbox"
        data-testid="keep-unlocked"
        checked={keep}
        onChange={(e) => setKeep(e.target.checked)}
      />
      <span className="nc-keep-switch" aria-hidden="true" />
      <span className="nc-keep-text">
        <strong>Stay unlocked in this tab</strong>
        <small>
          Reloading this tab will not ask for the passphrase again. Closing the tab locks your
          notes. Do not use this on a shared computer.
        </small>
      </span>
    </label>
  );

  const card = (children: ReactNode, testId: string) => (
    <div className="nc-unlock">
      <section className="nc-unlock-card" data-testid={testId}>
        <div className="nc-lock-cells" aria-hidden="true">
          <Strip n={12} solid={busy ? 12 : 0} size="lg" />
        </div>
        {children}
        {error && (
          <p
            className="nc-msg"
            data-tone="err"
            data-testid={testId === 'setup-wrap' ? 'setup-error' : undefined}
          >
            {error}
          </p>
        )}
      </section>
    </div>
  );

  if (method === undefined) return <Waiting title="Opening the encrypted store" />;

  if (method === 'webauthn-prf') {
    return card(
      <>
        <h1>Unlock your notes</h1>
        <p className="nc-lead">Your notes are protected by a passkey on this device.</p>
        <div className="nc-actions">
          <Button primary disabled={busy} testId="unlock-prf" onClick={() => run(unlockWithPrf)}>
            Unlock with passkey
          </Button>
        </div>
      </>,
      'unlock',
    );
  }

  if (method === 'passphrase') {
    return card(
      <>
        <h1>Unlock your notes</h1>
        <p className="nc-lead">
          Your notes are encrypted in this browser. The key lives in memory while this page is open.
        </p>
        <label className="nc-field">
          <span>Passphrase</span>
          <input
            type="password"
            autoComplete="current-password"
            value={passphrase}
            data-testid="unlock-passphrase"
            onChange={(e) => setPassphrase(e.target.value)}
          />
        </label>
        {keepBox}
        <div className="nc-actions">
          <Button
            primary
            disabled={busy || passphrase === ''}
            testId="unlock-submit"
            onClick={() => run(() => unlockWithPassphrase(passphrase))}
          >
            {busy ? 'Unlocking…' : 'Unlock'}
          </Button>
        </div>
      </>,
      'unlock',
    );
  }

  const longEnough = passphraseLength(passphrase) >= MIN_PASSPHRASE_LENGTH;
  return card(
    <>
      <h1>Protect your notes</h1>
      <p className="nc-lead">
        The note secret is created in this browser. It leaves only inside an encrypted backup you
        download. It is encrypted before it is stored, with a key that cannot be exported from the
        browser. Choose how to derive that key. It lives in memory while this page is open.
      </p>
      <label className="nc-field">
        <span>Passphrase (at least {MIN_PASSPHRASE_LENGTH} characters)</span>
        <input
          type="password"
          autoComplete="new-password"
          value={passphrase}
          data-testid="setup-passphrase"
          onChange={(e) => setPassphrase(e.target.value)}
        />
      </label>
      <div className="nc-pass" aria-hidden="true">
        <Strip n={12} solid={Math.min(passphraseLength(passphrase), 12)} size="sm" />
        <small>
          {Math.min(passphraseLength(passphrase), 12)} of {MIN_PASSPHRASE_LENGTH} characters
        </small>
      </div>
      <label className="nc-field">
        <span>Repeat the passphrase</span>
        <input
          type="password"
          autoComplete="new-password"
          value={confirm}
          data-testid="setup-confirm"
          onChange={(e) => setConfirm(e.target.value)}
        />
      </label>
      {confirm !== '' && confirm !== passphrase && (
        <p className="nc-msg" data-tone="err">
          The two passphrases differ.
        </p>
      )}
      <p className="nc-cap">
        {ARGON2_SENTENCE}. If you lose the passphrase and the backup, the note cannot be recovered.
      </p>
      {keepBox}
      <div className="nc-actions">
        <Button
          primary
          disabled={busy || !longEnough || passphrase !== confirm}
          testId="setup-submit"
          onClick={() => run(() => createPassphraseWrap(passphrase))}
        >
          {busy ? 'Deriving the key…' : 'Use this passphrase'}
        </Button>
        {prfPossible() && (
          <Button disabled={busy} testId="setup-prf" onClick={() => run(createPrfWrap)}>
            Use a passkey
          </Button>
        )}
      </div>
      {prfPossible() && (
        <p className="nc-cap">A passkey needs an authenticator that supports the PRF extension.</p>
      )}
    </>,
    'setup-wrap',
  );
}
