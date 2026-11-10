import {
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import { Cipher, Count } from './Board';
import { Icon } from './Icon';
import qwenLogo from './assets/brands/model-qwen.png';
import zaiLogo from './assets/brands/model-zai.svg';
import intearLogo from './assets/brands/wallet-intear.svg';
import meteorLogo from './assets/brands/wallet-meteor.svg';

// Parts the console and the real app screens share.

export const clamp = (n: number, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, n));
export const reduced = () =>
  typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
export const vars = (v: Record<string, string | number>) => v as CSSProperties;

export const WALLET_LOGO: Record<string, string> = { Meteor: meteorLogo, Intear: intearLogo };

/** A wallet's own mark on a light tile, so it reads on both themes. */
export function WalletMark({ name, size = 24 }: { name: string; size?: number }) {
  const src =
    WALLET_LOGO[name] ?? WALLET_LOGO[Object.keys(WALLET_LOGO).find((k) => name.includes(k)) ?? ''];
  if (!src) return null;
  return (
    <span className="nc-wallet-mark" style={vars({ '--s': `${size}px` })}>
      <img src={src} alt="" width={size} height={size} draggable={false} />
    </span>
  );
}

/** A row of cells. `solid` and `reserved` count cells and may be fractions of one. */
export function Strip({
  n = 10,
  solid,
  reserved = 0,
  drain = 0,
  size = 'md',
  label,
  mode,
  ready = 0,
}: {
  n?: number;
  solid: number;
  reserved?: number;
  /** Share of the reserved cells already charged, 0 to 1. */
  drain?: number;
  size?: 'sm' | 'md' | 'lg';
  label?: string;
  /** `idle` and `busy` run a quiet scan over the empty cells. */
  mode?: 'idle' | 'busy';
  /** Cells to outline brighter: how far the setup has come. May be a fraction. */
  ready?: number;
}) {
  return (
    <div
      className={`nc-strip nc-strip-${size}`}
      data-mode={mode}
      style={vars({ '--n': n, '--drain': drain })}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      {Array.from({ length: n }, (_, i) => {
        const s = clamp(solid - i);
        const r = clamp(solid + reserved - i);
        const kind = s >= 0.999 ? 'f' : s > 0.001 ? 'p' : r > 0.001 ? 'r' : 'e';
        return (
          <span
            key={i}
            data-k={kind}
            data-ready={i < ready ? '' : undefined}
            style={vars({
              '--p': s,
              '--i': Math.min(i, 24),
              transitionDelay: `${Math.min(i, 12) * 16}ms`,
            })}
          />
        );
      })}
    </div>
  );
}

export function Chip({
  children,
  tone = 'plain',
}: {
  children: ReactNode;
  tone?: 'plain' | 'good' | 'live' | 'warn';
}) {
  return <span className={`nc-chip nc-chip-${tone}`}>{children}</span>;
}

export function Facts({ rows }: { rows: readonly (readonly [string, ReactNode])[] }) {
  return (
    <dl className="nc-facts">
      {rows.map(([k, v]) => (
        <div key={k}>
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function Card({
  title,
  aside,
  children,
  className = '',
  testId,
}: {
  title?: string;
  aside?: ReactNode;
  children: ReactNode;
  className?: string;
  testId?: string;
}) {
  return (
    <section className={`nc-card ${className}`} data-testid={testId}>
      {title && (
        <header>
          <h2>{title}</h2>
          {aside && <span>{aside}</span>}
        </header>
      )}
      <div className="nc-card-body">{children}</div>
    </section>
  );
}

export function Button({
  children,
  onClick,
  primary = false,
  disabled = false,
  type = 'button',
  testId,
}: {
  testId?: string;
  children: ReactNode;
  onClick?: () => void;
  primary?: boolean;
  disabled?: boolean;
  type?: 'button' | 'submit';
}) {
  return (
    <button
      type={type}
      className={`nc-button ${primary ? 'nc-primary' : ''}`}
      disabled={disabled}
      data-testid={testId}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

/**
 * The panel beside a screen. Wide windows keep it as a column; narrow ones fold it into a sheet
 * that slides in from the right, opened by a tab on the right edge, closed by its own button, the
 * scrim or Escape.
 */
const NARROW = '(max-width: 1100px)';
const narrow = {
  subscribe: (cb: () => void) => {
    const q = window.matchMedia(NARROW);
    q.addEventListener('change', cb);
    return () => q.removeEventListener('change', cb);
  },
  get: () => window.matchMedia(NARROW).matches,
};

export function InspectorShell({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const sheet = useRef<HTMLElement>(null);
  const tab = useRef<HTMLButtonElement>(null);
  const id = useId();
  const small = useSyncExternalStore(narrow.subscribe, narrow.get);
  useEffect(() => {
    if (!open) return;
    sheet.current?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      setOpen(false);
      tab.current?.focus();
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [open]);
  const parts = (
    <>
      <button
        ref={tab}
        type="button"
        className="nc-insp-tab"
        aria-expanded={open}
        aria-controls={id}
        title={label}
        onClick={() => setOpen(true)}
      >
        <Icon name="chevron" size={12} />
        <span className="nc-sr">{label}</span>
      </button>
      <aside
        id={id}
        ref={sheet}
        tabIndex={-1}
        className="nc-inspector"
        data-open={open}
        aria-label={label}
      >
        <div className="nc-insp-head">
          <strong>{label}</strong>
          <button
            type="button"
            className="nc-insp-close"
            onClick={() => {
              setOpen(false);
              tab.current?.focus();
            }}
          >
            Close
          </button>
        </div>
        {children}
      </aside>
      {open && <div className="nc-insp-scrim" onClick={() => setOpen(false)} aria-hidden="true" />}
    </>
  );
  // Some ancestor of a screen makes its own frame for fixed boxes, so on a narrow window the sheet
  // goes to the end of <body>, inside a box that only carries the theme and the `nc` styles.
  if (!small) return parts;
  const theme = document.querySelector('.nm-site')?.getAttribute('data-theme') ?? 'dark';
  return createPortal(
    <div className="nm-site nc" data-theme={theme} style={{ display: 'contents' }}>
      {parts}
    </div>,
    document.body,
  );
}

export function Screen({
  title,
  lead,
  children,
  side,
  illustration,
  testId,
}: {
  title: string;
  lead: ReactNode;
  children: ReactNode;
  side?: ReactNode;
  illustration?: string;
  testId?: string;
}) {
  return (
    <div className={`nc-screen ${side ? 'nc-screen-side' : ''}`} data-testid={testId}>
      <div className="nc-screen-main">
        <div className={illustration ? 'nc-screen-intro nc-screen-intro-art' : 'nc-screen-intro'}>
          <div>
            <h1>{title}</h1>
            <p className="nc-lead">{lead}</p>
          </div>
          {illustration && (
            <img src={illustration} alt="" draggable={false} width="1000" height="500" />
          )}
        </div>
        {children}
      </div>
      {side && <InspectorShell label="Details">{side}</InspectorShell>}
    </div>
  );
}

// ---- the console shell and the chat parts, shared with the real app ----

export type NoteStatus = 'open' | 'leased' | 'expiring' | 'closing' | 'exiting' | 'closed';
export interface RowNote {
  id: number;
  size: number;
  balance: number;
  status: NoteStatus;
  /** Days since the deposit; a note expires after 30. */
  day: number;
  /** Replaces the default line under the cells. */
  text?: string;
}

const MODEL_LOGO = { GLM: zaiLogo, Qwen: qwenLogo } as const;
/** A pinned model with its family, logo and price (nano-USD per token) for the picker. */
export const modelInfo = (id: string, input: number, output: number): ModelInfo => ({
  id,
  family: id.startsWith('z-ai/') ? 'GLM' : 'Qwen',
  logo: id.startsWith('z-ai/') ? MODEL_LOGO.GLM : MODEL_LOGO.Qwen,
  input,
  output,
  ...(id.includes('-VL-') ? { note: 'text only here' } : {}),
});

/** The IP notice, kept one click away instead of on every screen. */
export function IpChip() {
  const [open, setOpen] = useState(false);
  const holder = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (
        e instanceof KeyboardEvent
          ? e.key === 'Escape'
          : !holder.current?.contains(e.target as Node)
      )
        setOpen(false);
    };
    window.addEventListener('mousedown', close);
    window.addEventListener('keydown', close);
    return () => {
      window.removeEventListener('mousedown', close);
      window.removeEventListener('keydown', close);
    };
  }, [open]);
  return (
    <div className="nc-ipchip" ref={holder}>
      <button
        type="button"
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => setOpen(!open)}
      >
        <Icon name="onion" size={14} />
        <span>IP visible</span>
      </button>
      {open && (
        <div className="nc-ipchip-pop" role="dialog" aria-label="About your IP address">
          <strong>The operator sees your IP address</strong>
          <p>
            This web app cannot hide your IP address from the operator, NEAR AI’s gateway or an RPC
            provider, and the browser cannot use Tor. Use Tor Browser or <code>nymirad</code> for
            that.
          </p>
        </div>
      )}
    </div>
  );
}

/** A short stop before something that needs a word first: a native modal, with a way on and out. */
export function NoteDialog({
  title,
  children,
  onContinue,
  onCancel,
  continueLabel = 'Continue',
}: {
  title: string;
  children: ReactNode;
  onContinue: () => void;
  onCancel: () => void;
  continueLabel?: string;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);
  return (
    <dialog
      ref={ref}
      className="nc-dialog"
      data-testid="wallet-note"
      aria-labelledby="nc-dialog-title"
      onCancel={(e) => {
        e.preventDefault();
        onCancel();
      }}
    >
      <h2 id="nc-dialog-title">{title}</h2>
      <div className="nc-dialog-body">{children}</div>
      <div className="nc-actions">
        <Button onClick={onCancel} testId="wallet-note-cancel">
          Cancel
        </Button>
        <Button primary onClick={onContinue} testId="wallet-note-continue">
          {continueLabel}
        </Button>
      </div>
    </dialog>
  );
}

/** The wallet control in the top bar: pick a wallet to connect, or see which one is. */
export function WalletMenu({
  wallet,
  wallets,
  onConnect,
  onDisconnect,
  onOpen,
  delay = 0,
  again = null,
  notes,
}: {
  wallet: { account: string; name: string } | null;
  /** A note to show when a wallet is picked, keyed by wallet id (a known problem, say). */
  notes?: Record<string, ReactNode>;
  /** The wallet this tab used before a reload: one click connects it again. */
  again?: { id: string; name: string } | null;
  wallets: readonly { id: string; name: string }[];
  /** Resolves when the wallet has answered; a rejection is shown in the menu. */
  onConnect: (id: string) => void | Promise<void>;
  onDisconnect: () => void;
  /** The menu was opened: the app can start what it needs to list wallets. */
  onOpen?: () => void;
  /** A pause before `onConnect`, so the sample app can show the wallet being asked. */
  delay?: number;
}) {
  const [open, setOpen] = useState(false);
  const [asking, setAsking] = useState('');
  const [error, setError] = useState('');
  const [asked, setAsked] = useState('');
  const timer = useRef(0);
  const holder = useRef<HTMLDivElement>(null);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (
        e instanceof KeyboardEvent
          ? e.key === 'Escape'
          : !holder.current?.contains(e.target as Node)
      )
        setOpen(false);
    };
    window.addEventListener('mousedown', close);
    window.addEventListener('keydown', close);
    return () => {
      window.removeEventListener('mousedown', close);
      window.removeEventListener('keydown', close);
    };
  }, [open]);
  const pick = (id: string) => {
    setAsking(id);
    setError('');
    timer.current = window.setTimeout(
      () => {
        void (async () => {
          try {
            await onConnect(id);
            setOpen(false);
          } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
          } finally {
            setAsking('');
          }
        })();
      },
      reduced() ? 0 : delay,
    );
  };
  return (
    <div className="nc-wallet" ref={holder}>
      <button
        type="button"
        className={`nc-button nc-wallet-button ${wallet ? '' : 'nc-primary'}`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => {
          if (!wallet && again) {
            setError('');
            setAsking(again.id);
            void Promise.resolve(onConnect(again.id))
              .catch((e: unknown) => {
                setError(e instanceof Error ? e.message : String(e));
                setOpen(true);
              })
              .finally(() => setAsking(''));
            return;
          }
          if (!open) onOpen?.();
          setOpen(!open);
        }}
      >
        {wallet ? (
          <>
            <WalletMark name={wallet.name} size={20} />
            <span className="nc-wl-account">{wallet.account}</span>
            <i className="nc-dot" aria-hidden="true" />
          </>
        ) : again ? (
          asking === again.id ? (
            'Connecting…'
          ) : (
            <>
              <WalletMark name={again.name} size={20} />
              <span>
                Reconnect<span className="nc-wl-extra"> {again.name}</span>
              </span>
            </>
          )
        ) : (
          <>
            Connect<span className="nc-wl-extra"> wallet</span>
          </>
        )}
      </button>
      {open && (
        <div className="nc-wallet-menu" role="menu">
          {wallet ? (
            <>
              <p>
                <strong>{wallet.account}</strong>
                <small>Connected with {wallet.name}. The sign-in signature was checked.</small>
              </p>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setOpen(false);
                  onDisconnect();
                }}
              >
                Disconnect
              </button>
            </>
          ) : (
            <>
              <p>
                <strong>Connect a wallet</strong>
                <small>
                  It runs in a sealed frame that keeps nothing on this device, so a reload forgets
                  the connection.
                </small>
              </p>
              {wallets.map((w) => (
                <button
                  key={w.id}
                  type="button"
                  role="menuitem"
                  disabled={asking !== ''}
                  onClick={() => (notes && w.id in notes ? setAsked(w.id) : pick(w.id))}
                >
                  <span className="nc-wallet-item">
                    <WalletMark name={w.name} size={24} />
                    {w.name}
                  </span>
                  {asking === w.id && <em>asking the wallet…</em>}
                </button>
              ))}
              {error && (
                <p role="alert">
                  <small>{error}</small>
                </p>
              )}
            </>
          )}
        </div>
      )}
      {asked && notes?.[asked] && (
        <NoteDialog
          title={`Before you connect ${wallets.find((w) => w.id === asked)?.name ?? 'this wallet'}`}
          onCancel={() => setAsked('')}
          onContinue={() => {
            const id = asked;
            setAsked('');
            pick(id);
          }}
        >
          {notes[asked]}
        </NoteDialog>
      )}
    </div>
  );
}

/** True for a moment after `value` changes, so a row can show that it just moved. */
function useFlash(value: number) {
  const [on, setOn] = useState(false);
  const prev = useRef(value);
  useEffect(() => {
    if (prev.current === value) return;
    prev.current = value;
    if (reduced()) return;
    setOn(true);
    const t = window.setTimeout(() => setOn(false), 1300);
    return () => window.clearTimeout(t);
  }, [value]);
  return on;
}

export function NoteRow({
  n,
  selected,
  onPick,
}: {
  n: RowNote;
  selected: boolean;
  onPick: () => void;
}) {
  const flash = useFlash(n.balance);
  return (
    <button
      type="button"
      role="option"
      aria-selected={selected}
      data-status={n.status}
      data-flash={flash}
      onClick={onPick}
    >
      <span className="nc-note-line">
        <strong>#{n.id}</strong>
        <span>
          <Count value={n.balance} /> USDC
        </span>
      </span>
      <Strip n={10} solid={clamp(n.balance / n.size) * 10} size="sm" />
      <Status n={n} />
    </button>
  );
}

export const statusText = (n: Pick<RowNote, 'status' | 'day' | 'text'>) =>
  n.text ??
  (n.status === 'expiring'
    ? `expires in ${30 - n.day} days`
    : n.status === 'leased'
      ? 'lease open'
      : n.status === 'closing'
        ? 'closing'
        : n.status === 'exiting'
          ? 'exit requested'
          : n.status === 'closed'
            ? 'closed'
            : 'open');

/** Six cells that light one after the other: something is being read or loaded. */
export function WaitBar() {
  return (
    <i className="nc-wait" aria-hidden="true">
      <b />
      <b />
      <b />
      <b />
      <b />
      <b />
    </i>
  );
}

/**
 * A screen or a block that has nothing to show yet: it says what it waits for (`wait`), that
 * there is nothing to show (`none`) or that it failed (`err`).
 */
export function Waiting({
  title,
  text,
  tone = 'wait',
  flat = false,
  testId,
  children,
}: {
  title: string;
  /** No frame: it sits inside a card that already has one. */
  flat?: boolean;
  testId?: string;
  text?: ReactNode;
  tone?: 'wait' | 'none' | 'err';
  children?: ReactNode;
}) {
  return (
    <div
      className="nc-hold"
      data-tone={tone}
      data-flat={flat || undefined}
      data-testid={testId}
      role={tone === 'err' ? 'alert' : 'status'}
    >
      {tone === 'wait' && <WaitBar />}
      <strong>{title}</strong>
      {text && <p>{text}</p>}
      {children}
    </div>
  );
}

/** One line under a note: a small mark that tells the state at a glance, then the words. */
export function Status({ n }: { n: Pick<RowNote, 'status' | 'day' | 'text'> }) {
  return (
    <small className="nc-state" data-s={n.status}>
      <i aria-hidden="true" />
      {statusText(n)}
    </small>
  );
}

export interface ModelInfo {
  id: string;
  family: string;
  logo: string;
  input: number;
  output: number;
  note?: string;
}
/** Dollars per million tokens: a nano-USD price per token times 0.001. */
const perMillion = (nano: number) => (nano / 1000).toFixed(2);

/** A model chooser that shows what each model costs, instead of a bare list of names. */
export function ModelPicker({
  models: MODEL_LIST,
  value,
  onChange,
  testId,
}: {
  models: readonly ModelInfo[];
  value: string;
  onChange: (id: string) => void;
  testId?: string;
}) {
  const [open, setOpen] = useState(false);
  const [hot, setHot] = useState(0);
  const holder = useRef<HTMLDivElement>(null);
  const current = MODEL_LIST.find((m) => m.id === value) ?? MODEL_LIST[0]!;
  const top = Math.max(...MODEL_LIST.map((m) => m.output));
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!holder.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener('mousedown', away);
    return () => window.removeEventListener('mousedown', away);
  }, [open]);
  const choose = (id: string) => {
    onChange(id);
    setOpen(false);
  };
  const key = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') setOpen(false);
    else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!open) {
        setOpen(true);
        setHot(MODEL_LIST.findIndex((m) => m.id === value));
        return;
      }
      setHot((h) => (h + (e.key === 'ArrowDown' ? 1 : -1) + MODEL_LIST.length) % MODEL_LIST.length);
    } else if ((e.key === 'Enter' || e.key === ' ') && open) {
      e.preventDefault();
      choose(MODEL_LIST[hot]!.id);
    }
  };
  return (
    <div className="nc-picker" ref={holder} onKeyDown={key}>
      <button
        type="button"
        className="nc-picker-button"
        data-testid={testId}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => {
          setOpen(!open);
          setHot(
            Math.max(
              0,
              MODEL_LIST.findIndex((m) => m.id === value),
            ),
          );
        }}
      >
        <img
          className="nc-model-logo"
          src={current.logo}
          alt=""
          width={26}
          height={26}
          draggable={false}
        />
        <span className="nc-picker-name">
          <strong>{current.id}</strong>
          <small>
            in ${perMillion(current.input)} · out ${perMillion(current.output)} per 1M tokens
          </small>
        </span>
        <Icon name="chevron" size={12} />
      </button>
      {open && (
        <ul className="nc-picker-list" role="listbox" aria-label="Model">
          {MODEL_LIST.map((m, i) => (
            <li key={m.id}>
              <button
                type="button"
                role="option"
                aria-selected={m.id === value}
                data-hot={i === hot}
                onMouseEnter={() => setHot(i)}
                onClick={() => choose(m.id)}
              >
                <span className="nc-picker-logo">
                  <img
                    className="nc-model-logo"
                    src={m.logo}
                    alt=""
                    width={28}
                    height={28}
                    draggable={false}
                  />
                  <span className="nc-picker-tick" aria-hidden="true">
                    <Icon name="check" size={10} />
                  </span>
                </span>
                <span className="nc-picker-info">
                  <strong>{m.id}</strong>
                  <small>
                    in ${perMillion(m.input)} · out ${perMillion(m.output)} per 1M tokens
                  </small>
                </span>
                <span className="nc-picker-side">
                  <Chip>{m.family}</Chip>
                  <Strip
                    n={6}
                    solid={(m.output / top) * 6}
                    size="sm"
                    label={`Output price of ${m.id}`}
                  />
                  {m.note && <em>{m.note}</em>}
                </span>
              </button>
            </li>
          ))}
          <li className="nc-picker-foot">
            Attested models only. The operator refuses any other model id. Prices are the live list
            of 2 Nov 2026.
          </li>
        </ul>
      )}
    </div>
  );
}

/** The card shown while a request is checked and sealed, or a lease is ended. */
export function Seal({
  title,
  titleTestId,
  badge,
  tag,
  rows,
  done,
  auto = false,
}: {
  title: string;
  titleTestId?: string;
  badge: ReactNode;
  tag: string;
  rows: readonly (readonly [string, string])[];
  /** Steps already passed; the next one is running. */
  done: number;
  /** Steps pass on their own, one after the other (the app waits on the network). */
  auto?: boolean;
}) {
  return (
    <div className="nc-seal" data-auto={auto}>
      <header>
        <h3 data-testid={titleTestId}>{title}</h3>
        {badge}
      </header>
      <div className="nc-seal-body">
        <div className="nc-cipher">
          <span>{tag}</span>
          <Cipher run rows={4} cols={30} />
        </div>
        <ol className="nc-vlist">
          {rows.map(([name, detail], i) => (
            <li
              key={name}
              data-state={auto ? 'auto' : done > i ? 'pass' : done === i ? 'run' : 'idle'}
              style={vars({ '--i': i })}
            >
              <span className="nc-vmark" aria-hidden="true">
                <Icon name="check" size={12} />
              </span>
              <div>
                <strong>{name}</strong>
                <small>{detail}</small>
              </div>
            </li>
          ))}
        </ol>
      </div>
      <div className="nc-seal-bar">
        <Strip n={rows.length} solid={auto ? 0 : done} reserved={auto ? 0 : 1} size="sm" />
      </div>
    </div>
  );
}

/** A section the reader opens when they want it. */
export function Fold({
  title,
  badge,
  children,
  testId,
}: {
  title: string;
  badge?: ReactNode;
  children: ReactNode;
  testId?: string;
}) {
  return (
    <details className="nc-fold" data-testid={testId}>
      <summary>
        <span>{title}</span>
        {badge}
        <Icon name="chevron" size={12} />
      </summary>
      <div className="nc-fold-body">{children}</div>
    </details>
  );
}

export const CHECK_ROWS: readonly (readonly [string, string])[] = [
  ['Enclave quote', 'Intel TDX, TCB UpToDate'],
  ['Key binding', 'Signing key bound to this quote and request'],
  ['Signed policy', 'Deployment found in the policy'],
  ['Prompt sealed', 'Encrypted to the model’s key'],
];

export const END_ROWS: readonly (readonly [string, string])[] = [
  ['Lease retired', 'The operator stores the provider’s signature'],
  ['Signatures read', 'Checked against the exact bytes exchanged'],
  ['Charge recomputed', 'From the signed usage and the vault’s rules'],
];

/** The checks made before a prompt is sent; the last row is what a browser cannot check. */
export function VerifyList({ state }: { state: (i: number) => 'idle' | 'run' | 'pass' }) {
  return (
    <ol className="nc-vlist">
      {CHECK_ROWS.map(([title, detail], i) => {
        const s = state(i);
        return (
          <li key={title} data-state={s}>
            <span className="nc-vmark" aria-hidden="true">
              <Icon name="check" size={12} />
            </span>
            <div>
              <strong>{title}</strong>
              <small>{detail}</small>
            </div>
            <span className="nc-sr">
              {s === 'pass' ? 'passed' : s === 'run' ? 'running' : 'waiting'}
            </span>
          </li>
        );
      })}
      <li data-state="partial">
        <span className="nc-vmark" aria-hidden="true" />
        <div>
          <strong>GPU evidence</strong>
          <small>
            NVIDIA: only the nonce is checked in a browser. nymirad verifies the evidence itself.
          </small>
        </div>
        <span className="nc-sr">partly checked</span>
      </li>
    </ol>
  );
}

/** The three sub-steps of "Protect and back up": the secret, the encrypted backup, the restore test. */
export function BackupSteps({
  prefix = '',
  created,
  createLabel,
  createDisabled,
  onCreate,
  createNote,
  pass,
  onPass,
  passDisabled,
  passMin,
  params,
  downloaded,
  downloadLabel,
  downloadDisabled,
  onDownload,
  tested,
  testedNote,
  chooseLabel,
  chooseDisabled,
  onChoose,
  onFile,
  fileReady = false,
}: {
  /** Test id prefix, so two copies on one page stay apart. */
  prefix?: string;
  created: boolean;
  createLabel: string;
  createDisabled: boolean;
  onCreate: () => void;
  createNote?: ReactNode;
  pass: string;
  onPass: (value: string) => void;
  passDisabled: boolean;
  passMin: number;
  params: ReactNode;
  downloaded: boolean;
  downloadLabel: string;
  downloadDisabled: boolean;
  onDownload: () => void;
  tested: boolean;
  testedNote?: ReactNode;
  chooseLabel: string;
  chooseDisabled: boolean;
  /** Without `onFile` the button just calls this. */
  onChoose?: () => void;
  /** With `onFile` the button opens a file chooser and hands over the chosen file. */
  onFile?: (file: File | undefined) => void;
  /** The file chooser exists once there is a backup to test. */
  fileReady?: boolean;
}) {
  const picker = useRef<HTMLInputElement>(null);
  const downloadNow = created && !downloaded;
  const chooseNow = downloaded && !tested;
  return (
    <ol className="nc-sub" data-testid={created ? `${prefix}backup-panel` : undefined}>
      <li data-on={created}>
        <i className="nc-sub-dot" aria-hidden="true" />
        <div className="nc-sub-main">
          <strong>Create the secret</strong>
          <p className="nc-sub-why">Made in this browser and stored encrypted.</p>
          {createNote}
        </div>
        <Button
          primary={!created}
          disabled={createDisabled}
          testId={created ? undefined : `${prefix}prepare`}
          onClick={onCreate}
        >
          {createLabel}
        </Button>
      </li>
      <li data-on={downloaded} data-locked={!created}>
        <i className="nc-sub-dot" aria-hidden="true" />
        <div className="nc-sub-main">
          <strong>Download an encrypted backup</strong>
          <p className="nc-sub-why">Without this file, a lost browser means a lost balance.</p>
          <label className="nc-field">
            <span>Backup passphrase</span>
            <input
              type="password"
              autoComplete="new-password"
              value={pass}
              disabled={passDisabled}
              data-testid={`${prefix}backup-passphrase`}
              onChange={(e) => onPass(e.target.value)}
            />
          </label>
          <div className="nc-pass" aria-hidden="true">
            <Strip n={12} solid={Math.min(pass.length, 12)} size="sm" />
            <small>
              {Math.min(pass.length, 12)} of {passMin} characters
            </small>
          </div>
          {params}
        </div>
        <Button
          primary={downloadNow}
          disabled={downloadDisabled}
          testId={`${prefix}backup-download`}
          onClick={onDownload}
        >
          {downloadLabel}
        </Button>
      </li>
      <li data-on={tested} data-locked={!downloaded}>
        <i className="nc-sub-dot" aria-hidden="true" />
        <div className="nc-sub-main">
          <strong>Check the backup</strong>
          {tested ? (
            testedNote
          ) : (
            <p className="nc-sub-why">
              Open the file you saved. The transfer unlocks after it opens.
            </p>
          )}
        </div>
        <Button
          primary={chooseNow}
          disabled={chooseDisabled}
          onClick={onFile ? () => picker.current?.click() : onChoose}
        >
          {chooseLabel}
        </Button>
        {onFile && fileReady && (
          <input
            ref={picker}
            hidden
            type="file"
            data-testid={`${prefix}backup-file`}
            disabled={chooseDisabled}
            onChange={(e) => {
              const file = e.target.files?.[0];
              // Cleared so choosing the same file again (after a typo) fires again.
              e.target.value = '';
              onFile(file);
            }}
          />
        )}
      </li>
    </ol>
  );
}

/** The inspector of the fund screen: the note, what it costs, where it pays, what leaves the page. */
export function FundSide({
  amount,
  unit,
  strip,
  yours,
  facts,
  factsTestId,
  caption,
  closes,
  leaves,
  limits,
  testnetNote,
}: {
  amount: number;
  unit: string;
  strip: { solid: number; reserved: number; mode?: 'idle' | 'busy'; ready?: number };
  /** The line under the cells, left side. */
  yours: string;
  facts: readonly (readonly [string, ReactNode])[];
  factsTestId?: string;
  caption: string;
  closes: readonly (readonly [string, ReactNode])[];
  leaves: ReactNode;
  limits: readonly (readonly [string, ReactNode])[];
  /** Shown under the limits, for a build that is not the real thing. */
  testnetNote?: ReactNode;
}) {
  return (
    <>
      <section className="nc-sum">
        <h2>The note</h2>
        <p className="nc-big">
          <Count value={amount} /> <small>{unit}</small>
        </p>
        <Strip
          n={21}
          solid={strip.solid}
          reserved={strip.reserved}
          size="lg"
          label="Note and fee"
          mode={strip.mode}
          ready={strip.ready}
        />
        <p className="nc-sum-line">
          <span>{yours}</span>
          <span>+ fee</span>
        </p>
        <div data-testid={factsTestId}>
          <Facts rows={facts} />
        </div>
        <p className="nc-cap">{caption}</p>
      </section>
      <section className="nc-sum">
        <h2>When it closes</h2>
        <Facts rows={closes} />
      </section>
      <Fold title="What leaves this page">{leaves}</Fold>
      <Fold title="Limits">
        <Facts rows={limits} />
        {testnetNote}
      </Fold>
    </>
  );
}
