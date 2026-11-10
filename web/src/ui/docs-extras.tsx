import { useMemo, useState, type ReactNode } from 'react';
import { PROFILE } from '../profile';
import { checkVaultOnly, runChecks, type Check, type Level, type Report } from '../status/live';
import { vars } from '../marketing/kit';
import { rpc } from './services';
import type { Section } from './docs-content';

const WORD: Record<Level, string> = { ok: 'OK', warn: 'NOTE', fail: 'FAIL' };

/** A level mark in the console's cell style. */
const Dot = ({ level }: { level: Level | 'idle' }) => (
  <i className="nd-dot" data-level={level} aria-hidden="true" />
);

/** The vault, compared with what this app pins, when the reader presses the button. */
export function VaultCompare() {
  const [check, setCheck] = useState<Check | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      setCheck(await checkVaultOnly(PROFILE, rpc));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="nd-live" data-level={check?.level ?? 'idle'} data-testid="docs-vault-compare">
      <div className="nd-live-head">
        <Dot level={check?.level ?? 'idle'} />
        <strong>
          {check ? `${WORD[check.level]}: the vault on chain` : 'Compare the vault on chain'}
        </strong>
        <button type="button" onClick={() => void run()} disabled={busy}>
          {busy ? 'Reading…' : check ? 'Check again' : 'Compare now'}
        </button>
      </div>
      <p>
        {check
          ? check.summary
          : `Reads ${PROFILE.vaultAccount} from the RPC and compares its code hash, verifying keys, config and access keys with the pins in this app. The read shows your address to the RPC provider.`}
      </p>
      {check && check.facts.length > 0 && (
        <dl>
          {check.facts.map(([k, v], i) => (
            <div key={`${k}${i}`}>
              <dt>{k}</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
      )}
    </div>
  );
}

/** A small live summary in the right column. Nothing is read until the reader asks. */
export function LivePanel({ onStatus }: { onStatus: () => void }) {
  const [report, setReport] = useState<Report | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      setReport(await runChecks(PROFILE, rpc));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="nd-aside-block nd-livepanel">
      <span className="nd-aside-title">Live</span>
      {report ? (
        <ul>
          {report.checks.map((c) => (
            <li key={c.id} title={c.summary}>
              <Dot level={c.level} />
              {c.title}
              <b data-level={c.level}>{WORD[c.level]}</b>
            </li>
          ))}
        </ul>
      ) : (
        <p>Operator, vault, RPC and release. Read only when you ask.</p>
      )}
      <div className="nd-livepanel-actions">
        <button type="button" onClick={() => void run()} disabled={busy}>
          {busy ? 'Checking…' : report ? 'Check again' : 'Check live'}
        </button>
        <button type="button" onClick={onStatus}>
          Open status
        </button>
      </div>
    </div>
  );
}

const DAY = 86_400_000;
const fmt = (ms: number) =>
  new Date(ms).toLocaleDateString('en-GB', {
    weekday: 'short',
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    timeZone: 'UTC',
  });

const todayUtc = () => new Date().toISOString().slice(0, 10);

/**
 * The dates of one note, from its deposit day. A note expires at the end of the UTC day 30 days
 * after the deposit; the expiry guard and the treasury follow the numbers the vault and the app use.
 */
export function NoteCalendar() {
  const [day, setDay] = useState(todayUtc());
  const dates = useMemo(() => {
    const start = Date.parse(`${day}T00:00:00Z`);
    if (!Number.isFinite(start)) return null;
    const expiryEnd = start + 31 * DAY; // the end of day 30 is the start of day 31
    const lastDay = start + 30 * DAY;
    const before = (n: number) => expiryEnd - n * DAY;
    return [
      ['Deposit', start, 'The note opens.'],
      ['Notice, 14 days left', before(14), 'The app starts to warn you.'],
      ['Notice, 7 days left', before(7), 'A second warning.'],
      ['Close attempt, 5 days left', before(5), 'No new lease; the app tries a close.'],
      ['Exit, 3 days left', before(3), 'If the close failed, the app files an exit.'],
      ['Notice, 2 days left', before(2), 'The last warning.'],
      ['Expires', lastDay, 'The end of this UTC day. It can still close or exit.'],
      [
        'The treasury can take it',
        expiryEnd + 7 * DAY,
        'Seven days after the expiry, and never while an exit is pending.',
      ],
    ] as [string, number, string][];
  }, [day]);
  const now = Date.now();
  return (
    <div className="nd-cal" data-testid="docs-calendar">
      <label>
        <span>Deposit day (UTC)</span>
        <input type="date" value={day} onChange={(e) => setDay(e.target.value)} />
      </label>
      {dates ? (
        <ol>
          {dates.map(([name, at, note]) => (
            <li key={name} data-past={at < now} data-edge={name === 'Expires'}>
              <i aria-hidden="true" />
              <time dateTime={new Date(at).toISOString()}>{fmt(at)}</time>
              <b>{name}</b>
              <span>{note}</span>
            </li>
          ))}
        </ol>
      ) : (
        <p>Pick a day.</p>
      )}
    </div>
  );
}

type Mode = 'app' | 'cli';
const IP_ROWS: [string, Record<Mode, string>][] = [
  [
    "The operator's entry relay",
    {
      app: 'Your address and the time of every connection.',
      cli: 'A Tor exit, not your address.',
    },
  ],
  [
    'The RPC provider',
    {
      app: 'Your address on every read and send.',
      cli: 'A Tor exit, each send on its own circuit. If Tor is down, `nymirad` asks you before it sends directly.',
    },
  ],
  [
    'NEAR AI',
    {
      app: 'Your address, when the app asks it for an attestation report.',
      cli: 'Prompts go through the operator, so it sees the operator, not you.',
    },
  ],
  [
    'The web host',
    {
      app: 'Your address and the files you ask for.',
      cli: 'Not involved. You get the binary from GitHub, which sees that download.',
    },
  ],
];

/** Who can see the reader's IP address, for the web app and for `nymirad`. */
export function IpMatrix({ rich }: { rich: (n: ReactNode) => ReactNode }) {
  const [mode, setMode] = useState<Mode>('app');
  return (
    <div className="nd-ip" data-mode={mode} data-testid="docs-ip">
      <div className="nd-ip-bar" role="group" aria-label="How you connect">
        <span>When you use</span>
        {(
          [
            ['app', 'The web app'],
            ['cli', 'nymirad'],
          ] as [Mode, string][]
        ).map(([id, label]) => (
          <button key={id} type="button" aria-pressed={mode === id} onClick={() => setMode(id)}>
            {label}
          </button>
        ))}
      </div>
      <ul>
        {IP_ROWS.map(([who, by], i) => (
          <li key={who} style={vars({ '--n': i })}>
            <b>{who}</b>
            <span>{rich(by[mode])}</span>
          </li>
        ))}
      </ul>
      <p>
        {rich(
          'Timing and usage can still link requests, whatever hides the address. The web app cannot use Tor; Tor Browser or `nymirad` can.',
        )}
      </p>
    </div>
  );
}

/** A section as Markdown, for a reader or a tool that wants plain text. */
export function markdownOf(s: Section): string {
  const text = (n: ReactNode) => (typeof n === 'string' ? n : '');
  const out: string[] = [`## ${s.title}`, ''];
  if (s.lead) out.push(text(s.lead), '');
  for (const b of s.blocks) {
    switch (b.t) {
      case 'p':
        out.push(text(b.text), '');
        break;
      case 'h3':
        out.push(`### ${text(b.text)}`, '');
        break;
      case 'list':
        out.push(...b.items.map((i) => `- ${text(i)}`), '');
        break;
      case 'steps':
        out.push(...b.items.map(([t, x], i) => `${i + 1}. **${t}.** ${text(x)}`), '');
        break;
      case 'table':
        out.push(
          `| ${b.head.join(' | ')} |`,
          `| ${b.head.map(() => '---').join(' | ')} |`,
          ...b.rows.map((r) => `| ${r.map((c) => text(c).replace(/\|/g, '\\|')).join(' | ')} |`),
          '',
        );
        break;
      case 'code':
        out.push('```', b.text, '```', '');
        break;
      case 'note':
        out.push(`> **${b.title}.** ${text(b.text)}`, '');
        break;
      case 'faq':
        out.push(...b.items.flatMap(([q, a]) => [`**${q}**`, text(a), '']));
        break;
      case 'compare':
        out.push(
          ...b.items.flatMap((i) => [
            `**${i.name}**`,
            ...i.rows.map(([k, v]) => `- ${k}: ${v}`),
            '',
          ]),
        );
        break;
      default:
    }
  }
  return (
    out
      .join('\n')
      .replace(/\n{3,}/g, '\n\n')
      .trim() + '\n'
  );
}

export function RawView({ section, onClose }: { section: Section; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  const md = useMemo(() => markdownOf(section), [section]);
  return (
    <div
      className="nd-search"
      role="dialog"
      aria-modal="true"
      aria-label={`${section.title} as Markdown`}
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
      onKeyDown={(e) => e.key === 'Escape' && onClose()}
    >
      <div className="nd-search-box nd-raw">
        <div className="nd-raw-bar">
          <span>{section.title} · Markdown</span>
          <button
            type="button"
            onClick={() =>
              void navigator.clipboard
                ?.writeText(md)
                .then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1400);
                })
                .catch(() => undefined)
            }
          >
            {copied ? 'Copied' : 'Copy'}
          </button>
          <button type="button" onClick={onClose} autoFocus>
            Close
          </button>
        </div>
        <pre tabIndex={0}>{md}</pre>
      </div>
    </div>
  );
}
