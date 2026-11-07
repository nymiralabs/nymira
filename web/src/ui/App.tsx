import { useEffect, useState, useSyncExternalStore } from 'react';
import { agentsAvailable } from '../agents/available';
import { retireBackup } from '../chat/retire-backup';
import { warmProver } from '../chat/runtime';
import { PROFILE } from '../profile';
import { Agents } from './Agents';
import { BundleHash } from './BundleHash';
import { Chat } from './Chat';
import { Close } from './Close';
import { Fund } from './Fund';
import { Landing } from './Landing';
import { Notes } from './Notes';
import { Privacy } from './Privacy';
import { originOk, useVaultCheck, useWallet } from './services';
import { Watch } from './Watch';

type Screen = 'landing' | 'fund' | 'chat' | 'close' | 'notes' | 'agents';

export function App() {
  const [screen, setScreen] = useState<Screen>('landing');
  // The panel opens over the chat or notes screen, which stay mounted so a conversation goes on.
  const [privacy, setPrivacy] = useState<string | null>(null);
  const go = (next: Screen) => {
    setPrivacy(null);
    setScreen(next);
  };
  const { check, recheck } = useVaultCheck();
  const wallet = useWallet();
  const showAgents = agentsAvailable(wallet.wallets, wallet.connected?.walletId ?? null);
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
        <button type="button" className="brand" onClick={() => go('landing')}>
          Nymira
        </button>
        <nav>
          <button type="button" data-testid="nav-fund" onClick={() => go('fund')}>
            Fund
          </button>
          <button type="button" data-testid="nav-chat" onClick={() => go('chat')}>
            Chat
          </button>
          <button type="button" data-testid="nav-close" onClick={() => go('close')}>
            Close
          </button>
          <button type="button" data-testid="nav-notes" onClick={() => go('notes')}>
            Notes
          </button>
          {showAgents && (
            <button type="button" data-testid="nav-agents" onClick={() => go('agents')}>
              Agents
            </button>
          )}
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
          <button type="button" onClick={() => go('chat')}>
            Back up now
          </button>
        </p>
      )}
      {screen !== 'landing' && <Watch screen={screen} onClose={() => setScreen('close')} />}
      {screen === 'landing' && <Landing onOpen={() => go('fund')} />}
      {screen === 'fund' && <Fund vault={check} recheck={recheck} />}
      {screen === 'chat' && (
        <div hidden={privacy !== null}>
          <Chat vault={check} onPrivacy={setPrivacy} />
        </div>
      )}
      {screen === 'close' && <Close />}
      {screen === 'notes' && (
        <div hidden={privacy !== null}>
          <Notes onPrivacy={setPrivacy} />
        </div>
      )}
      {screen === 'agents' && showAgents && <Agents />}
      {privacy !== null && <Privacy noteId={privacy} onBack={() => setPrivacy(null)} />}
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
