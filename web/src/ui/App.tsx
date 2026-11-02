import { useState } from 'react';
import { PROFILE } from '../profile';
import { Chat } from './Chat';
import { Fund } from './Fund';
import { Landing } from './Landing';
import { Notes } from './Notes';
import { originOk, useVaultCheck } from './services';

type Screen = 'landing' | 'fund' | 'chat' | 'notes';

export function App() {
  const [screen, setScreen] = useState<Screen>('landing');
  const { check, recheck } = useVaultCheck();

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
      {screen === 'landing' && <Landing onOpen={() => setScreen('fund')} />}
      {screen === 'fund' && <Fund vault={check} recheck={recheck} />}
      {screen === 'chat' && <Chat vault={check} />}
      {screen === 'notes' && <Notes />}
      <footer>
        <small data-testid="network-label">
          {PROFILE.network} ·{' '}
          {PROFILE.id === 'mock' ? 'test build' : 'vault ' + PROFILE.vaultAccount}
        </small>
      </footer>
    </main>
  );
}
