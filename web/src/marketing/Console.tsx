import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { Cipher, Count } from './Board';
import { Icon, type IconName } from './Icon';
import qwenLogo from './assets/brands/model-qwen.png';
import zaiLogo from './assets/brands/model-zai.svg';
import intearLogo from './assets/brands/wallet-intear.svg';
import meteorLogo from './assets/brands/wallet-meteor.svg';
import fundIllustration from './assets/app-fund.webp';
import notesIllustration from './assets/app-notes.webp';
import closeIllustration from './assets/app-close.webp';
import agentsIllustration from './assets/app-agents.webp';
import { ClientGrid, ClientMark, CLIENTS, type ClientId, type ClientInfo } from './kit-agents';
import { ThemeToggle } from './ThemeToggle';
import {
  BackupSteps,
  Button,
  Card,
  Chip,
  CHECK_ROWS,
  END_ROWS,
  Facts,
  Fold,
  FundSide,
  type ModelInfo,
  IpChip,
  ModelPicker,
  NoteRow,
  Screen,
  Seal,
  Strip,
  VerifyList,
  WalletMenu,
  WALLET_LOGO,
  WalletMark,
  clamp,
  statusText,
  reduced,
  vars,
} from './kit';
import { ExitDays, ExpiryBanner, FlowSteps, GuardTimeline } from './kit-close';
import { LookupRow, NoteLedger, NoteLedgerRow, PassField } from './kit-notes';
import { Wordmark } from './Wordmark';
import { FLAG_NAME, SetCells, SetLegend, type Flag } from './kit-privacy';

// The Nymira web app as a console, designed on the landing site's material: cells for money and
// time, mono for data, accent only where something is reserved, running or waiting for you.
// Everything is sample data and nothing is connected. The rules shown are the README's.
export type Tab = 'fund' | 'chat' | 'notes' | 'close' | 'agents';

type Status = 'open' | 'expiring' | 'exiting' | 'closed';
interface Note {
  id: number;
  size: number;
  balance: number;
  status: Status;
  /** Days since the deposit; a note expires after 30. */
  day: number;
}

interface Wallet {
  account: string;
  name: string;
}
const WALLET_ITEMS = (['Meteor', 'Intear'] as const).map((name) => ({ id: name, name }));

const NAV: readonly [Tab, string, IconName][] = [
  ['chat', 'Chat', 'swap'],
  ['fund', 'Fund', 'downRight'],
  ['notes', 'Notes', 'lock'],
  ['close', 'Close', 'upRight'],
  ['agents', 'Agents', 'plug'],
];

const TITLES: Record<Tab, string> = {
  chat: 'Chat',
  fund: 'Fund a note',
  notes: 'Notes and backup',
  close: 'Close or exit a note',
  agents: 'Agents',
};

const START_NOTES: readonly Note[] = [
  { id: 41, size: 1, balance: 0.98, status: 'open', day: 1 },
  { id: 27, size: 5, balance: 4.1, status: 'open', day: 6 },
  { id: 12, size: 1, balance: 0.33, status: 'expiring', day: 27 },
];

const usd = (n: number, d = 2) => n.toFixed(d);

// ---------------------------------------------------------------------------------------------
// Parts every screen shares

// ---------------------------------------------------------------------------------------------
// Chat

type Turn = { role: 'you' | 'model'; text: string; usage?: number };
type Phase = 'idle' | 'checking' | 'typing' | 'ending';

const CHECKS = [
  'Intel TDX quote verified, TCB UpToDate',
  'Signing key bound to this quote and request',
  'Deployment found in the signed policy',
  'Prompt sealed to the model’s key',
] as const;

const REPLIES = [
  'A prepaid note is credit you fund once with USDC. Your device proves it can pay for a lease without saying which deposit it came from, so the operator sees a proof and a size, not your account.',
  'Only the charge leaves the note. A lease reserves its bound while it is open, and when it closes the vault keeps what the signed usage says. The rest of the note stays yours.',
  'The model decrypts your prompt inside its enclave. The operator relays ciphertext and sees the model, the message sizes and the timing, not the text.',
] as const;

// The pinned attested models and their prices in nano-USD per token (web/src/chat/models.ts).
const MODEL_LIST: readonly ModelInfo[] = [
  { id: 'z-ai/glm-5.3-flash', family: 'GLM', logo: zaiLogo, input: 80, output: 500 },
  { id: 'Qwen/Qwen3.6-35B-A3B-FP8', family: 'Qwen', logo: qwenLogo, input: 170, output: 1100 },
  { id: 'Qwen/Qwen3.8-27B', family: 'Qwen', logo: qwenLogo, input: 40, output: 1350 },
  {
    id: 'Qwen/Qwen3-VL-30B-A3B-Instruct',
    family: 'Qwen',
    logo: qwenLogo,
    input: 150,
    output: 550,
    note: 'text only here',
  },
];
const MODELS = MODEL_LIST.map((m) => m.id);

interface ChatState {
  turns: Turn[];
  phase: Phase;
  check: number;
  lease: boolean;
  usage: number;
  settled: { charge: number; balance: number } | null;
  draft: string;
}
const FRESH_CHAT: ChatState = {
  turns: [],
  phase: 'idle',
  check: 0,
  lease: false,
  usage: 0,
  settled: null,
  draft: '',
};

function Chat({
  note,
  chat,
  setChat,
  onSettle,
  backupStale,
  onPrivacy,
  onFund,
  onNotes,
  wallet,
}: {
  onPrivacy: () => void;
  onFund: () => void;
  onNotes: () => void;
  wallet: Wallet | null;
  note: Note;
  chat: ChatState;
  setChat: (u: (c: ChatState) => ChatState) => void;
  onSettle: (charge: number) => void;
  backupStale: boolean;
}) {
  const timers = useRef<number[]>([]);
  const threadEnd = useRef<HTMLDivElement>(null);
  const [model, setModel] = useState<string>(MODELS[0]!);
  const [typed, setTyped] = useState(0);
  const pending = useRef('');
  useEffect(() => () => timers.current.forEach((t) => window.clearTimeout(t)), []);
  useEffect(() => {
    threadEnd.current?.scrollIntoView({
      block: 'nearest',
      behavior: reduced() ? 'auto' : 'smooth',
    });
  }, [chat.turns.length, chat.phase, typed]);

  const later = (ms: number, fn: () => void) => {
    timers.current.push(window.setTimeout(fn, reduced() ? 0 : ms));
  };
  const bound = 0.1;
  const worst = clamp(0.012 + chat.draft.length * 0.00045, 0.012, bound);
  const busy = chat.phase !== 'idle';
  const closed = note.status === 'closed' || note.status === 'exiting';
  const expiring = note.day >= 25;
  const short = note.balance < bound;
  const needsNote = closed || (!chat.lease && (expiring || short));
  const canSend = chat.draft.trim() !== '' && !busy && !chat.settled && !needsNote;

  const send = () => {
    if (!canSend) return;
    const prompt = chat.draft.trim();
    const reply = REPLIES[prompt.length % REPLIES.length]!;
    pending.current = reply;
    setTyped(0);
    setChat((c) => ({
      ...c,
      turns: [...c.turns, { role: 'you', text: prompt }],
      draft: '',
      phase: 'checking',
      check: 0,
      lease: true,
    }));
    [1, 2, 3, 4].forEach((step) =>
      later(step * 620, () => setChat((c) => ({ ...c, check: step }))),
    );
    later(4 * 620 + 300, () => {
      setChat((c) => ({ ...c, phase: 'typing' }));
      const speed = reduced() ? 0 : 14;
      let shown = 0;
      const tick = () => {
        shown = Math.min(reply.length, shown + (speed === 0 ? reply.length : 2));
        setTyped(shown);
        if (shown < reply.length) timers.current.push(window.setTimeout(tick, speed));
        else {
          const cost =
            Math.round((0.006 + reply.length * 0.00007 + prompt.length * 0.00003) * 1000) / 1000;
          setChat((c) => ({
            ...c,
            turns: [...c.turns, { role: 'model', text: reply, usage: cost }],
            phase: 'idle',
            usage: Math.round((c.usage + cost) * 1000) / 1000,
          }));
        }
      };
      tick();
    });
  };

  const end = () => {
    if (!chat.lease || busy) return;
    setChat((c) => ({ ...c, phase: 'ending' }));
    later(1500, () => {
      const charge = Math.round((chat.usage + 0.003) * 1000) / 1000;
      onSettle(charge);
      setChat((c) => ({
        ...c,
        phase: 'idle',
        lease: false,
        settled: { charge, balance: Math.round((note.balance - charge) * 1000) / 1000 },
      }));
    });
  };

  const checking = chat.phase === 'checking';
  const share = note.balance / note.size;
  const reservedCells = chat.lease ? 10 : 0;
  const chargedShare = chat.lease ? clamp(chat.usage / bound) : 0;
  const passed = chat.turns.length === 0 && !checking ? 0 : checking ? chat.check : CHECKS.length;
  const rows = Math.min(8, Math.max(1, chat.draft.split('\n').length));

  return (
    <div className="nc-screen nc-screen-side nc-chat-screen">
      <div className="nc-chat-main">
        <h1 className="nc-sr">Chat</h1>
        <div className="nc-chat-head">
          <ModelPicker models={MODEL_LIST} value={model} onChange={setModel} />
          <div className="nc-chat-head-end">
            {chat.lease ? (
              <>
                <Chip tone="live">lease open · {usd(bound)} reserved</Chip>
                <Button onClick={end} disabled={busy}>
                  End conversation
                </Button>
              </>
            ) : (
              <Chip>no lease yet</Chip>
            )}
          </div>
        </div>
        <div className="nc-thread" aria-live="polite">
          {chat.turns.length === 0 && !busy && !chat.settled && (
            <div className="nc-empty">
              <h2>Ask an attested model.</h2>
              <p>
                Your prompt is encrypted to its key before it leaves this page. The first message
                opens a lease that reserves {usd(bound)} USDC.
              </p>
              {!wallet && (
                <div className="nc-chat-fund">
                  <p>Need a new note? Connect a wallet and fund one first.</p>
                  <div>
                    <Button onClick={onFund}>Fund a note</Button>
                  </div>
                </div>
              )}
              <div className="nc-suggest">
                {[
                  'Explain how a prepaid note works.',
                  'What does the operator see?',
                  'How do I get my balance back?',
                ].map((q) => (
                  <button
                    key={q}
                    type="button"
                    onClick={() => setChat((c) => ({ ...c, draft: q }))}
                  >
                    {q}
                    <Icon name="right" size={14} />
                  </button>
                ))}
              </div>
            </div>
          )}
          {(chat.turns.length > 0 || busy) && (
            <p className="nc-divider">
              <span>Lease opened · reserves {usd(bound)} USDC</span>
            </p>
          )}
          {chat.turns.map((t, i) => (
            <div key={i} className="nc-turn" data-role={t.role}>
              <span className={t.role === 'you' ? 'nc-sr' : 'nc-who'}>
                {t.role === 'you' ? 'You' : model}
              </span>
              <p>{t.text}</p>
              {t.role === 'model' && (
                <div className="nc-meta">
                  <Chip tone="good">attested model</Chip>
                  <Chip>sealed to its key</Chip>
                  <Chip>usage {usd(t.usage ?? 0, 3)} USDC</Chip>
                </div>
              )}
            </div>
          ))}
          {checking && (
            <Seal
              title="Checking the model, then sealing your prompt"
              badge={
                <Chip tone="live">
                  {chat.check} of {CHECKS.length} passed
                </Chip>
              }
              tag="X25519 · XChaCha20-Poly1305"
              rows={CHECK_ROWS}
              done={chat.check}
            />
          )}
          {chat.phase === 'typing' && (
            <div className="nc-turn" data-role="model">
              <span className="nc-who">{model}</span>
              <p>
                {pending.current.slice(0, typed)}
                <i className="nc-caret" />
              </p>
            </div>
          )}
          {chat.phase === 'ending' && (
            <Seal
              title="Ending the lease"
              badge={<Chip tone="live">settling</Chip>}
              tag="signed usage"
              rows={END_ROWS}
              done={0}
              auto
            />
          )}
          {chat.settled && (
            <>
              <p className="nc-divider">
                <span>Lease closed · charged {usd(chat.settled.charge, 3)} USDC</span>
              </p>
              <div className="nc-settled">
                <h3>
                  <Chip tone="good">model-signed response</Chip>
                </h3>
                <p>
                  Lease cost {usd(chat.settled.charge, 3)} USDC, recomputed from the signed usage
                  and the vault’s rules. Balance now {usd(chat.settled.balance)} USDC.
                </p>
                {backupStale && (
                  <p className="nc-settled-note">
                    A lease ended and your note changed. Back up now: a lost latest state lets the
                    operator defeat your exit.
                  </p>
                )}
                <Button primary onClick={() => setChat(() => ({ ...FRESH_CHAT }))}>
                  New conversation
                </Button>
              </div>
            </>
          )}
          <div ref={threadEnd} />
        </div>
        {!chat.settled && (
          <div className="nc-composer">
            {needsNote && (
              <div className="nc-chat-fund" role="status">
                <p>
                  {closed
                    ? 'This note is closed or exiting.'
                    : expiring
                      ? 'This note is in its last 5 days and cannot open a new lease.'
                      : `This note needs at least ${usd(bound)} USDC to open a lease.`}{' '}
                  Choose another note or fund a new one to chat.
                </p>
                <div className="nc-actions">
                  <Button primary onClick={onFund}>
                    Fund a note
                  </Button>
                  <Button onClick={onNotes}>Choose a note</Button>
                </div>
              </div>
            )}
            <div className="nc-compose-box" data-busy={busy}>
              <textarea
                rows={rows}
                value={chat.draft}
                placeholder="Ask the model"
                aria-label="Message"
                onChange={(e) => setChat((c) => ({ ...c, draft: e.target.value }))}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    send();
                  }
                }}
              />
              <button
                type="button"
                className="nc-send"
                aria-label="Send"
                disabled={!canSend}
                onClick={send}
              >
                <Icon name="right" size={16} />
              </button>
            </div>
            <div className="nc-composer-foot">
              <div className="nc-worst" title="Example pricing">
                <Strip n={10} solid={0} reserved={(worst / bound) * 10} size="sm" />
                <small>
                  Worst case {usd(worst, 3)} of {usd(bound)} USDC
                </small>
              </div>
              <small className="nc-hint">Ctrl or ⌘ + Enter sends</small>
            </div>
          </div>
        )}
      </div>
      <aside className="nc-inspector" aria-label="Note and lease">
        <section className="nc-sum">
          <h2>Note #{note.id}</h2>
          <p className="nc-big">
            <Count value={note.balance} /> <small>USDC</small>
          </p>
          <Strip
            n={20}
            solid={clamp(share) * 20}
            size="md"
            label={`${usd(note.balance)} of ${note.size} USDC left`}
          />
          <p className="nc-sum-line">
            <span>Expires in {30 - note.day} days</span>
            <span>of {note.size} USDC</span>
          </p>
        </section>
        <section className="nc-sum">
          <h2>Lease</h2>
          <Strip
            n={10}
            solid={chat.settled ? clamp(chat.settled.charge / bound) * 10 : 0}
            reserved={reservedCells}
            drain={chargedShare}
            size="lg"
            label="Lease bound"
          />
          <p className="nc-sum-line">
            <span>
              {chat.lease ? 'open' : chat.settled ? 'settled' : 'not open'} · bound {usd(bound)}
            </span>
            <span>used {usd(chat.usage, 3)}</span>
          </p>
        </section>
        <Fold
          title="Verification"
          badge={
            <Chip tone={passed === CHECKS.length ? 'good' : checking ? 'live' : 'plain'}>
              {passed} of {CHECKS.length}
            </Chip>
          }
        >
          <VerifyList
            state={(i) =>
              chat.turns.length === 0 && !checking
                ? 'idle'
                : chat.check > i || !checking
                  ? 'pass'
                  : chat.check === i
                    ? 'run'
                    : 'idle'
            }
          />
        </Fold>
        <Fold title="Who sees what">
          <Facts
            rows={[
              ['Operator', 'model, sizes, timing'],
              ['Model', 'your prompt'],
              ['Chain', 'nothing until you close'],
            ]}
          />
        </Fold>
        <Fold title="About the lease">
          <p className="nc-cap">
            A lease reserves its bound while it is open. When it closes, only the charge leaves the
            note. The proof for the first lease is made while you type.
          </p>
        </Fold>
        <div className="nc-sum nc-privacy-link">
          <Button onClick={onPrivacy}>
            <Icon name="shield" size={14} /> Privacy panel
          </Button>
        </div>
      </aside>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Fund

type FundStep = 'form' | 'sending' | 'done';
type Assess = 'checking' | { fresh: boolean } | null;

const ACCOUNT = /^[a-z0-9]+([._-][a-z0-9]+)*\.(testnet|near)$/;
/** A short stand-in for the account fingerprint, so the check-letter-by-letter step is shown. */
function fingerprint(account: string) {
  let h = 2166136261;
  for (const c of account) h = Math.imul(h ^ c.charCodeAt(0), 16777619) >>> 0;
  const hex = h.toString(16).padStart(8, '0') + (h ^ 0x9e3779b9).toString(16).padStart(8, '0');
  return hex.match(/.{4}/g)!.join(' ');
}

function Fund({
  nextId,
  wallet,
  onConnect,
  onFunded,
  onOpenChat,
  onNotes,
}: {
  nextId: number;
  wallet: Wallet | null;
  onConnect: (name: string) => void;
  onFunded: (size: number) => void;
  onOpenChat: () => void;
  onNotes: () => void;
}) {
  const [size, setSize] = useState(1);
  const [payout, setPayout] = useState<'self' | 'other'>('self');
  const [other, setOther] = useState('');
  const [assess, setAssess] = useState<Assess>(null);
  const [fpOk, setFpOk] = useState(false);
  const [secret, setSecret] = useState(false);
  const [pass, setPass] = useState('');
  const [downloaded, setDownloaded] = useState(false);
  const [tested, setTested] = useState(false);
  const [step, setStep] = useState<FundStep>('form');
  const [fill, setFill] = useState(0);
  const timer = useRef(0);
  const check = useRef(0);
  const fundedId = useRef(nextId);
  useEffect(
    () => () => {
      window.clearInterval(timer.current);
      window.clearTimeout(check.current);
    },
    [],
  );
  const fee = 0.05;
  const account = wallet?.account ?? '';
  const target = payout === 'self' ? account : other.trim();
  const valid = ACCOUNT.test(target);
  const locked = step !== 'form';
  const long = pass.length >= 12;

  // The payout account is looked up on the RPC provider; the sample answers after a moment.
  useEffect(() => {
    window.clearTimeout(check.current);
    setFpOk(false);
    if (payout !== 'other' || !valid || target === account) {
      setAssess(null);
      return;
    }
    setAssess('checking');
    check.current = window.setTimeout(
      () => setAssess({ fresh: !/main|old|alice/.test(target) }),
      reduced() ? 0 : 900,
    );
  }, [payout, target, valid, account]);

  const payoutReady =
    Boolean(wallet) &&
    (payout === 'self' || (valid && target !== account && fpOk && typeof assess === 'object'));
  const ready = payoutReady && secret && tested;

  const send = () => {
    fundedId.current = nextId;
    setStep('sending');
    setFill(0);
    let n = 0;
    timer.current = window.setInterval(
      () => {
        n += 1;
        setFill(n);
        if (n >= 21) {
          window.clearInterval(timer.current);
          setStep('done');
          onFunded(size);
        }
      },
      reduced() ? 1 : 80,
    );
  };

  const flags = [Boolean(wallet), Boolean(wallet), payoutReady, secret && tested, step === 'done'];
  const now = step === 'sending' ? 4 : flags.findIndex((f) => !f);
  const stepState = (i: number) => (flags[i] ? 'done' : i === now ? 'now' : 'next');
  const open = (i: number) => Boolean(wallet) || i === 0;

  return (
    <Screen
      title="Fund a note"
      lead="Deposit USDC from a NEAR wallet in a fixed size. The note secret is made in this browser and never leaves it."
      illustration={fundIllustration}
      side={
        <FundSide
          amount={size}
          unit="USDC"
          strip={{
            solid: step === 'form' ? 0 : step === 'sending' ? Math.min(fill, 20) : 20,
            reserved: step === 'form' ? 0 : step === 'sending' ? (fill > 20 ? 1 : 0) : 1,
          }}
          yours={`${usd(size)} yours · cells of ${usd(size / 20, 3)}`}
          facts={[
            ['Denomination', `${usd(size)} USDC`],
            ['Note fee', `${usd(fee)} USDC`],
            ['You send', <strong key="s">{usd(size + fee)} USDC</strong>],
          ]}
          caption="The fee is not refunded. The denomination is yours to spend or get back."
          closes={[
            ['Paid to', payoutReady || wallet ? target || '…' : 'connect a wallet'],
            ['Expires', '30 days after the deposit'],
          ]}
          leaves={
            <p className="nc-cap">
              After the transfer the app reads the deposit event from your wallet’s receipt. If the
              wallet gives no receipt, it asks the RPC provider, which then sees your IP address,
              account, the transaction hash and the time.
            </p>
          }
          limits={[
            ['Open deposits', 'capped at 100 USDC'],
            ['Sizes', '1 or 5 USDC'],
          ]}
        />
      }
    >
      <p className="nc-terms">
        By funding a note you accept the <a href="#terms">terms of use</a>.
      </p>
      <ol className="nc-steps">
        <li data-s={stepState(0)}>
          <h2>Wallet</h2>
          <div className="nc-row">
            {wallet ? (
              <>
                <span>
                  Connected as <strong>{wallet.account}</strong> (sign-in signature checked)
                </span>
                <span className="nc-wallet-chip">
                  <WalletMark name={wallet.name} size={20} />
                  {wallet.name}
                </span>
              </>
            ) : (
              <>
                <span>No wallet connected.</span>
                <WalletMenu
                  wallet={null}
                  wallets={WALLET_ITEMS}
                  delay={1100}
                  onConnect={onConnect}
                  onDisconnect={() => {}}
                />
              </>
            )}
          </div>
          <p className="nc-cap">
            {wallet
              ? 'The vault matches what this build expects: code hash, verifying keys, settings and access keys.'
              : 'On testnet, Meteor is the tested wallet. Other wallets may work and are not promised.'}
          </p>
        </li>
        <li data-s={stepState(1)} data-locked={!open(1)}>
          <h2>Size</h2>
          <div className="nc-choices" role="radiogroup" aria-label="Denomination">
            {[1, 5].map((s) => (
              <button
                key={s}
                type="button"
                role="radio"
                aria-checked={size === s}
                disabled={locked || !open(1)}
                onClick={() => setSize(s)}
              >
                <strong>{s} USDC</strong>
                <Strip n={11} solid={s === 1 ? 2 : 10} reserved={1} size="sm" />
                <small>
                  You send {usd(s + fee)} USDC: {usd(s)} stays yours, {usd(fee)} is the fee.
                </small>
              </button>
            ))}
          </div>
          <p className="nc-cap">
            Test USDC is Circle’s testnet token. Get it at <a href="#faucet">faucet.circle.com</a>{' '}
            (choose Near Testnet; 20 USDC per request, one request per address every 2 hours). Call{' '}
            <code>storage_deposit</code> on the token for your account before the transfer, and for
            the payout account if it is another one.
          </p>
        </li>
        <li data-s={stepState(2)} data-locked={!open(2)}>
          <h2>Payout account</h2>
          <p className="nc-cap nc-cap-first">
            When you close the note, or leave through an exit, the vault pays this account. It is
            fixed at deposit.
          </p>
          <div className="nc-radios" role="radiogroup" aria-label="Payout account">
            <button
              type="button"
              role="radio"
              aria-checked={payout === 'self'}
              disabled={locked || !open(2)}
              onClick={() => setPayout('self')}
            >
              <span>My sending account</span>
              <code>{account || 'connect a wallet'}</code>
            </button>
            <button
              type="button"
              role="radio"
              aria-checked={payout === 'other'}
              disabled={locked || !open(2)}
              onClick={() => setPayout('other')}
            >
              <span>Another account</span>
              <code>{other.trim() || 'a fresh account'}</code>
            </button>
          </div>
          {payout === 'self' ? (
            <p className="nc-cap">
              The deposit message will not name a payout account, so there is nothing in it to
              alter. For a payout that is not linked to this account on chain, use a fresh account
              instead.
            </p>
          ) : (
            <div className="nc-other">
              <label className="nc-field">
                <span>Payout account</span>
                <input
                  value={other}
                  placeholder="fresh-account.testnet"
                  disabled={locked}
                  onChange={(e) => setOther(e.target.value.toLowerCase())}
                />
              </label>
              {other !== '' && !valid && (
                <p className="nc-msg" data-tone="err">
                  Not a valid NEAR account id.
                </p>
              )}
              {valid && target === account && (
                <p className="nc-msg">That is your sending account: choose the first option.</p>
              )}
              {valid && target !== account && (
                <>
                  <div className="nc-fp">
                    <small>Fingerprint of {target}</small>
                    <code>{fingerprint(target)}</code>
                    <p>
                      Check the account letter by letter. A swapped account is the way this page
                      could cost you the payout.
                    </p>
                  </div>
                  <label className="nc-check">
                    <input
                      type="checkbox"
                      checked={fpOk}
                      disabled={locked}
                      onChange={(e) => setFpOk(e.target.checked)}
                    />
                    <span>
                      This is the account I want paid.
                      <small>
                        Checking the account asks the RPC provider about it, so it sees the account
                        next to your IP address.
                      </small>
                    </span>
                  </label>
                  {assess === 'checking' && <p className="nc-msg">Checking the account…</p>}
                  {typeof assess === 'object' && assess !== null && (
                    <p className="nc-msg" data-tone={assess.fresh ? 'ok' : 'warn'}>
                      {assess.fresh
                        ? 'Registered with the token and holding none yet: a fresh account.'
                        : 'This account already holds USDC, so it is not fresh. A payout to it is easy to link to its history.'}
                    </p>
                  )}
                </>
              )}
            </div>
          )}
        </li>
        <li data-s={stepState(3)} data-locked={!payoutReady}>
          <h2>Protect and back up</h2>
          <p className="nc-cap nc-cap-first">
            The secret is stored encrypted before anything is sent. Then you download a backup and
            check it by opening it again: only after that does the app offer the transfer.
          </p>
          <BackupSteps
            created={secret}
            createLabel={secret ? 'Created' : 'Create the secret'}
            createDisabled={!payoutReady || secret || locked}
            onCreate={() => setSecret(true)}
            createNote={
              secret && (
                <small>
                  Persistent storage: granted. The browser will not clear this site’s data under
                  disk pressure.
                </small>
              )
            }
            pass={pass}
            onPass={setPass}
            passDisabled={!secret || downloaded || locked}
            passMin={12}
            params={
              <small>
                Encrypted with Argon2id, 64 MiB of memory, 3 passes, 1 lane. Do not keep the file in
                a synced folder: a copy in the cloud can be opened by guessing the passphrase, and
                whoever opens it can spend the note.
              </small>
            }
            downloaded={downloaded}
            downloadLabel={downloaded ? 'Downloaded' : 'Download (.nymira-backup)'}
            downloadDisabled={!secret || !long || downloaded || locked}
            onDownload={() => setDownloaded(true)}
            tested={tested}
            chooseLabel={tested ? 'Restores' : 'Choose the file'}
            chooseDisabled={!downloaded || tested || locked}
            onChoose={() => setTested(true)}
          />
        </li>
        <li data-s={stepState(4)} data-locked={!(payoutReady && secret && tested)}>
          <h2>Send</h2>
          {step === 'done' ? (
            <div className="nc-done nc-accepted">
              <Chip tone="good">deposit accepted</Chip>
              <p>
                Note <strong>#{fundedId.current}</strong> expires in 30 days. The receipt was read
                from your wallet.
              </p>
              <ul>
                <li>
                  <a href="#explorer">Open the deposit receipt in the explorer</a>
                  <small>
                    Check there that the payout account is {target}. This page is the code you would
                    have to trust to tell you that.
                  </small>
                </li>
                <li>
                  <Button>Download a calendar reminder for the expiry</Button>
                  <small>
                    It has a generic title and no note data. A calendar that syncs to a provider
                    tells that provider the date.
                  </small>
                </li>
                <li>
                  <span>Wait a while before your first chat.</span>
                  <small>
                    A lease opened right after funding is easy to link to the deposit by timing.
                  </small>
                </li>
              </ul>
              <div className="nc-actions">
                <Button primary onClick={onOpenChat}>
                  Chat with this note
                </Button>
                <Button onClick={onNotes}>Download a fresh backup in Notes</Button>
              </div>
            </div>
          ) : (
            <>
              <div className="nc-actions nc-actions-first">
                <Button primary disabled={!ready || locked} onClick={send}>
                  {step === 'sending'
                    ? 'Waiting for the wallet and the receipt…'
                    : `Send ${usd(size + fee)} USDC`}
                </Button>
              </div>
              {!ready && (
                <p className="nc-cap">
                  {!wallet
                    ? 'Connect a wallet first.'
                    : !payoutReady
                      ? 'Choose where the payout goes.'
                      : 'Create the secret, download the backup and test it first.'}
                </p>
              )}
            </>
          )}
        </li>
      </ol>
      <Card title="Fund from another chain" aside={<Chip>quote only on testnet</Chip>}>
        <p className="nc-cap nc-cap-first">
          Pick the token you hold on another chain and 1Click quotes what the payment costs and who
          sees what. The deposit path itself is in the mainnet build.
        </p>
        <div className="nc-quotes">
          {['USDC on Ethereum', 'USDC on Base', 'USDC on Arbitrum', 'USDC on Solana'].map((a) => (
            <span key={a}>{a}</span>
          ))}
        </div>
      </Card>
    </Screen>
  );
}

// ---------------------------------------------------------------------------------------------
// Notes

const FILE_NOTES: Record<
  number,
  { payout: string; deposited: string; leases: number; spent: number }
> = {
  41: { payout: 'alice.testnet', deposited: '8 Nov 2026', leases: 3, spent: 0.021 },
  27: { payout: 'fresh-pay.testnet', deposited: '3 Nov 2026', leases: 9, spent: 0.9 },
  12: { payout: 'alice.testnet', deposited: '13 Oct 2026', leases: 14, spent: 0.67 },
};
const noteFile = (n: Note) =>
  FILE_NOTES[n.id] ?? { payout: 'alice.testnet', deposited: 'just now', leases: 0, spent: 0 };

function Notes({
  notes,
  active,
  onPick,
  go,
  backup,
  onBackup,
  onPrivacy,
}: {
  notes: Note[];
  active: number;
  onPick: (i: number) => void;
  onPrivacy: () => void;
  go: (t: Tab) => void;
  backup: { saved: boolean; when: string };
  onBackup: () => void;
}) {
  const [pass, setPass] = useState('');
  const [restored, setRestored] = useState('');
  const [hash, setHash] = useState('');
  const [lookup, setLookup] = useState('');
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const note = notes[active] ?? notes[0]!;
  const file = noteFile(note);
  const doRestore = () => {
    setRestored('reading');
    timer.current = window.setTimeout(
      () => setRestored('Restored 1 note; 1 kept the newer state this browser already holds.'),
      reduced() ? 0 : 1100,
    );
  };
  const doLookup = () => {
    setLookup('looking');
    timer.current = window.setTimeout(
      () => setLookup('Found the deposit receipt: note #44, 1.00 USDC, expires 9 Dec 2026.'),
      reduced() ? 0 : 1100,
    );
  };
  return (
    <Screen
      title="Notes and backup"
      lead="Every note this browser holds. The note secrets are encrypted before they are stored, with a key that cannot be exported."
      illustration={notesIllustration}
      side={
        <>
          <section className="nc-sum">
            <h2>Note #{note.id}</h2>
            <p className="nc-big">
              <Count value={note.balance} /> <small>USDC</small>
            </p>
            <Strip n={20} solid={clamp(note.balance / note.size) * 20} size="md" label="Balance" />
            <Facts
              rows={[
                ['Status', statusText(note)],
                ['Deposited', file.deposited],
                ['Expires', `in ${30 - note.day} days`],
                ['Payout account', file.payout],
                ['Leases so far', String(file.leases)],
                ['Spent', `${usd(file.spent)} USDC`],
              ]}
            />
            <div className="nc-actions">
              <Button primary disabled={note.status === 'closed'} onClick={() => go('chat')}>
                Chat
              </Button>
              <Button disabled={note.status === 'closed'} onClick={() => go('close')}>
                Close
              </Button>
            </div>
          </section>
          <section className="nc-sum">
            <h2>Privacy</h2>
            <p className="nc-cap nc-cap-first">
              What anyone can read about this note on chain, beside what the operator saw for each
              lease, with the set size of every lease.
            </p>
            <Button onClick={onPrivacy}>Open the privacy panel</Button>
          </section>
        </>
      }
    >
      <NoteLedger columns={['Note', 'Balance', 'Its 30 days']}>
        {notes.map((n, i) => (
          <NoteLedgerRow
            key={n.id}
            index={i}
            active={i === active}
            pick={{ label: `Select note ${n.id}`, onPick: () => onPick(i) }}
            id={{ strong: `#${n.id}`, small: `${n.size} USDC` }}
            mid={
              <>
                <strong>
                  <Count value={n.balance} />
                </strong>
                <Strip n={10} solid={clamp(n.balance / n.size) * 10} size="sm" />
              </>
            }
            late={n.status === 'expiring'}
            life={
              <>
                <Strip n={30} solid={n.day} size="sm" />
                <small>{statusText(n)}</small>
              </>
            }
            actions={
              <>
                <Button
                  onClick={() => {
                    onPick(i);
                    go('chat');
                  }}
                  disabled={n.status === 'closed'}
                >
                  Chat
                </Button>
                <Button
                  onClick={() => {
                    onPick(i);
                    go('close');
                  }}
                  disabled={n.status === 'closed'}
                >
                  Close
                </Button>
              </>
            }
          />
        ))}
      </NoteLedger>
      <Card
        title="Backup and restore"
        aside={backup.saved ? <Chip tone="good">saved</Chip> : <Chip tone="warn">out of date</Chip>}
      >
        <ul className="nc-places">
          {['~/Documents/nymira/state.age', '/Volumes/keys/nymira/state.age'].map((p, i) => (
            <li key={p} data-ok={backup.saved || i === 1}>
              <span className="nc-vmark" aria-hidden="true">
                <Icon name="check" size={12} />
              </span>
              <div>
                <code>{p}</code>
                <small>
                  {backup.saved || i === 1
                    ? i === 1 && !backup.saved
                      ? 'older state, 2 days ago'
                      : `written ${backup.when}`
                    : 'waiting for the new state'}
                </small>
              </div>
            </li>
          ))}
        </ul>
        <PassField label="Backup passphrase" value={pass} min={12} onChange={setPass} />
        <div className="nc-actions">
          <Button primary disabled={pass.length < 12} onClick={onBackup}>
            Download a backup
          </Button>
          <Button disabled={pass.length < 12 || restored === 'reading'} onClick={doRestore}>
            {restored === 'reading' ? 'Reading the file…' : 'Restore from a file'}
          </Button>
        </div>
        {restored !== '' && restored !== 'reading' && (
          <p className="nc-msg" data-tone="ok">
            {restored}
          </p>
        )}
        <p className="nc-cap">
          A backup file is encrypted with Argon2id, 64 MiB of memory, 3 passes, 1 lane. Whoever
          opens it can spend the note, so keep it out of a synced folder. A note this browser
          already holds keeps its newer state: a backup is older by definition.
        </p>
      </Card>
      <Card title="Find a deposit" aside={<Chip>asks the RPC provider</Chip>}>
        <p className="nc-cap nc-cap-first">
          If the page closed during the transfer, the note is found again from its deposit receipt.
          Enter the transaction hash from your wallet’s history or the explorer. The lookup asks the
          RPC provider, which sees your IP address, account and that hash.
        </p>
        <LookupRow
          hash={hash}
          onHash={setHash}
          onLookup={doLookup}
          disabled={hash.length < 8 || lookup === 'looking'}
          label={lookup === 'looking' ? 'Looking…' : 'Look up the receipt'}
        />
        {lookup !== '' && lookup !== 'looking' && (
          <p className="nc-msg" data-tone="ok">
            {lookup}
          </p>
        )}
      </Card>
    </Screen>
  );
}

// ---------------------------------------------------------------------------------------------
// Close

function Close({
  note,
  onClose,
  onExit,
}: {
  note: Note;
  onClose: (id: number) => void;
  onExit: (id: number) => void;
}) {
  const [path, setPath] = useState<'close' | 'exit'>('close');
  const [backed, setBacked] = useState(false);
  const [from, setFrom] = useState<'same' | 'other'>('same');
  const [run, setRun] = useState(0);
  const [days, setDays] = useState(0);
  const timer = useRef(0);
  useEffect(() => () => window.clearInterval(timer.current), []);
  const fee = 0.03;
  const file = noteFile(note);
  const payout = Math.max(note.balance - fee, 0);
  const finished = note.status === 'closed' || note.status === 'exiting';
  const drained = finished && path === 'close';
  const stages =
    path === 'close'
      ? ['Wallet signs the close', 'Operator signs a clearance', 'Vault pays the payout account']
      : [
          'Wallet signs request_exit',
          'The vault records the request',
          'Seven days pass (time-lapse)',
        ];
  const doneAt = path === 'close' ? [2, 3, 4] : [2, 3, 10];
  const total = doneAt[2]!;

  const go = () => {
    setRun(1);
    let n = 1;
    timer.current = window.setInterval(
      () => {
        n += 1;
        setRun(n);
        if (path === 'exit' && n >= 3) setDays(Math.min(7, n - 2));
        if (n >= total) {
          window.clearInterval(timer.current);
          if (path === 'close') onClose(note.id);
          else onExit(note.id);
        }
      },
      reduced() ? 1 : path === 'exit' ? 700 : 900,
    );
  };

  return (
    <Screen
      title="Close or exit a note"
      lead="Closing returns the balance to the note’s payout account. If the operator will not sign, you can leave without it."
      illustration={closeIllustration}
      side={
        <>
          <section className="nc-sum">
            <h2>Note #{note.id}</h2>
            <p className="nc-big">
              <Count value={drained ? 0 : note.balance} /> <small>USDC</small>
            </p>
            <Strip
              n={20}
              solid={drained ? 0 : clamp(note.balance / note.size) * 20}
              size="md"
              label="Balance"
            />
            <p className="nc-sum-line">
              <span>of {note.size} USDC</span>
              <span>{drained ? 'paid out' : statusText(note)}</span>
            </p>
          </section>
          <Fold title="What the chain shows" badge={<Chip tone="warn">public</Chip>}>
            <Facts
              rows={[
                ['Note ID', `#${note.id}`],
                ['Total spend', `${usd(file.spent)} USDC`],
                ['Payout account', file.payout],
                ['Sent from', from === 'same' ? 'alice.testnet' : 'another account'],
              ]}
            />
            <p className="nc-cap">
              Sending the close from the account that funded the note puts the two side by side.
              Another account with a little NEAR keeps them apart.
            </p>
          </Fold>
          <Fold title="If you lose your latest state">
            <p className="nc-cap">
              An exit from an old state can be defeated: the operator can answer your exit with a
              later request and keep what that request charged, up to the whole remaining balance.
              Keep a current backup.
            </p>
          </Fold>
        </>
      }
    >
      <div className="nc-choices nc-choices-wide" role="radiogroup" aria-label="How to leave">
        <button
          type="button"
          role="radio"
          aria-checked={path === 'close'}
          disabled={run > 0}
          onClick={() => setPath('close')}
        >
          <strong>Close with the operator</strong>
          <small>
            The operator signs a clearance. The vault pays the balance less a {usd(fee)} USDC close
            fee. It is the fast way.
          </small>
        </button>
        <button
          type="button"
          role="radio"
          aria-checked={path === 'exit'}
          disabled={run > 0}
          onClick={() => setPath('exit')}
        >
          <strong>Exit without the operator</strong>
          <small>
            Request the exit, wait seven days, then anyone can finalize it. The operator can deduct
            at most the bound of the one lease in flight.
          </small>
        </button>
      </div>

      <div className="nc-payout" data-path={path}>
        <div className="nc-payout-flow">
          <div>
            <small>Balance</small>
            <strong>{usd(note.balance)}</strong>
            <Strip n={10} solid={drained ? 0 : clamp(note.balance / note.size) * 10} size="sm" />
          </div>
          {path === 'close' ? (
            <>
              <span className="nc-payout-minus" aria-hidden="true">
                −
              </span>
              <div>
                <small>Close fee</small>
                <strong>{usd(fee)}</strong>
                <Strip
                  n={10}
                  solid={0}
                  reserved={Math.max(1, Math.round((fee / note.size) * 10))}
                  size="sm"
                />
              </div>
            </>
          ) : (
            <>
              <span className="nc-payout-minus" aria-hidden="true">
                −
              </span>
              <div>
                <small>At most one lease</small>
                <strong>0.10</strong>
                <Strip n={10} solid={0} reserved={1} size="sm" />
              </div>
            </>
          )}
          <span className="nc-payout-arrow" aria-hidden="true">
            <Icon name="right" size={18} />
          </span>
          <div data-to>
            <small>{file.payout}</small>
            <strong>{path === 'close' ? usd(payout) : `up to ${usd(note.balance)}`}</strong>
            <Strip
              n={10}
              solid={finished && path === 'close' ? clamp(payout / note.size) * 10 : 0}
              size="sm"
            />
          </div>
        </div>
      </div>

      <fieldset className="nc-from" disabled={run > 0}>
        <legend>Send the {path === 'close' ? 'close' : 'exit request'} from</legend>
        <div className="nc-radios" role="radiogroup" aria-label="Sending account">
          <button
            type="button"
            role="radio"
            aria-checked={from === 'same'}
            onClick={() => setFrom('same')}
          >
            <span>The account that funded the note</span>
            <code>alice.testnet</code>
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={from === 'other'}
            onClick={() => setFrom('other')}
          >
            <span>Another account with a little NEAR</span>
            <code>keeps them apart on chain</code>
          </button>
        </div>
      </fieldset>

      {path === 'exit' && (
        <ExitDays days={days} requested={run > 0}>
          <p className="nc-cap">
            request_exit and finalize_exit need a little NEAR for gas. Any account can send them,
            and the payout still goes to the payout account.
          </p>
        </ExitDays>
      )}
      <FlowSteps
        steps={stages.map((text, i) => ({
          text,
          s: run >= doneAt[i]! ? 'done' : run >= i + 1 ? 'now' : 'next',
        }))}
      />
      <label className="nc-check">
        <input
          type="checkbox"
          checked={backed}
          disabled={run > 0}
          onChange={(e) => setBacked(e.target.checked)}
        />
        <span>
          I have the latest backup of this note.
          <small>An exit from an older state can be defeated.</small>
        </span>
      </label>
      {finished ? (
        <div className="nc-done">
          <Chip tone="good">{note.status === 'closed' ? 'paid out' : 'exit requested'}</Chip>
          <p>
            {note.status === 'closed'
              ? `${usd(payout)} USDC went to ${file.payout}. The note is closed.`
              : 'The seven days have started. Anyone can finalize the exit after they pass.'}
          </p>
        </div>
      ) : (
        <div className="nc-actions">
          <Button primary disabled={!backed || run > 0} onClick={go}>
            {run > 0
              ? 'Working…'
              : path === 'close'
                ? `Close and receive ${usd(payout)} USDC`
                : 'Request the exit'}
          </Button>
        </div>
      )}
      <GuardCard />
    </Screen>
  );
}

// ---------------------------------------------------------------------------------------------
// Agents

interface Agent {
  name: string;
  client: ClientId;
  transport: 'tcp' | 'unix';
  note: string;
  spent: number;
  limit: number;
  on: boolean;
}

function Agents({ wallet }: { wallet: Wallet | null }) {
  const [agents, setAgents] = useState<Agent[]>([
    {
      name: 'cursor',
      client: 'cursor',
      transport: 'unix',
      note: '#31',
      spent: 0.34,
      limit: 1,
      on: true,
    },
    {
      name: 'worker',
      client: 'ironclaw',
      transport: 'tcp',
      note: '#36',
      spent: 0.71,
      limit: 1,
      on: true,
    },
  ]);
  const [held, setHeld] = useState<'wait' | 'approved' | 'denied'>('wait');
  const [pick, setPick] = useState<ClientId>('claude');
  const [name, setName] = useState('research');
  const client = CLIENTS.find((c) => c.id === pick)!;
  const [transport, setTransport] = useState<'tcp' | 'unix'>(client.transport);
  const [daily, setDaily] = useState('1.00');
  const [call, setCall] = useState('0.10');
  const [confirm, setConfirm] = useState('0.05');
  const [rate, setRate] = useState('10');
  const [tee, setTee] = useState(true);
  const [signed, setSigned] = useState(0);
  const [signing, setSigning] = useState(false);
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const choose = (c: ClientInfo) => {
    setPick(c.id);
    setTransport(c.transport);
  };
  const token = `/srv/agent/${name || 'NAME'}.token`;
  const create = `nymirad agent create --name ${name || 'NAME'} --transport ${transport} --key-file KEY --amount 1000000`;
  const connect = client.host
    ? `node tools/host-config.ts --host ${client.host} --install-dir /opt/nymira-mcp \\\n  --token-file ${token} ` +
      (transport === 'unix'
        ? '--transport unix --socket /tmp/nymira-agent-501/agent.sock'
        : '--transport tcp --url http://127.0.0.1:8788')
    : `base_url = http://localhost:8788/v1\nAPI key   = the contents of ${token}`;
  const yaml = [
    'allowed_models: z-ai/glm-5.3-flash',
    `max_lease_usd: 0.10`,
    `max_call_usd: ${call || '…'}`,
    `max_daily_usd: ${daily || '…'}`,
    `confirm_above_usd: ${confirm || '…'}`,
    `rate_limit_per_min: ${rate || '…'}`,
    `require_tee: ${tee}`,
  ].join('\n');
  const num = (v: string) => /^\d+(\.\d{1,2})?$/.test(v);
  const okForm = num(daily) && num(call) && num(confirm) && /^\d+$/.test(rate);
  const sign = () => {
    setSigning(true);
    timer.current = window.setTimeout(
      () => {
        setSigning(false);
        setSigned((v) => v + 1);
      },
      reduced() ? 0 : 1200,
    );
  };
  return (
    <Screen
      title="Agents"
      lead="Give an agent its own note and token. Limits live in your own nymirad, and only you can approve a held call or disable a token."
      illustration={agentsIllustration}
    >
      <div className="nc-agents">
        {agents.map((a, i) => {
          const c = CLIENTS.find((x) => x.id === a.client)!;
          return (
            <article key={a.name} className="nc-agent" data-on={a.on}>
              <header>
                <span className="nc-agent-id">
                  <ClientMark client={c} size={32} />
                  <span>
                    <h2>{a.name}</h2>
                    <small>{c.name}</small>
                  </span>
                </span>
                <span>
                  <Chip>{a.transport}</Chip>
                  <Chip>note {a.note}</Chip>
                </span>
              </header>
              <Strip n={20} solid={clamp(a.spent / a.limit) * 20} size="md" label="Spent today" />
              <div className="nc-agent-foot">
                <small>
                  Spent today {usd(a.spent)} of {usd(a.limit)} USD
                </small>
                <button
                  type="button"
                  role="switch"
                  aria-checked={a.on}
                  className="nc-switch"
                  onClick={() =>
                    setAgents((x) => x.map((y, j) => (j === i ? { ...y, on: !y.on } : y)))
                  }
                >
                  <span className="nc-track" aria-hidden="true">
                    <i />
                  </span>
                  <span>{a.on ? 'Enabled' : 'Disabled'}</span>
                </button>
              </div>
              {!a.on && <p className="nc-agent-off">Its next request gets 401 token_disabled.</p>}
            </article>
          );
        })}
      </div>

      <Card
        title="Held for you"
        aside={held === 'wait' ? <Chip tone="live">waiting</Chip> : <Chip>{held}</Chip>}
      >
        <div className="nc-held">
          <div>
            <p>
              <strong>cursor</strong> asks for a call with a worst case of 0.08 USD, above the 0.05
              USD confirmation threshold. The agent cannot approve its own request.
            </p>
            <Facts
              rows={[
                ['Model', 'z-ai/glm-5.3-flash'],
                ['Budget note', '#31'],
                ['Confirm above', '0.05 USD'],
              ]}
            />
          </div>
          <div className="nc-actions">
            <Button disabled={held !== 'wait'} onClick={() => setHeld('denied')}>
              Deny
            </Button>
            <Button primary disabled={held !== 'wait'} onClick={() => setHeld('approved')}>
              Approve once
            </Button>
          </div>
        </div>
      </Card>

      <Card title="Connect a client" aside={<Chip>pick one</Chip>}>
        <ClientGrid pick={pick} onPick={choose} />
        <div className="nc-newagent">
          <label className="nc-field">
            <span>Agent name</span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value.replace(/[^a-z0-9-]/gi, '').slice(0, 24))}
            />
          </label>
          <div className="nc-field">
            <span>Transport</span>
            <div className="nc-seg" role="radiogroup" aria-label="Transport">
              {(['tcp', 'unix'] as const).map((t) => (
                <button
                  key={t}
                  type="button"
                  role="radio"
                  aria-checked={transport === t}
                  onClick={() => setTransport(t)}
                >
                  {t}
                </button>
              ))}
            </div>
          </div>
        </div>
        <ol className="nc-cmds">
          <li>
            <strong>1 · Create the agent and its note</strong>
            <pre className="nc-code">
              <i>$ </i>
              {create}
            </pre>
          </li>
          <li>
            <strong>
              2 · {client.host ? `Write ${client.name}’s entry` : `Point ${client.name} at nymirad`}
            </strong>
            <pre className="nc-code">
              {client.host && <i>$ </i>}
              {connect}
            </pre>
            {client.file && <small>Paste the printed JSON into {client.file}.</small>}
          </li>
        </ol>
        <p className="nc-cap">
          A token works on one transport only: unix on the agent socket, tcp on loopback port 8788.
          Tokens are never stored, only their hashes.{' '}
          {client.via === 'MCP over stdio'
            ? 'The entry holds the path of the token file, never the token.'
            : 'The token works as an OpenAI API key on chat and models.'}
        </p>
        {client.id === 'codex' && (
          <p className="nc-msg" data-tone="warn">
            Nymira has no Codex recipe yet. Any client that takes an OpenAI base URL and key can use
            the values above, with chat completions only.
          </p>
        )}
      </Card>

      <Card
        title="Policy"
        aside={
          signed > 0 ? (
            <Chip tone="good">version {signed} signed</Chip>
          ) : (
            <Chip tone="warn">no policy: 402 policy_missing</Chip>
          )
        }
      >
        <div className="nc-policy">
          <div className="nc-policy-form">
            {(
              [
                ['Daily limit (USD)', daily, setDaily],
                ['Per-call ceiling (USD)', call, setCall],
                ['Confirm above (USD)', confirm, setConfirm],
                ['Requests per minute', rate, setRate],
              ] as const
            ).map(([label, value, set]) => (
              <label className="nc-field" key={label}>
                <span>{label}</span>
                <input value={value} inputMode="decimal" onChange={(e) => set(e.target.value)} />
              </label>
            ))}
            <label className="nc-check">
              <input type="checkbox" checked={tee} onChange={(e) => setTee(e.target.checked)} />
              <span>
                Require the attested model
                <small>
                  require_tee: an answer is released only after its signature is checked.
                </small>
              </span>
            </label>
          </div>
          <pre className="nc-code nc-yaml">{yaml}</pre>
        </div>
        <div className="nc-actions">
          <Button primary disabled={!okForm || signing || !wallet} onClick={sign}>
            {wallet && <WalletMark name={wallet.name} size={18} />}
            {signing ? 'Waiting for the wallet…' : 'Sign and load the policy'}
          </Button>
          {!wallet && <small className="nc-hint">Connect a wallet first.</small>}
        </div>
        <p className="nc-cap">
          You sign with your NEAR account (NEP-413) and a full-access key. The policy applies from
          the agent’s next request. Without a valid signed policy, every agent request is refused
          before anything leaves the machine.
        </p>
      </Card>
    </Screen>
  );
}

// ---------------------------------------------------------------------------------------------
// Privacy panel: what anyone can read on chain beside what the operator saw. The sentences are
// the ones in shared/privacy-strings.json and web/src/privacy/text.ts; the figures are samples.

const P = {
  address:
    'The operator, and the hosts in front of it, see your network address and the time of every lease. The RPC providers this page asks for the set size see your address too. This page cannot hide them and never shows an address. Use Tor Browser, or run nymirad, which sends its traffic through Tor.',
  leftIntro:
    'What anyone can read on chain about this note. It comes from the record this browser keeps; the page asks no node for it.',
  rightIntro: 'What the operator saw for each lease on this note.',
  linkOpen:
    'The proofs are designed to leave no zero-knowledge link between the deposit and the leases. The leases can still be linked by network address, by timing and usage metadata, or by timing alone while the set is small.',
  policyLabel: 'The operator’s published retention policy. The client cannot check it.',
  policy:
    'After a lease settles, the operator keeps the nullifier, the charge, the time rounded to the hour and the delivery status. It keeps the signed states and request proofs for 44 days, on both of its machines, and deletes the key ID after the provider’s usage is reconciled. It copies these records to its second machine.',
  smallSet: 'With so few notes in the set, matching a lease to a deposit is easy.',
  standsOut: (bound: string) =>
    `This lease used the ${bound} class. Few leases use it and the operator sees the bound, so it stands out.`,
  timing: 'Timing can match this lease to the deposit.',
  fiveNotSafe: 'With 5 or more external notes, 5 is still not a safe number.',
  balancesPrivate:
    'Note balances are private. N is the most notes that could have paid for the lease; fewer may have had enough balance.',
  includesOwn:
    'N includes your own note. One person can hold several notes, so N can count the same person more than once.',
  team: 'The team list is the team’s own declaration of the notes it holds. The pinned list ships with this build, and the operator may list more IDs; any difference between the two is shown.',
  unlisted: 'N includes notes the operator or the team control and did not list.',
  thresholds:
    'Thresholds used on this page: a lease less than 1 hour after the deposit, or with fewer than 5 external deposits in between, is flagged; an external set under 5 is called small.',
  testnet:
    'This is a testnet build. The notes are test notes funded with free testnet USDC, anyone can add them at no cost, and the set is small.',
  leaving: [
    'A close shows the note ID and the total spend.',
    'A clearance request shows the operator the note ID and the balance. In the browser it comes from the same address as the leases.',
    'An exit links the last request to the note.',
    'A raise or a defeat ties the answered lease to the note.',
    'The account that sends a close or an exit shows on chain next to the note ID.',
  ],
  exitPurpose: 'An exit is there to keep the money safe. It does not keep the user private.',
  reuse: {
    evidence:
      'The operator’s status for the request nullifier of the last settled lease is the evidence.',
    finalized: 'finalized: the operator treats this request nullifier as spent.',
    resent: 'The stored request is inside its 300 seconds, so it was sent to the operator again.',
    refuses: 'the operator refuses',
    says: 'operator says:',
    read: 'Read the reason. Only a reason about the request nullifier shows the rule. An old proof, a stale root or a missing puzzle is refused for other reasons.',
    rule: 'A refusal is the operator’s own rule, backed by its replicated records. The vault does not check it.',
    none: 'No lease on this note has settled yet, so there is no request nullifier to check.',
  },
};

// 0 = not in the sentence, 1 = the sentence says so, 2 = only in part or only on leaving.
const OBSERVERS: readonly {
  who: string;
  text: string;
  sees: readonly [ip: number, times: number, prompts: number, note: number];
}[] = [
  {
    who: 'Chain',
    text: 'The chain shows your deposit and, when you close, the note’s ID, total spend and payout account.',
    sees: [0, 0, 0, 2],
  },
  {
    who: 'Operator',
    text: 'The operator sees your IP address, request times and proofs, not your prompts.',
    sees: [1, 1, 0, 2],
  },
  {
    who: 'Operator’s public entry',
    text: 'The operator’s public entry sees your network address and the time of every connection.',
    sees: [1, 1, 0, 0],
  },
  {
    who: 'Operator’s second machine',
    text: 'The operator’s second machine holds a copy of the lease records, with times rounded to the hour.',
    sees: [0, 2, 0, 0],
  },
  {
    who: 'NEAR AI',
    text: 'The model in NEAR AI’s TEE reads your prompts. NEAR AI sees the model, the token counts and the timing of each lease, and can tie the requests of one lease together. It does not see which note the lease belongs to.',
    sees: [1, 1, 1, 0],
  },
  {
    who: 'RPC providers',
    text: 'RPC providers see your network address with every read and send. The funding step reads the receipt of your own deposit from this browser, so the provider that answers sees your address next to that deposit.',
    sees: [1, 2, 0, 2],
  },
  {
    who: 'Web host',
    text: 'The web host sees your network address and which files you ask for, and it decides which code you get.',
    sees: [1, 0, 0, 0],
  },
  {
    who: 'Attestation services',
    text: 'The attestation services, NEAR AI’s attestation endpoint and Phala’s PCCS, see the address that asks and the time.',
    sees: [1, 1, 0, 0],
  },
  {
    who: 'Certificate logs',
    text: 'Public certificate logs list the operator’s host name, which the certificate of its public entry carries. They show that the name exists, not who connects.',
    sees: [0, 0, 0, 0],
  },
];

interface SampleLease {
  id: string;
  at: string;
  open?: boolean;
  bound: number;
  charge?: number;
  model: string;
  sampling: string;
  msgs: number;
  bytes: number;
  tin: number;
  tout: number;
  ext: number;
  team: number;
  gap: string;
  between: number;
  extBetween: number;
}
const SAMPLE_LEASES: readonly SampleLease[] = [
  {
    id: 'ls_9f2c41',
    at: '2026-11-07 14:02:11 UTC',
    bound: 0.1,
    charge: 0.012,
    model: 'z-ai/glm-5.3-flash',
    sampling: 'temperature 0.7, not streamed',
    msgs: 2,
    bytes: 318,
    tin: 33,
    tout: 7,
    ext: 14,
    team: 2,
    gap: '2 days',
    between: 11,
    extBetween: 9,
  },
  {
    id: 'ls_1b77aa',
    at: '2026-11-08 09:41:56 UTC',
    bound: 0.5,
    charge: 0.031,
    model: 'Qwen/Qwen3.8-27B',
    sampling: 'streamed',
    msgs: 4,
    bytes: 902,
    tin: 120,
    tout: 211,
    ext: 6,
    team: 0,
    gap: '1 day',
    between: 3,
    extBetween: 2,
  },
  {
    id: 'ls_c03d19',
    at: '2026-11-09 13:15:02 UTC',
    open: true,
    bound: 0.1,
    model: 'z-ai/glm-5.3-flash',
    sampling: 'not streamed',
    msgs: 1,
    bytes: 96,
    tin: 11,
    tout: 0,
    ext: 3,
    team: 0,
    gap: '12 minutes',
    between: 1,
    extBetween: 0,
  },
];

const flagsOf = (l: SampleLease): Flag[] => {
  const out: Flag[] = [];
  if (l.bound > 0.1) out.push({ kind: 'stands', text: P.standsOut(`${usd(l.bound)} USDC`) });
  if (l.ext < 5) out.push({ kind: 'small', text: P.smallSet });
  if (l.extBetween < 5 || l.gap.includes('minute')) out.push({ kind: 'timing', text: P.timing });
  return out;
};

/** One lease, in full: what the operator recorded, how many notes could have paid, what to watch. */
function LeaseDetail({ l, i }: { l: SampleLease; i: number }) {
  const flags = flagsOf(l);
  return (
    <div className="nc-detail" key={l.id}>
      <header>
        <h3>{l.at}</h3>
        {l.open && <Chip tone="live">still open</Chip>}
        <code>{l.id}</code>
      </header>
      <div className="nc-set">
        <p className="nc-set-head">
          <strong>{l.ext}</strong>
          <span>
            set size: {l.ext} (external) · total {l.ext + l.team}
          </span>
        </p>
        <SetCells ext={l.ext} team={l.team} label={`${l.ext} external notes could have paid`} />
        <SetLegend />
        {i === 0 && <p className="nc-cap">team IDs only in this release’s pinned list: 7, 9</p>}
        {l.ext >= 5 && <p className="nc-cap">{P.fiveNotSafe}</p>}
      </div>
      {flags.length > 0 && (
        <ul className="nc-flaglist">
          {flags.map((f) => (
            <li key={f.kind}>
              <Chip tone="warn">{FLAG_NAME[f.kind]}</Chip>
              <span>{f.text}</span>
            </li>
          ))}
        </ul>
      )}
      <dl className="nc-facts">
        <div>
          <dt>Bound class</dt>
          <dd>{usd(l.bound)} USDC</dd>
        </div>
        <div>
          <dt>Charge</dt>
          <dd>{l.charge !== undefined ? `${usd(l.charge, 3)} USDC` : 'not settled yet'}</dd>
        </div>
        <div>
          <dt>Model</dt>
          <dd>{l.model}</dd>
        </div>
        <div>
          <dt>Settings</dt>
          <dd>{l.sampling}</dd>
        </div>
        <div>
          <dt>Messages</dt>
          <dd>
            {l.msgs}, {l.bytes} bytes
          </dd>
        </div>
        <div>
          <dt>Tokens</dt>
          <dd>
            {l.tin} in, {l.tout} out
          </dd>
        </div>
        <div>
          <dt>After the deposit</dt>
          <dd>{l.gap}</dd>
        </div>
        <div>
          <dt>Joined the tree since</dt>
          <dd>
            {l.between} deposits, {l.extBetween} external
          </dd>
        </div>
      </dl>
    </div>
  );
}

function ReuseCard({ hasLease }: { hasLease: boolean }) {
  const [state, setState] = useState<'idle' | 'busy' | 'done'>('idle');
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const run = () => {
    setState('busy');
    timer.current = window.setTimeout(() => setState('done'), reduced() ? 0 : 1300);
  };
  return (
    <Card title="Check reuse" aside={<Chip>asks the operator</Chip>}>
      {!hasLease ? (
        <p className="nc-cap nc-cap-first">{P.reuse.none}</p>
      ) : (
        <>
          <p className="nc-cap nc-cap-first">{P.reuse.evidence}</p>
          <div className="nc-actions">
            <Button disabled={state === 'busy'} onClick={run}>
              {state === 'busy' ? 'Asking the operator…' : 'Check reuse'}
            </Button>
          </div>
          {state === 'done' && (
            <div className="nc-reuse">
              <p className="nc-msg" data-tone="ok">
                {P.reuse.finalized}
              </p>
              <p>{P.reuse.resent}</p>
              <p className="nc-flag">
                <strong>{P.reuse.refuses}</strong>
              </p>
              <div className="nc-says">
                <small>{P.reuse.says}</small>
                <pre>request nullifier already spent</pre>
              </div>
              <p className="nc-cap">{P.reuse.read}</p>
            </div>
          )}
        </>
      )}
      <p className="nc-cap">{P.reuse.rule}</p>
    </Card>
  );
}

const CELL_STATES = ['none', 'yes', 'part'] as const;

type PTab = 'overview' | 'leases' | 'chain' | 'observers' | 'leaving' | 'limits';

function PrivacyPanel({ note, onBack }: { note: Note; onBack: () => void }) {
  const file = noteFile(note);
  const leases = SAMPLE_LEASES.slice(0, Math.min(3, file.leases));
  const [tab, setTab] = useState<PTab>('overview');
  const [sel, setSel] = useState(Math.max(0, leases.length - 1));
  const [open, setOpen] = useState<string | null>(null);
  const [more, setMore] = useState(false);
  const last = leases[leases.length - 1];
  const nowExt = last ? last.ext + 2 : 16;
  const allFlags = leases.flatMap((l, i) => flagsOf(l).map((f) => ({ ...f, l, i })));
  const tabs: readonly [PTab, string, string?][] = [
    ['overview', 'Overview'],
    ['leases', 'Leases', String(leases.length)],
    ['chain', 'On chain'],
    ['observers', 'Who sees what'],
    ['leaving', 'Leaving and reuse'],
    ['limits', 'Limits'],
  ];
  const openLease = (i: number) => {
    setSel(i);
    setTab('leases');
  };
  const keyNav = (e: React.KeyboardEvent) => {
    const at = tabs.findIndex(([id]) => id === tab);
    if (e.key === 'ArrowRight') setTab(tabs[(at + 1) % tabs.length]![0]);
    if (e.key === 'ArrowLeft') setTab(tabs[(at - 1 + tabs.length) % tabs.length]![0]);
  };
  return (
    <div className="nc-privacy">
      <div className="nc-privacy-bar">
        <Button onClick={onBack}>
          <Icon name="chevron" size={12} /> Back
        </Button>
        <h1>Privacy panel</h1>
        <Chip>note #{note.id}</Chip>
      </div>
      <div className="nc-address" data-more={more}>
        <Icon name="onion" size={16} />
        <p>
          <strong>This page cannot hide your network address.</strong>{' '}
          {more
            ? P.address
            : 'The operator, the hosts in front of it and the RPC providers see it.'}{' '}
          <button type="button" className="nc-link" onClick={() => setMore(!more)}>
            {more ? 'Less' : 'Read more'}
          </button>
        </p>
      </div>

      <div
        className="nc-ptabs"
        role="tablist"
        aria-label="Privacy panel sections"
        onKeyDown={keyNav}
      >
        {tabs.map(([id, label, count]) => (
          <button
            key={id}
            type="button"
            role="tab"
            id={`ptab-${id}`}
            aria-selected={tab === id}
            aria-controls="ptab-panel"
            tabIndex={tab === id ? 0 : -1}
            onClick={() => setTab(id)}
          >
            {label}
            {count && <em>{count}</em>}
          </button>
        ))}
      </div>

      <div
        className="nc-ppanel"
        role="tabpanel"
        id="ptab-panel"
        aria-labelledby={`ptab-${tab}`}
        key={tab}
      >
        {tab === 'overview' && (
          <>
            <section className="nc-glance">
              <h2>At a glance</h2>
              <ul>
                <li>
                  <span className="nc-glance-k">Set size now</span>
                  <span className="nc-glance-v">
                    <strong>{nowExt}</strong> external notes could pay
                    <SetCells ext={nowExt} team={2} label="Notes that could pay now" />
                  </span>
                  <small>
                    read from the vault, from one RPC provider only (the second is not set up)
                  </small>
                </li>
                <li>
                  <span className="nc-glance-k">Latest lease</span>
                  <span className="nc-glance-v">
                    {last ? (
                      <>
                        <strong>{last.ext}</strong> could have paid · {last.gap} after the deposit
                      </>
                    ) : (
                      'No lease has been made on this note yet.'
                    )}
                  </span>
                  {last && (
                    <small>
                      {last.at} · {flagsOf(last).length} flag{flagsOf(last).length === 1 ? '' : 's'}
                    </small>
                  )}
                </li>
                <li>
                  <span className="nc-glance-k">A close would show</span>
                  <span className="nc-glance-v nc-glance-chips">
                    <Chip>note #{note.id}</Chip>
                    <Chip>{usd(file.spent)} USDC spent</Chip>
                    <Chip>{file.payout}</Chip>
                  </span>
                  <small>next to the account that sends the close</small>
                </li>
              </ul>
            </section>

            <section className="nc-flags">
              <h2>
                What to watch{' '}
                <Chip tone={allFlags.length ? 'warn' : 'good'}>{allFlags.length}</Chip>
              </h2>
              {allFlags.length === 0 ? (
                <p className="nc-cap nc-cap-first">No lease on this note has a flag.</p>
              ) : (
                <ul>
                  {allFlags.map((f) => (
                    <li key={f.l.id + f.kind}>
                      <Chip tone="warn">{FLAG_NAME[f.kind]}</Chip>
                      <span>{f.text}</span>
                      <button type="button" className="nc-link" onClick={() => openLease(f.i)}>
                        {f.l.at.slice(5, 16)}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              <p className="nc-cap">{P.testnet}</p>
            </section>

            <section className="nc-join">
              <h2>What joins the two views</h2>
              <div className="nc-join-diagram" aria-hidden="true">
                <span>
                  <b>On chain</b>
                  <small>deposit</small>
                </span>
                <i />
                <span>
                  <b>Operator</b>
                  <small>leases</small>
                </span>
              </div>
              <p>{P.linkOpen}</p>
            </section>
          </>
        )}

        {tab === 'leases' && (
          <div className="nc-master">
            {leases.length === 0 ? (
              <p className="nc-empty-line">No lease has been made on this note yet.</p>
            ) : (
              <>
                <ul className="nc-lease-pick" role="listbox" aria-label="Leases">
                  {leases.map((l, i) => {
                    const fl = flagsOf(l);
                    return (
                      <li key={l.id}>
                        <button
                          type="button"
                          role="option"
                          aria-selected={sel === i}
                          onClick={() => setSel(i)}
                        >
                          <span className="nc-lp-top">
                            <strong>{l.at.slice(5, 16)}</strong>
                            {l.open && <Chip tone="live">open</Chip>}
                          </span>
                          <span className="nc-lp-set">
                            <b>{l.ext}</b> could have paid
                          </span>
                          <span className="nc-lp-flags">
                            {fl.length === 0 ? (
                              <small>no flags</small>
                            ) : (
                              fl.map((f) => (
                                <Chip key={f.kind} tone="warn">
                                  {FLAG_NAME[f.kind]}
                                </Chip>
                              ))
                            )}
                          </span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
                <LeaseDetail l={leases[sel] ?? leases[0]!} i={sel} />
              </>
            )}
          </div>
        )}

        {tab === 'chain' && (
          <section className="nc-chain">
            <h2>On chain</h2>
            <p className="nc-cap nc-cap-first">{P.leftIntro}</p>
            <dl className="nc-facts">
              <div>
                <dt>Deposit transaction</dt>
                <dd>
                  <a href="#explorer">8Qm4…c91Kd</a>
                </dd>
              </div>
              <div>
                <dt>Note ID</dt>
                <dd>#{note.id}</dd>
              </div>
              <div>
                <dt>Commitment</dt>
                <dd>
                  <code>0x3fa1…9be2</code>
                </dd>
              </div>
              <div>
                <dt>Denomination</dt>
                <dd>{usd(note.size)} USDC</dd>
              </div>
              <div>
                <dt>Expiry</dt>
                <dd>in {30 - note.day} days</dd>
              </div>
              <div>
                <dt>Payout account</dt>
                <dd>{file.payout}</dd>
              </div>
              <div>
                <dt>Note fee</dt>
                <dd>0.05 USDC</dd>
              </div>
            </dl>
          </section>
        )}

        {tab === 'observers' && (
          <section className="nc-observers">
            <h2>Who sees what</h2>
            <div className="nc-matrix" role="table" aria-label="Who sees what">
              <div className="nc-matrix-head" role="row">
                <span />
                <span>IP</span>
                <span>Times</span>
                <span>Prompts</span>
                <span>Note ID</span>
              </div>
              {OBSERVERS.map((o) => (
                <div key={o.who} className="nc-matrix-row" role="row" data-open={open === o.who}>
                  <button
                    type="button"
                    aria-expanded={open === o.who}
                    onClick={() => setOpen(open === o.who ? null : o.who)}
                  >
                    <span>{o.who}</span>
                    <Icon name="chevron" size={12} />
                  </button>
                  {o.sees.map((v, k) => (
                    <i
                      key={k}
                      data-s={CELL_STATES[v]}
                      aria-label={`${['IP', 'Times', 'Prompts', 'Note ID'][k]}: ${['no', 'yes', 'in part'][v]}`}
                    />
                  ))}
                  {open === o.who && <p>{o.text}</p>}
                </div>
              ))}
            </div>
            <p className="nc-cap">
              A solid cell means the sentence says so, a half cell means only in part or only when
              you leave. Open a row for the sentence itself.
            </p>
          </section>
        )}

        {tab === 'leaving' && (
          <>
            <section className="nc-leave">
              <h2>What leaving reveals</h2>
              <ul className="nc-leaving">
                {P.leaving.map((t) => (
                  <li key={t}>
                    <span className="nc-vmark" aria-hidden="true" />
                    {t}
                  </li>
                ))}
              </ul>
              <p className="nc-callout">{P.exitPurpose}</p>
            </section>
            <ReuseCard hasLease={leases.some((l) => !l.open)} />
          </>
        )}

        {tab === 'limits' && (
          <>
            <section className="nc-limits">
              <h2>What the figures do not say</h2>
              <ul className="nc-honest">
                {[
                  P.balancesPrivate,
                  P.includesOwn,
                  P.team,
                  P.unlisted,
                  P.fiveNotSafe,
                  P.thresholds,
                  P.testnet,
                ].map((t) => (
                  <li key={t}>{t}</li>
                ))}
              </ul>
            </section>
            <Card
              title="The operator’s retention policy"
              aside={<Chip>the client cannot check it</Chip>}
            >
              <p className="nc-cap nc-cap-first">{P.policyLabel}</p>
              <p className="nc-policy-text">{P.policy}</p>
            </Card>
          </>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Unlock: the key that wraps the note secrets, kept in memory while the page is open.

function Unlock({ onUnlock }: { onUnlock: () => void }) {
  const [mode, setMode] = useState<'unlock' | 'setup'>('unlock');
  const [pass, setPass] = useState('');
  const [again, setAgain] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const run = (ok: boolean) => {
    setBusy(true);
    setError('');
    timer.current = window.setTimeout(
      () => {
        setBusy(false);
        if (ok) onUnlock();
        else setError('Wrong passphrase.');
      },
      reduced() ? 0 : 1400,
    );
  };
  const longEnough = pass.length >= 12;
  return (
    <div className="nc-unlock">
      <div className="nc-unlock-card">
        <div className="nc-lock-cells" aria-hidden="true">
          <Strip n={12} solid={busy ? 12 : 0} reserved={busy ? 0 : 0} size="lg" />
        </div>
        {mode === 'unlock' ? (
          <>
            <h1>Unlock your notes</h1>
            <p className="nc-lead">
              Your notes are encrypted in this browser. The key lives in memory while this page is
              open.
            </p>
            <label className="nc-field">
              <span>Passphrase</span>
              <input
                type="password"
                autoComplete="current-password"
                value={pass}
                disabled={busy}
                onChange={(e) => setPass(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && pass !== '' && run(pass.length >= 12)}
              />
            </label>
            {error && (
              <p className="nc-msg" data-tone="err">
                {error}
              </p>
            )}
            <div className="nc-actions">
              <Button primary disabled={busy || pass === ''} onClick={() => run(pass.length >= 12)}>
                {busy ? 'Unlocking…' : 'Unlock'}
              </Button>
              <Button disabled={busy} onClick={() => run(true)}>
                Unlock with passkey
              </Button>
            </div>
            <p className="nc-cap">
              The preview accepts any passphrase of 12 characters or more. First time here?{' '}
              <button type="button" className="nc-link" onClick={() => setMode('setup')}>
                Protect your notes
              </button>
            </p>
          </>
        ) : (
          <>
            <h1>Protect your notes</h1>
            <p className="nc-lead">
              The note secret is created in this browser and never leaves it. It is encrypted before
              it is stored, with a key that cannot be exported from the browser. Choose how to
              derive that key. It lives in memory while this page is open.
            </p>
            <label className="nc-field">
              <span>Passphrase (at least 12 characters)</span>
              <input
                type="password"
                autoComplete="new-password"
                value={pass}
                disabled={busy}
                onChange={(e) => setPass(e.target.value)}
              />
            </label>
            <div className="nc-pass" aria-hidden="true">
              <Strip n={12} solid={Math.min(pass.length, 12)} size="sm" />
              <small>{Math.min(pass.length, 12)} of 12 characters</small>
            </div>
            <label className="nc-field">
              <span>Repeat the passphrase</span>
              <input
                type="password"
                autoComplete="new-password"
                value={again}
                disabled={busy}
                onChange={(e) => setAgain(e.target.value)}
              />
            </label>
            {again !== '' && again !== pass && (
              <p className="nc-msg" data-tone="err">
                The two passphrases differ.
              </p>
            )}
            <p className="nc-cap">
              Argon2id, 64 MiB memory, 3 passes, 1 lane. If you lose the passphrase and the backup,
              the note cannot be recovered.
            </p>
            <div className="nc-actions">
              <Button
                primary
                disabled={busy || !longEnough || pass !== again}
                onClick={() => run(true)}
              >
                {busy ? 'Deriving the key…' : 'Use this passphrase'}
              </Button>
              <Button disabled={busy} onClick={() => run(true)}>
                Use a passkey
              </Button>
            </div>
            <p className="nc-cap">
              A passkey needs an authenticator that supports the PRF extension.{' '}
              <button type="button" className="nc-link" onClick={() => setMode('unlock')}>
                I already have a passphrase
              </button>
            </p>
          </>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Banners: a note close to its expiry. The notice levels are 14, 7 and 2 days.

function Banners({ notes, onClose }: { notes: Note[]; onClose: () => void }) {
  const near = notes.filter(
    (n) => n.status !== 'closed' && n.status !== 'exiting' && 30 - n.day <= 14,
  );
  return (
    <>
      {near.map((n) => {
        const left = 30 - n.day;
        const level = left <= 2 ? 2 : left <= 7 ? 7 : 14;
        return (
          <ExpiryBanner key={n.id} left={left} onOpen={onClose}>
            <strong>
              Note #{n.id} expires in {left} days
            </strong>{' '}
            ({level}-day notice). At 5 days this app stops opening leases on it and, while it is
            open, starts a close; at 3 days it starts an exit. Each waits for your wallet signature.
          </ExpiryBanner>
        );
      })}
    </>
  );
}

/** The expiry guard: its two settings and the days at which it acts. */
function GuardCard() {
  const [guard, setGuard] = useState(true);
  const [atEnd, setAtEnd] = useState(false);
  return (
    <Card
      title="Expiry guard"
      aside={<Chip tone={guard ? 'good' : 'warn'}>{guard ? 'on' : 'off'}</Chip>}
    >
      <p className="nc-cap nc-cap-first">
        A note that reaches its expiry goes to the treasury. While this app is open it shows notices
        at 14, 7 and 2 days, stops opening leases on the note 5 days before, starts a close at 5
        days and, if the close did not go through, an exit at 3 days. Each waits for your wallet
        signature.
      </p>
      <GuardTimeline off={!guard} />
      <label className="nc-check">
        <input type="checkbox" checked={guard} onChange={(e) => setGuard(e.target.checked)} />
        <span>
          Expiry guard
          <small>On by default.</small>
        </span>
      </label>
      <label className="nc-check">
        <input type="checkbox" checked={atEnd} onChange={(e) => setAtEnd(e.target.checked)} />
        <span>
          Start a close when a conversation ends
          <small>Off by default: every close shows the note ID and the total spend on chain.</small>
        </span>
      </label>
    </Card>
  );
}

// ---------------------------------------------------------------------------------------------
// The console

export function AppConsole({
  tab,
  onTab,
  onExit,
  theme,
  onTheme,
}: {
  tab: Tab;
  onTab: (tab: Tab) => void;
  onExit: () => void;
  theme: 'dark' | 'light';
  onTheme: () => void;
}) {
  const [notes, setNotes] = useState<Note[]>(() => START_NOTES.map((n) => ({ ...n })));
  const [active, setActive] = useState(0);
  const [drawer, setDrawer] = useState(false);
  const [notesOpen, setNotesOpen] = useState(false);
  const [privacy, setPrivacy] = useState(false);
  const [locked, setLocked] = useState(false);
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
  const [wallet, setWallet] = useState<Wallet | null>({ account: 'alice.testnet', name: 'Meteor' });
  const [chat, setChat] = useState<ChatState>({ ...FRESH_CHAT });
  const [backup, setBackup] = useState({ saved: true, when: '2 minutes ago' });
  const note = notes[active] ?? notes[0]!;

  const go = (next: Tab) => {
    setPrivacy(false);
    onTab(next);
    setDrawer(false);
    window.scrollTo({ top: 0, behavior: 'auto' });
  };
  const update = (id: number, patch: Partial<Note>) =>
    setNotes((all) => all.map((n) => (n.id === id ? { ...n, ...patch } : n)));
  const selectNote = (i: number) => {
    if (i === active) return;
    setActive(i);
    setChat((c) => ({ ...FRESH_CHAT, draft: c.draft }));
  };

  return (
    <div className="nc" data-drawer={drawer}>
      <aside className="nc-side" aria-label="Console">
        <div className="nc-brand-row">
          <a
            className="nc-brand"
            href="#home"
            aria-label="Back to the Nymira site"
            draggable={false}
            onClick={(e) => {
              e.preventDefault();
              onExit();
            }}
          >
            <Wordmark animated />
          </a>
          <span className="nc-network" title="vault.nymira.testnet">
            <i aria-hidden="true" />
            testnet
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
          <ul id="nc-note-list" role="listbox" aria-label="Choose a note">
            {notes.map((n, i) => (
              <li key={n.id} data-show={notesOpen || i === active}>
                <NoteRow n={n} selected={i === active} onPick={() => selectNote(i)} />
              </li>
            ))}
          </ul>
        </div>
        <nav className="nc-nav" aria-label="Screens">
          <span className="nc-group">Screens</span>
          {NAV.map(([id, label, icon]) => (
            <a
              key={id}
              href={`#${id}`}
              aria-current={tab === id ? 'page' : undefined}
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
          <button type="button" className="nc-lock" onClick={() => setLocked(true)}>
            <Icon name="lock" size={14} />
            Lock notes
          </button>
          <div className="nc-backup" data-saved={backup.saved}>
            <Strip n={2} solid={backup.saved ? 2 : 1} size="sm" />
            <span>
              <strong>{backup.saved ? 'Latest state saved' : 'Back up now'}</strong>
              <small>
                {backup.saved ? `${backup.when}, to 2 places` : 'A lease changed your note'}
              </small>
            </span>
            {!backup.saved && (
              <button type="button" onClick={() => setBackup({ saved: true, when: 'just now' })}>
                Save
              </button>
            )}
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
          <span className="nc-title">{TITLES[tab]}</span>
          <span className="nc-preview">preview · sample data</span>
          <span className="nc-net-spacer" />
          <IpChip />
          <ThemeToggle theme={theme} onTheme={onTheme} />
          <WalletMenu
            wallet={wallet}
            wallets={WALLET_ITEMS}
            delay={1100}
            onConnect={(name) => setWallet({ account: 'alice.testnet', name })}
            onDisconnect={() => setWallet(null)}
          />
        </header>
        {!ipSeen && !locked && (
          <p className="nc-ip" role="status">
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
        {!locked && <Banners notes={notes} onClose={() => go('close')} />}
        <main
          id="main-content"
          className="nc-content"
          key={locked ? 'locked' : privacy ? 'privacy' : tab}
        >
          {locked ? (
            <Unlock onUnlock={() => setLocked(false)} />
          ) : privacy ? (
            <PrivacyPanel note={note} onBack={() => setPrivacy(false)} />
          ) : (
            <>
              {tab === 'chat' && (
                <Chat
                  note={note}
                  onFund={() => go('fund')}
                  onNotes={() => go('notes')}
                  wallet={wallet}
                  chat={chat}
                  setChat={(u) => setChat(u)}
                  onPrivacy={() => setPrivacy(true)}
                  backupStale={!backup.saved}
                  onSettle={(charge) => {
                    update(note.id, { balance: Math.round((note.balance - charge) * 1000) / 1000 });
                    setBackup((b) => ({ ...b, saved: false }));
                  }}
                />
              )}
              {tab === 'fund' && (
                <Fund
                  wallet={wallet}
                  onConnect={(name) => setWallet({ account: 'alice.testnet', name })}
                  nextId={Math.max(...notes.map((n) => n.id)) + 1}
                  onFunded={(size) => {
                    const id = Math.max(...notes.map((n) => n.id)) + 1;
                    setNotes((all) => [
                      { id, size, balance: size, status: 'open', day: 0 },
                      ...all,
                    ]);
                    setActive(0);
                    setChat((c) => ({ ...FRESH_CHAT, draft: c.draft }));
                    setBackup({ saved: false, when: '' });
                  }}
                  onOpenChat={() => go('chat')}
                  onNotes={() => go('notes')}
                />
              )}
              {tab === 'notes' && (
                <Notes
                  notes={notes}
                  active={active}
                  onPick={selectNote}
                  onPrivacy={() => setPrivacy(true)}
                  go={go}
                  backup={backup}
                  onBackup={() => setBackup({ saved: true, when: 'just now' })}
                />
              )}
              {tab === 'close' && (
                <Close
                  note={note}
                  onClose={(id) => update(id, { status: 'closed', balance: 0 })}
                  onExit={(id) => update(id, { status: 'exiting' })}
                />
              )}
              {tab === 'agents' && <Agents wallet={wallet} />}
            </>
          )}
        </main>
      </div>
      {drawer && <div className="nc-scrim" onClick={() => setDrawer(false)} aria-hidden="true" />}
    </div>
  );
}
