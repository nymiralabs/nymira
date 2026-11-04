import { useEffect, useState, useSyncExternalStore } from 'react';
import { retireBackup } from '../chat/retire-backup';
import { warmProver } from '../chat/runtime';
import { PROFILE } from '../profile';
import { BundleHash } from './BundleHash';
import { Chat } from './Chat';
import { Close } from './Close';
import { Fund } from './Fund';
import { Landing } from './Landing';
import { Notes } from './Notes';
import { originOk, useVaultCheck } from './services';
import { Watch } from './Watch';

type Screen = 'landing' | 'fund' | 'chat' | 'close' | 'notes';

export function App() {
  const [screen, setScreen] = useState<Screen>('landing');
  const { check, recheck } = useVaultCheck();
  const owed = useSyncExternalStore(retireBackup.subscribe, () => retireBackup.state?.needed);

  // The proving key loads into the worker when the page opens, so the first lease does not wait
  // for it. A copy served from another origin loads nothing.
  useEffect(() => {
    if (originOk) void warmProver();
  }, []);

  if (!originOk) {
    return (
      <main>
        <div className="warning" role="alert" data-testid="origin-refused">
          <h1>Wrong address</h1>
          <p>
            This copy of the app is served from <code>{location.origin}</code>. It is built to run
            from <code>{PROFILE.appOrigin}</code> only, and the operator answers no other origin, so
            it will not connect or fund anything from here. Open the project's own address.
          </p>
        </div>
      </main>
    );
  }

  return (
    <main>
      <header>
        <button type="button" className="brand" onClick={() => setScreen('landing')}>
          Nymira
        </button>
        <nav>
          <button type="button" data-testid="nav-fund" onClick={() => setScreen('fund')}>
            Fund
          </button>
          <button type="button" data-testid="nav-chat" onClick={() => setScreen('chat')}>
            Chat
          </button>
          <button type="button" data-testid="nav-close" onClick={() => setScreen('close')}>
            Close
          </button>
          <button type="button" data-testid="nav-notes" onClick={() => setScreen('notes')}>
            Notes
          </button>
        </nav>
      </header>
      {screen !== 'landing' && (
        <p className="banner" data-testid="ip-banner">
          This web app cannot hide your IP address from the operator. Use Tor Browser or{' '}
          <code>nymirad</code> for that.
        </p>
      )}
      {owed && screen !== 'chat' && (
        <p className="warning" role="alert" data-testid="backup-owed">
          A lease ended and your note changed.{' '}
          <button type="button" onClick={() => setScreen('chat')}>
            Back up now
          </button>
        </p>
      )}
      {screen !== 'landing' && <Watch screen={screen} onClose={() => setScreen('close')} />}
      {screen === 'landing' && <Landing onOpen={() => setScreen('fund')} />}
      {screen === 'fund' && <Fund vault={check} recheck={recheck} />}
      {screen === 'chat' && <Chat vault={check} />}
      {screen === 'close' && <Close />}
      {screen === 'notes' && <Notes />}
      <footer>
        <small data-testid="network-label">
          {PROFILE.network} ·{' '}
          {PROFILE.id === 'mock' ? 'test build' : 'vault ' + PROFILE.vaultAccount}
        </small>
        <br />
        <small>
          <a
            href="https://github.com/nymiralabs/nymira/blob/main/TERMS.md"
            target="_blank"
            rel="noopener noreferrer"
            data-testid="terms-link"
          >
            Terms of use
          </a>
        </small>
        <br />
        <BundleHash />
      </footer>
    </main>
  );
}
