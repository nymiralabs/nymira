import { useEffect, useState } from 'react';
import { PROFILE } from '../profile';
import { useWallet } from './services';

/** The wallet list and the connected account. The same buttons the Fund screen has. */
export function WalletConnect({ busy = false }: { busy?: boolean }) {
  const wallet = useWallet();
  const [error, setError] = useState('');
  useEffect(() => {
    void wallet.start();
  }, [wallet]);

  if (wallet.connected) {
    return (
      <p data-testid="connected">
        Connected as <strong data-testid="account">{wallet.connected.accountId}</strong>
      </p>
    );
  }
  return (
    <div>
      <p>
        On {PROFILE.network}, Meteor and Intear are the supported wallets. Other wallets may work
        and are not promised.
      </p>
      <ul className="wallets">
        {wallet.wallets.map((w) => (
          <li key={w.id}>
            <button
              type="button"
              data-testid={`connect-${w.id}`}
              disabled={busy}
              onClick={async () => {
                setError('');
                try {
                  await wallet.connect(w.id);
                } catch (e) {
                  setError(e instanceof Error ? e.message : String(e));
                }
              }}
            >
              {w.name}
              {w.injected ? ' (browser extension)' : ''}
            </button>
          </li>
        ))}
      </ul>
      {wallet.wallets.length === 0 && <p className="hint">Starting the wallet connection…</p>}
      {error && (
        <p className="error" data-testid="connect-error">
          {error}
        </p>
      )}
    </div>
  );
}
