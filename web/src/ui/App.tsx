import { useEffect, useState, useSyncExternalStore } from 'react';
import { Icon, type IconName } from '../marketing/Icon';
import { Wordmark } from '../marketing/Wordmark';
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
type Theme = 'dark' | 'light';

const TITLES: Record<Exclude<Screen, 'landing'>, string> = {
  fund: 'Fund a note',
  chat: 'Chat',
  notes: 'Notes and backup',
  close: 'Close a note',
  agents: 'Agents',
};

const NAV: readonly [Exclude<Screen, 'landing'>, string, IconName][] = [
  ['fund', 'Fund', 'downRight'],
  ['chat', 'Chat', 'swap'],
  ['notes', 'Notes', 'lock'],
  ['close', 'Close', 'upRight'],
];

const THEME_KEY = 'nymira.theme';

function firstTheme(): Theme {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === 'dark' || saved === 'light') return saved;
  } catch {
    // Storage can be blocked: the theme then follows the system and is not remembered.
  }
  return matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

function ThemeToggle({ theme, onTheme }: { theme: Theme; onTheme: () => void }) {
  return (
    <button
      className="nm-icon-button nm-theme-toggle"
      type="button"
      aria-label={`Switch to ${theme === 'dark' ? 'light' : 'dark'} mode`}
      onClick={onTheme}
    >
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <circle cx="12" cy="12" r="4" stroke="currentColor" />
        {[0, 45, 90, 135, 180, 225, 270, 315].map((a) => (
          <path key={a} d="M12 1v3" stroke="currentColor" transform={`rotate(${a} 12 12)`} />
        ))}
      </svg>
    </button>
  );
}

function Foot() {
  return (
    <div className="nm-app-foot">
      <small data-testid="network-label">
        {PROFILE.network} · {PROFILE.id === 'mock' ? 'test build' : 'vault ' + PROFILE.vaultAccount}
      </small>
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
      <BundleHash />
    </div>
  );
}

export function App() {
  const [screen, setScreen] = useState<Screen>('landing');
  const [theme, setTheme] = useState<Theme>(firstTheme);
  const [drawer, setDrawer] = useState(false);
  // The panel opens over the chat or notes screen, which stay mounted so a conversation goes on.
  const [privacy, setPrivacy] = useState<string | null>(null);
  const go = (next: Screen) => {
    setPrivacy(null);
    setDrawer(false);
    setScreen(next);
    window.scrollTo({ top: 0, behavior: 'auto' });
  };
  const { check, recheck } = useVaultCheck();
  const wallet = useWallet();
  const showAgents = agentsAvailable(wallet.wallets, wallet.connected?.walletId ?? null);
  const owed = useSyncExternalStore(retireBackup.subscribe, () => retireBackup.state?.needed);

  const flip = () => {
    const next = theme === 'dark' ? 'light' : 'dark';
    setTheme(next);
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch {
      // Not remembered; the page still switches.
    }
  };
  // The page behind the app shows during overscroll; keep it the theme's own colour.
  useEffect(() => {
    document.documentElement.style.backgroundColor = theme === 'dark' ? '#000' : '#fff';
    document.documentElement.style.colorScheme = theme;
  }, [theme]);

  // The proving key loads into the worker when the page opens, so the first lease does not wait
  // for it. A copy served from another origin loads nothing.
  useEffect(() => {
    if (originOk) void warmProver();
  }, []);

  if (!originOk) {
    return (
      <div className="nm-site nm-app" data-theme={theme}>
        <main className="nm-app-plain">
          <div className="warning" role="alert" data-testid="origin-refused">
            <h1>Wrong address</h1>
            <p>
              This copy of the app is served from <code>{location.origin}</code>. It is built to run
              from <code>{PROFILE.appOrigin}</code> only, and the operator answers no other origin,
              so it will not connect or fund anything from here. Open the project's own address.
            </p>
          </div>
        </main>
      </div>
    );
  }

  if (screen === 'landing') {
    return (
      <Landing onOpen={() => go('fund')} theme={theme} onTheme={flip} footerStatus={<Foot />} />
    );
  }

  const brand = (
    <button
      type="button"
      className="brand nm-console-brand"
      aria-label="Nymira"
      onClick={() => go('landing')}
    >
      <Wordmark />
    </button>
  );

  return (
    <div className="nm-site nm-app nm-console" data-theme={theme} data-drawer={drawer}>
      <aside className="nm-console-side">
        {brand}
        <nav className="nm-console-nav" aria-label="Screens">
          <div>
            <span className="nm-console-group">Note</span>
            {NAV.map(([id, label, icon]) => (
              <button
                key={id}
                type="button"
                data-testid={`nav-${id}`}
                aria-current={screen === id ? 'page' : undefined}
                onClick={() => go(id)}
              >
                <Icon name={icon} size={16} />
                {label}
              </button>
            ))}
          </div>
          {showAgents && (
            <div>
              <span className="nm-console-group">Agents</span>
              <button
                type="button"
                data-testid="nav-agents"
                aria-current={screen === 'agents' ? 'page' : undefined}
                onClick={() => go('agents')}
              >
                <Icon name="plug" size={16} />
                Agents
              </button>
            </div>
          )}
        </nav>
        <div className="nm-console-foot">
          {wallet.connected && (
            <div className="nm-console-account">
              <span>
                <strong>{wallet.connected.accountId}</strong>
                <small>{wallet.connected.walletId}</small>
              </span>
            </div>
          )}
          <Foot />
        </div>
      </aside>
      <div className="nm-console-main">
        <header className="nm-console-top">
          <button
            type="button"
            className="nm-icon-button nm-console-menu"
            aria-label={drawer ? 'Close menu' : 'Open menu'}
            aria-expanded={drawer}
            onClick={() => setDrawer(!drawer)}
          >
            <svg width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
              <path
                d={drawer ? 'M4 4l12 12M16 4L4 16' : 'M2 5h16M2 10h16M2 15h16'}
                stroke="currentColor"
              />
            </svg>
          </button>
          <span className="nm-console-title">{TITLES[screen]}</span>
          <span className="nm-console-net">{PROFILE.network}</span>
          <ThemeToggle theme={theme} onTheme={flip} />
        </header>
        <p className="nm-console-ip" data-testid="ip-banner">
          This web app cannot hide your IP address from the operator. Use Tor Browser or{' '}
          <code>nymirad</code> for that.
        </p>
        <main id="main-content" className="nm-console-content">
          {owed && screen !== 'chat' && (
            <p className="warning" role="alert" data-testid="backup-owed">
              A lease ended and your note changed.{' '}
              <button type="button" onClick={() => go('chat')}>
                Back up now
              </button>
            </p>
          )}
          <Watch screen={screen} onClose={() => setScreen('close')} />
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
        </main>
      </div>
      {drawer && (
        <div className="nm-console-scrim" onClick={() => setDrawer(false)} aria-hidden="true" />
      )}
    </div>
  );
}
