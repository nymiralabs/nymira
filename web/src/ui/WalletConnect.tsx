import { useEffect, useState } from 'react';
import { Button, NoteDialog, WalletMark } from '../marketing/kit';
import { PROFILE } from '../profile';
import { WALLET_NOTES } from '../wallet/notes';
import { useWallet } from './services';

/**
 * After a reload the wallet connection is gone: the wallet frame keeps nothing on this device
 * (host.ts), so the page says so and offers to connect the same wallet again.
 */
export function SessionLost() {
  const wallet = useWallet();
  const [error, setError] = useState('');
  const last = wallet.wallets.find((w) => w.id === wallet.lostWallet);
  if (wallet.connected || !wallet.lostWallet) return null;
  return (
    <div className="nc-notice" data-testid="session-lost">
      <p>
        Your wallet is not connected any more. The wallet runs in a sealed frame that keeps nothing
        on this device, so a reload or a new visit forgets the connection on purpose. Connect again
        to continue.
      </p>
      {last && (
        <div className="nc-actions">
          <Button
            testId="reconnect"
            onClick={async () => {
              setError('');
              try {
                await wallet.connect(last.id);
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              }
            }}
          >
            Reconnect with {last.name}
          </Button>
        </div>
      )}
      {error && (
        <p className="nc-msg" data-tone="err">
          {error}
        </p>
      )}
    </div>
  );
}

/** The wallet list and the connected account, as the first step of a flow. */
export function WalletConnect({ busy = false }: { busy?: boolean }) {
  const wallet = useWallet();
  const [error, setError] = useState('');
  const [asked, setAsked] = useState('');
  useEffect(() => {
    void wallet.start();
  }, [wallet]);

  const connect = async (id: string) => {
    setError('');
    try {
      await wallet.connect(id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  if (wallet.connected) {
    const name =
      wallet.wallets.find((w) => w.id === wallet.connected?.walletId)?.name ??
      wallet.connected.walletId;
    return (
      <div className="nc-row">
        <span data-testid="connected">
          Connected as <strong data-testid="account">{wallet.connected.accountId}</strong>
          {wallet.connected.signature === 'valid'
            ? ' (sign-in signature checked)'
            : ' (this browser cannot check the sign-in signature)'}
        </span>
        <span className="nc-wallet-chip">
          <WalletMark name={name} size={20} />
          {name}
        </span>
      </div>
    );
  }
  return (
    <>
      <SessionLost />
      <div className="nc-row">
        <span>No wallet connected.</span>
        <div className="nc-actions nc-actions-first">
          {wallet.wallets.map((w) => (
            <Button
              key={w.id}
              primary
              testId={`connect-${w.id}`}
              disabled={busy}
              onClick={() => (w.id in WALLET_NOTES ? setAsked(w.id) : void connect(w.id))}
            >
              <span className="nc-wallet-item">
                <WalletMark name={w.name} size={20} />
                {w.name}
                {w.injected ? ' (browser extension)' : ''}
              </span>
            </Button>
          ))}
        </div>
      </div>
      {asked && WALLET_NOTES[asked] && (
        <NoteDialog
          title={`Before you connect ${wallet.wallets.find((w) => w.id === asked)?.name ?? 'this wallet'}`}
          onCancel={() => setAsked('')}
          onContinue={() => {
            const id = asked;
            setAsked('');
            void connect(id);
          }}
        >
          <p>{WALLET_NOTES[asked].text}</p>
          <p>
            <a href={WALLET_NOTES[asked].issue} target="_blank" rel="noopener noreferrer">
              Read the report on Intear’s tracker
            </a>
          </p>
        </NoteDialog>
      )}
      <p className="nc-cap">
        On {PROFILE.network}, Meteor is the tested wallet. Intear signs in, but its approval window
        may not report back. Other wallets may work and are not promised.
      </p>
      {wallet.wallets.length === 0 && <p className="nc-cap">Starting the wallet connection…</p>}
      {wallet.skipped.map((s) => (
        <p key={s.id} className="nc-msg" data-tone="warn" data-testid="wallet-skipped">
          {s.id} is not offered: {s.reason}
        </p>
      ))}
      {error && (
        <p className="nc-msg" data-tone="err" data-testid="connect-error">
          {error}
        </p>
      )}
    </>
  );
}
