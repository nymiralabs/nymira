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
import type { VaultCheck } from '../chain/vault';
import { originOk, useVaultCheck, useWallet } from './services';
import { Unlock } from './Unlock';
import { Docs } from './Docs';
import { Status } from './Status';
import { StatusChip } from './StatusChip';
import { Watch } from './Watch';

type Screen = 'landing' | 'docs' | 'status' | 'fund' | 'chat' | 'close' | 'notes' | 'agents';
type Theme = 'dark' | 'light';

const TITLES: Record<Exclude<Screen, 'landing'>, string> = {
  docs: 'Documentation',
  status: 'Status',
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

const SCREEN_IDS: readonly Screen[] = [
  'docs',
  'status',
  'chat',
  'fund',
  'notes',
  'close',
  'agents',
];
function hashScreen(): Screen | null {
  const name = location.hash.slice(1).split('/')[0];
  return SCREEN_IDS.find((id) => id === name) ?? null;
}

// A reload of this tab opens the screen it was on. A new tab starts at the landing page.
function firstScreen(): Screen {
  // A link to a screen (`#fund`) opens it; anything else in the hash is a landing page anchor.
  const linked = hashScreen();
  if (linked && linked !== 'agents') return linked;
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

function Foot({ check, onStatus }: { check: VaultCheck | null; onStatus: () => void }) {
  return (
    <div className="nm-app-foot">
      <StatusChip check={check} onStatus={onStatus} />
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
  const [brandMenu, setBrandMenu] = useState(false);
  const brandRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!brandMenu) return;
    const away = (e: MouseEvent) => {
      if (!brandRef.current?.contains(e.target as Node)) setBrandMenu(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setBrandMenu(false);
    };
    document.addEventListener('mousedown', away);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('mousedown', away);
      document.removeEventListener('keydown', esc);
    };
  }, [brandMenu]);
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
      if (screen === 'landing' || screen === 'agents' || screen === 'docs' || screen === 'status')
        sessionStorage.removeItem(SCREEN_KEY);
      else sessionStorage.setItem(SCREEN_KEY, screen);
    } catch {
      // Not remembered: a reload starts at the landing page.
    }
  }, [screen]);
  // The tab title and the address follow the screen, so a link and the back button work.
  useEffect(() => {
    document.title =
      screen === 'landing'
        ? 'Nymira: private, prepaid API credits on NEAR'
        : `${TITLES[screen]} · Nymira`;
    if (screen === 'landing' && hashScreen()) {
      history.pushState(null, '', location.pathname + location.search);
    }
    if (screen !== 'landing' && hashScreen() !== screen) {
      history.pushState(null, '', `#${screen}`);
    }
  }, [screen]);
  const showAgentsRef = useRef(false);
  useEffect(() => {
    const onPop = () => {
      const linked = hashScreen();
      if (linked === 'agents' && !showAgentsRef.current) return;
      setPrivacy(null);
      setDrawer(false);
      setScreen(linked ?? 'landing');
    };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);
  const go = (next: Screen) => {
    setPrivacy(null);
    setDrawer(false);
    setScreen(next);
    window.scrollTo({ top: 0, behavior: 'auto' });
  };
  // "Save" and "Back up now" lead to the backup prompt in the chat thread: open Chat if it is not
  // open, bring the prompt into view and put the cursor in it (the chat screen may still be mounting).
  const showBackup = () => {
    // On the Notes screen the backup form is on the page already.
    if (screen !== 'notes') go('chat');
    for (const ms of [0, 200]) {
      window.setTimeout(() => {
        const box = document.querySelector<HTMLElement>(
          screen === 'notes'
            ? '[data-testid="notes-backup-passphrase"]'
            : '[data-testid="backup-prompt"]',
        );
        if (!box) return;
        box.scrollIntoView({ block: 'center', behavior: 'smooth' });
        (box.matches('input, button')
          ? box
          : box.querySelector<HTMLElement>('input:not([disabled]), button:not([disabled])')
        )?.focus({
          preventScroll: true,
        });
      }, ms);
    }
  };
  const { check, recheck } = useVaultCheck();
  const wallet = useWallet();
  const showAgents = agentsAvailable(wallet.wallets, wallet.connected?.walletId ?? null);
  showAgentsRef.current = showAgents;
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

  if (screen === 'docs') {
    return (
      <Docs
        theme={theme}
        onTheme={flip}
        onHome={() => go('landing')}
        onOpenApp={() => go('fund')}
        onStatus={() => go('status')}
      />
    );
  }

  if (screen === 'status') {
    return (
      <Status
        theme={theme}
        onTheme={flip}
        onHome={() => go('landing')}
        onDocs={() => go('docs')}
        onOpenApp={() => go('fund')}
      />
    );
  }

  if (screen === 'landing') {
    return (
      <Landing
        onOpen={() => go('fund')}
        onDocs={() => go('docs')}
        onStatus={() => go('status')}
        theme={theme}
        onTheme={flip}
        footerStatus={<Foot check={check} onStatus={() => go('status')} />}
      />
    );
  }

  return (
    <div className="nm-site nm-app" data-theme={theme}>
      <div className="nc" data-drawer={drawer} data-screen={privacy !== null ? 'privacy' : screen}>
        <aside className="nc-side" aria-label="Console">
          <div className="nc-brand-row" ref={brandRef}>
            <button
              type="button"
              className="nc-brand"
              aria-label="Nymira"
              aria-haspopup="menu"
              aria-expanded={brandMenu}
              aria-controls="nc-brand-menu"
              data-testid="brand-menu"
              draggable={false}
              onClick={() => setBrandMenu(!brandMenu)}
            >
              <Wordmark animated />
            </button>
            {brandMenu && (
              <div className="nc-brand-menu" id="nc-brand-menu" role="menu" aria-label="Nymira">
                <button
                  type="button"
                  role="menuitem"
                  data-testid="brand-home"
                  onClick={() => {
                    setBrandMenu(false);
                    go('landing');
                  }}
                >
                  <Icon name="browser" size={14} />
                  <span>Home page</span>
                </button>
                <a
                  role="menuitem"
                  href="https://github.com/nymiralabs/nymira"
                  target="_blank"
                  rel="noopener noreferrer"
                  data-testid="brand-github"
                  onClick={() => setBrandMenu(false)}
                >
                  <svg
                    className="nm-icon"
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="currentColor"
                    aria-hidden="true"
                  >
                    <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
                  </svg>
                  <span>GitHub</span>
                  <Icon name="upRight" size={12} />
                </a>
                <button
                  type="button"
                  role="menuitem"
                  data-testid="brand-docs"
                  onClick={() => {
                    setBrandMenu(false);
                    go('docs');
                  }}
                >
                  <Icon name="braces" size={14} />
                  <span>Docs</span>
                </button>
              </div>
            )}
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
                        // A note on its way out takes no conversation: its row leads to the Close screen.
                        const leaving =
                          n.status === 'closing' || n.status === 'exiting' || n.status === 'closed';
                        const target = leaving ? 'close' : 'chat';
                        if (screen !== target) go(target);
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
                  {owed
                    ? 'Back up now'
                    : backupState?.saved
                      ? 'Latest state saved'
                      : backupState
                        ? 'No backup due yet'
                        : 'No backup owed'}
                </strong>
                <small>
                  {owed
                    ? 'A lease changed your note'
                    : backupState?.saved
                      ? 'Nothing changed since'
                      : backupState
                        ? 'A small balance: a backup every tenth lease'
                        : 'No lease ended in this page'}
                </small>
              </span>
              {owed && (
                <button type="button" data-testid="backup-save" onClick={showBackup}>
                  Save
                </button>
              )}
            </div>
            <div className="nc-legal">
              <div className="nc-legal-row">
                <StatusChip dot check={check} onStatus={() => go('status')} />
                <a
                  href="https://github.com/nymiralabs/nymira/blob/main/TERMS.md"
                  target="_blank"
                  rel="noopener noreferrer"
                  data-testid="terms-link"
                >
                  <span>Terms of use</span>
                  <svg viewBox="0 0 16 16" width="14" height="14" fill="none" aria-hidden="true">
                    <path d="M4 12 12 4M5 4h7v7" stroke="currentColor" strokeWidth="1.2" />
                  </svg>
                </a>
              </div>
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
          {owed && screen !== 'chat' && (
            <p className="nc-ip nc-owed" role="alert" data-testid="backup-owed">
              <Icon name="lock" size={14} />
              <span>A lease ended and your note changed. Save a backup before you go on.</span>
              <button type="button" className="nc-ip-ok" onClick={showBackup}>
                Back up now
              </button>
            </p>
          )}
          {!ipSeen && !locked && (
            <p className="nc-ip" role="status" data-testid="ip-banner">
              <Icon name="onion" size={14} />
              <span>
                This web app cannot hide your IP address from the operator, the RPC providers or
                1Click. Use Tor Browser or <code>nymirad</code> for that.
              </span>
              <button type="button" className="nc-ip-ok" onClick={dismissIp}>
                Got it
              </button>
            </p>
          )}
          <main id="main-content" className="nc-content">
            <Watch screen={screen} onClose={() => setScreen('close')} />
            {locked && <Unlock onReady={() => setLocked(false)} />}
            <div hidden={locked}>
              {screen === 'chat' && (
                <div hidden={privacy !== null}>
                  <Chat
                    vault={check}
                    onPrivacy={setPrivacy}
                    onFund={() => go('fund')}
                    onLeaving={(id) => {
                      if (id) setPreferred(id);
                      go('close');
                    }}
                    onActive={setActive}
                    preferred={preferred}
                  />
                </div>
              )}
              {screen === 'close' && <Close onFund={() => go('fund')} noteId={preferred} />}
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
                    onChat={(id) => {
                      setPreferred(id);
                      go('chat');
                    }}
                    onClose={(id) => {
                      setPreferred(id);
                      go('close');
                    }}
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
