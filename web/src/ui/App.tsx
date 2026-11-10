import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { Icon, type IconName } from '../marketing/Icon';
import { Wordmark } from '../marketing/Wordmark';
import { ThemeToggle } from '../marketing/ThemeToggle';
import { IpChip, NoteRow, Strip, WalletMenu, type RowNote } from '../marketing/kit';
import { agentsAvailable } from '../agents/available';
import { retireBackup } from '../chat/retire-backup';
import { warmProver } from '../chat/runtime';
import { isUnlocked, lock, requireKey, subscribeUnlocked } from '../crypto/wrap';
import { loadNotes } from '../funding/flow';
import { DAY, CLOSE_DAYS } from '../leave/guard';
import { PROFILE } from '../profile';
import { WALLET_NOTES } from '../wallet/notes';
import { noteAad } from '../store/types';
import { callWorker } from '../worker/client';
import { Agents } from './Agents';
import { BundleHash } from './BundleHash';
import { Chat } from './Chat';
import { Close } from './Close';
import { Fund } from './Fund';
import { Landing } from './Landing';
import { Notes } from './Notes';
import { Privacy } from './Privacy';
import { originOk, useVaultCheck, useWallet } from './services';
import { Unlock } from './Unlock';
import { Watch } from './Watch';

type Screen = 'landing' | 'fund' | 'chat' | 'close' | 'notes' | 'agents';
type Theme = 'dark' | 'light';

const TITLES: Record<Exclude<Screen, 'landing'>, string> = {
  chat: 'Chat',
  fund: 'Fund a note',
  notes: 'Notes and backup',
  close: 'Close or exit a note',
  agents: 'Agents',
};

const NAV: readonly [Exclude<Screen, 'landing'>, string, IconName][] = [
  ['chat', 'Chat', 'swap'],
  ['fund', 'Fund', 'downRight'],
  ['notes', 'Notes', 'lock'],
  ['close', 'Close', 'upRight'],
  ['agents', 'Agents', 'plug'],
];

const THEME_KEY = 'nymira.theme';
const SCREEN_KEY = 'nymira.screen';

// A reload of this tab opens the screen it was on. A new tab starts at the landing page.
function firstScreen(): Screen {
  try {
    const saved = sessionStorage.getItem(SCREEN_KEY);
    if (saved === 'fund' || saved === 'chat' || saved === 'close' || saved === 'notes')
      return saved;
  } catch {
    // No storage: the app starts at the landing page.
  }
  return 'landing';
}

function firstTheme(): Theme {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === 'dark' || saved === 'light') return saved;
  } catch {
    // Storage can be blocked: the theme then follows the system and is not remembered.
  }
  return matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

type ShellNote = RowNote & { key: string };

/** The accepted notes of this vault, read from the encrypted store while it is unlocked. */
function useShellNotes(unlocked: boolean, refreshOn: string): ShellNote[] {
  const [rows, setRows] = useState<ShellNote[]>([]);
  useEffect(() => {
    if (!unlocked) {
      setRows([]);
      return;
    }
    let live = true;
    void (async () => {
      try {
        const key = requireKey();
        const all = (await loadNotes()).filter((n) => n.kind === 'state' && n.phase === 'accepted');
        const now = Math.floor(Date.now() / 1000);
        const scale = 10 ** PROFILE.tokenDecimals;
        const next = await Promise.all(
          all.map(async (record): Promise<ShellNote> => {
            const view = await callWorker({
              op: 'readNote',
              key,
              aad: noteAad(record.network, record.vault, record.id),
              sealed: record.sealed,
            });
            const left = Math.max(0, Math.ceil((view.expiry - now) / DAY));
            const state = record.leave?.state;
            const gone = state === 'closed' || state === 'exited';
            const wait = record.leave?.deadline;
            const days = wait === undefined ? null : Math.max(0, Math.ceil((wait - now) / DAY));
            return {
              key: record.id,
              id: view.noteId,
              size: Number(BigInt(record.denomination)) / scale,
              balance: gone ? 0 : Number(BigInt(view.balance)) / scale,
              status: gone
                ? 'closed'
                : state === 'closing'
                  ? 'closing'
                  : state
                    ? 'exiting'
                    : left <= CLOSE_DAYS
                      ? 'expiring'
                      : view.pending
                        ? 'leased'
                        : 'open',
              day: 30 - left,
              ...(state === 'exited'
                ? { text: 'exited, paid out' }
                : state === 'exiting'
                  ? {
                      text:
                        days === null
                          ? 'exit requested'
                          : days === 0
                            ? 'exit can be finalized'
                            : `exit: ${days} ${days === 1 ? 'day' : 'days'} left`,
                    }
                  : {}),
            };
          }),
        );
        if (live) setRows(next);
      } catch {
        // Locked again, or the store is unavailable: the list stays empty.
        if (live) setRows([]);
      }
    })();
    return () => {
      live = false;
    };
  }, [unlocked, refreshOn]);
  return rows;
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
  const [screen, setScreen] = useState<Screen>(firstScreen);
  const [theme, setTheme] = useState<Theme>(firstTheme);
  const [drawer, setDrawer] = useState(false);
  const [funded, setFunded] = useState(0);
  const [notesOpen, setNotesOpen] = useState(false);
  const [locked, setLocked] = useState(false);
  const [active, setActive] = useState<string | null>(null);
  const [preferred, setPreferred] = useState<string | null>(null);
  const [ipSeen, setIpSeen] = useState(() => {
    try {
      return sessionStorage.getItem('nymira.ip-seen') === '1';
    } catch {
      return false;
    }
  });
  const dismissIp = () => {
    setIpSeen(true);
    try {
      sessionStorage.setItem('nymira.ip-seen', '1');
    } catch {
      // Not remembered: the notice shows again on the next visit.
    }
  };
  // The panel opens over the chat or notes screen, which stay mounted so a conversation goes on.
  const [privacy, setPrivacy] = useState<string | null>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const [thumb, setThumb] = useState({ top: 0, h: 0, show: false, ft: 0, fb: 0 });
  useEffect(() => {
    try {
      if (screen === 'landing' || screen === 'agents') sessionStorage.removeItem(SCREEN_KEY);
      else sessionStorage.setItem(SCREEN_KEY, screen);
    } catch {
      // Not remembered: a reload starts at the landing page.
    }
  }, [screen]);
  const go = (next: Screen) => {
    setPrivacy(null);
    setDrawer(false);
    setScreen(next);
    window.scrollTo({ top: 0, behavior: 'auto' });
  };
  const { check, recheck } = useVaultCheck();
  const wallet = useWallet();
  const showAgents = agentsAvailable(wallet.wallets, wallet.connected?.walletId ?? null);
  const backupState = useSyncExternalStore(retireBackup.subscribe, () => retireBackup.state);
  const owed = backupState?.needed === true;
  const unlocked = useSyncExternalStore(subscribeUnlocked, isUnlocked);
  const notes = useShellNotes(unlocked, `${screen}|${owed}|${backupState?.leases ?? ''}|${funded}`);
  // The note the sidebar marks: the conversation's, else the one picked, else the first.
  // The note list scrolls on its own and draws its own thin thumb: the native bar would take width
  // from the cards the moment the list overflows.
  useEffect(() => {
    const ul = listRef.current;
    if (!ul) return;
    const update = () => {
      const over = ul.scrollHeight - ul.clientHeight;
      if (over <= 1)
        return setThumb((t) => (t.show ? { top: 0, h: 0, show: false, ft: 0, fb: 0 } : t));
      const h = Math.max(24, (ul.clientHeight / ul.scrollHeight) * ul.clientHeight);
      const top = (ul.scrollTop / over) * (ul.clientHeight - h);
      setThumb({
        top: Math.round(top),
        h: Math.round(h),
        show: true,
        ft: ul.scrollTop > 2 ? 14 : 0,
        fb: over - ul.scrollTop > 2 ? 14 : 0,
      });
    };
    update();
    ul.addEventListener('scroll', update, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(ul);
    return () => {
      ul.removeEventListener('scroll', update);
      ro.disconnect();
    };
  }, [notes.length, notesOpen]);
  const current = notes.some((n) => n.key === active)
    ? active
    : notes.some((n) => n.key === preferred)
      ? preferred
      : (notes[0]?.key ?? null);

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

  // A tab that had a wallet before a reload starts the wallet frame at once, so one click on
  // "Reconnect" reaches the wallet while the click still counts as the user's.
  const lostWallet = wallet.lostWallet;
  useEffect(() => {
    if (originOk && lostWallet) void wallet.start().catch(() => undefined);
  }, [lostWallet, wallet]);

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

  return (
    <div className="nm-site nm-app" data-theme={theme}>
      <div className="nc" data-drawer={drawer}>
        <aside className="nc-side" aria-label="Console">
          <div className="nc-brand-row">
            <a
              className="nc-brand"
              href="#home"
              role="button"
              aria-label="Nymira"
              draggable={false}
              onClick={(e) => {
                e.preventDefault();
                go('landing');
              }}
            >
              <Wordmark animated />
            </a>
            <span className="nc-network" title={PROFILE.vaultAccount} data-testid="network-label">
              <i aria-hidden="true" />
              {PROFILE.network}
            </span>
          </div>
          <div className="nc-notes" data-open={notesOpen}>
            <button
              type="button"
              className="nc-group nc-notes-toggle"
              aria-expanded={notesOpen}
              aria-controls="nc-note-list"
              onClick={() => setNotesOpen(!notesOpen)}
            >
              <span>Your notes</span>
              <span className="nc-notes-count">{notes.length}</span>
              <Icon name="chevron" size={12} />
            </button>
            <div
              className="nc-notes-scroll"
              data-scroll={thumb.show}
              style={{ ['--ft' as string]: `${thumb.ft}px`, ['--fb' as string]: `${thumb.fb}px` }}
            >
              <ul id="nc-note-list" role="listbox" aria-label="Notes" ref={listRef}>
                {notes.map((n, i) => (
                  <li key={n.key} data-show={notesOpen || n.key === current}>
                    <NoteRow
                      n={n}
                      selected={n.key === current}
                      onPick={() => {
                        setPreferred(n.key);
                        if (screen !== 'chat') go('chat');
                      }}
                    />
                  </li>
                ))}
                {notes.length === 0 && (
                  <li data-show="true">
                    <button
                      type="button"
                      className="nc-note-empty"
                      onClick={() => go(unlocked ? 'fund' : 'chat')}
                    >
                      <span className="nc-note-line">
                        <strong>No note yet</strong>
                        <span>0.00 {PROFILE.tokenSymbol}</span>
                      </span>
                      <Strip n={10} solid={0} size="sm" />
                      <small>{unlocked ? 'Fund a note' : 'Unlock to see your notes'}</small>
                    </button>
                  </li>
                )}
              </ul>
              <i
                className="nc-thumb"
                aria-hidden="true"
                style={{ height: thumb.h, transform: `translateY(${thumb.top}px)` }}
              />
            </div>
          </div>
          <nav className="nc-nav" aria-label="Screens">
            <span className="nc-group">Screens</span>
            {NAV.filter(([id]) => id !== 'agents' || showAgents).map(([id, label, icon]) => (
              <a
                key={id}
                href={`#${id}`}
                data-testid={`nav-${id}`}
                aria-current={screen === id ? 'page' : undefined}
                onClick={(e) => {
                  e.preventDefault();
                  go(id);
                }}
              >
                <Icon name={icon} size={16} />
                {label}
              </a>
            ))}
          </nav>
          <div className="nc-foot">
            {unlocked && (
              <button
                type="button"
                className="nc-lock"
                data-testid="lock-notes"
                onClick={() => {
                  lock();
                  setLocked(true);
                }}
              >
                <Icon name="lock" size={14} />
                Lock notes
              </button>
            )}
            <div className="nc-backup" data-saved={!owed}>
              <Strip n={2} solid={owed ? 1 : 2} size="sm" />
              <span>
                <strong>
                  {owed ? 'Back up now' : backupState ? 'Latest state saved' : 'No backup owed'}
                </strong>
                <small>
                  {owed
                    ? 'A lease changed your note'
                    : backupState
                      ? 'Nothing changed since'
                      : 'No lease ended in this page'}
                </small>
              </span>
              {owed && (
                <button type="button" onClick={() => go('chat')}>
                  Save
                </button>
              )}
            </div>
            <div className="nc-legal">
              <a
                href="https://github.com/nymiralabs/nymira/blob/main/TERMS.md"
                target="_blank"
                rel="noopener noreferrer"
                data-testid="terms-link"
              >
                Terms of use
              </a>
              <BundleHash />
            </div>
          </div>
        </aside>
        <div className="nc-main">
          <header className="nc-top">
            <button
              type="button"
              className="nm-icon-button nc-menu"
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
            <span className="nc-title">{TITLES[screen]}</span>
            <span className="nc-net-spacer" />
            <IpChip />
            <ThemeToggle theme={theme} onTheme={flip} />
            <WalletMenu
              wallet={
                wallet.connected
                  ? {
                      account: wallet.connected.accountId,
                      name:
                        wallet.wallets.find((w) => w.id === wallet.connected?.walletId)?.name ??
                        wallet.connected.walletId,
                    }
                  : null
              }
              wallets={wallet.wallets}
              again={
                wallet.lostWallet
                  ? (() => {
                      const last = wallet.wallets.find((w) => w.id === wallet.lostWallet);
                      return last ? { id: last.id, name: last.name } : null;
                    })()
                  : null
              }
              onOpen={() => void wallet.start().catch(() => undefined)}
              notes={Object.fromEntries(
                Object.entries(WALLET_NOTES).map(([id, n]) => [
                  id,
                  <>
                    <p>{n.text}</p>
                    <p>
                      <a href={n.issue} target="_blank" rel="noopener noreferrer">
                        Read the report on Intear’s tracker
                      </a>
                    </p>
                  </>,
                ]),
              )}
              onConnect={async (id) => {
                await wallet.connect(id);
              }}
              onDisconnect={() => void wallet.disconnect().catch(() => undefined)}
            />
          </header>
          {!ipSeen && !locked && (
            <p className="nc-ip" role="status" data-testid="ip-banner">
              <Icon name="onion" size={14} />
              <span>
                This web app cannot hide your IP address from the operator. Use Tor Browser or{' '}
                <code>nymirad</code> for that.
              </span>
              <button type="button" className="nc-ip-ok" onClick={dismissIp}>
                Got it
              </button>
            </p>
          )}
          <main id="main-content" className="nc-content">
            <div className="nm-legacy">
              {owed && screen !== 'chat' && (
                <p className="warning" role="alert" data-testid="backup-owed">
                  A lease ended and your note changed.{' '}
                  <button type="button" onClick={() => go('chat')}>
                    Back up now
                  </button>
                </p>
              )}
            </div>
            <Watch screen={screen} onClose={() => setScreen('close')} />
            {locked && <Unlock onReady={() => setLocked(false)} />}
            <div hidden={locked}>
              {screen === 'chat' && (
                <div hidden={privacy !== null}>
                  <Chat
                    vault={check}
                    onPrivacy={setPrivacy}
                    onFund={() => go('fund')}
                    onActive={setActive}
                    preferred={preferred}
                  />
                </div>
              )}
              {screen === 'close' && <Close onFund={() => go('fund')} />}
              {screen === 'agents' && showAgents && <Agents />}
              {screen === 'fund' && (
                <Fund
                  vault={check}
                  recheck={recheck}
                  onChat={() => go('chat')}
                  onNotes={() => go('notes')}
                  onFunded={() => setFunded((n) => n + 1)}
                />
              )}
              {screen === 'notes' && (
                <div hidden={privacy !== null}>
                  <Notes
                    onPrivacy={setPrivacy}
                    onChat={() => go('chat')}
                    onClose={() => go('close')}
                    onFund={() => go('fund')}
                    shell={notes}
                    onChanged={() => setFunded((n) => n + 1)}
                  />
                </div>
              )}
              {privacy !== null && <Privacy noteId={privacy} onBack={() => setPrivacy(null)} />}
            </div>
          </main>
        </div>
        {drawer && <div className="nc-scrim" onClick={() => setDrawer(false)} aria-hidden="true" />}
      </div>
    </div>
  );
}
