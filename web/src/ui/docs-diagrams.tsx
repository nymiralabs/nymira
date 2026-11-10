import { useEffect, useRef, useState } from 'react';
import { vars } from '../marketing/kit';

// The 30 days of a note, then the 7 days the treasury waits. The marks are the numbers the vault
// and the expiry guard use (`note_ttl` 30 days, `exit_delay_secs` 7 days, guard at 5 and 3 days).
const DAYS = 30;
const TAIL = 7;
const MARKS: [number, string][] = [
  [0, 'Deposit'],
  [30, 'Expires'],
  [36, 'Treasury can take it'],
];
const LEGEND: [string, string][] = [
  ['open', 'Open: spend it, or close it'],
  ['close', 'From day 25 (5 days left): no new lease, a close is tried'],
  ['exit', 'From day 27 (3 days left): an exit if the close failed'],
  ['tail', '7 days after expiry: the treasury can take it, unless an exit is pending'],
];

function zone(day: number): string {
  if (day >= DAYS) return 'tail';
  if (day >= 27) return 'exit';
  if (day >= 25) return 'close';
  return 'open';
}

export function NoteLife() {
  const ref = useRef<HTMLDivElement>(null);
  const [seen, setSeen] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof IntersectionObserver === 'undefined') {
      setSeen(true);
      return;
    }
    const watch = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          setSeen(true);
          watch.disconnect();
        }
      },
      { threshold: 0.4 },
    );
    watch.observe(el);
    return () => watch.disconnect();
  }, []);
  const total = DAYS + TAIL;
  return (
    <figure className="nd-life" ref={ref} data-seen={seen}>
      <div
        className="nd-life-cells"
        role="img"
        aria-label="A note's 30 days, then 7 more days before the treasury can take it. The expiry guard tries a close at 5 days left and an exit at 3 days left."
      >
        {Array.from({ length: total }, (_, day) => (
          <i key={day} data-zone={zone(day)} style={vars({ '--n': day })} />
        ))}
      </div>
      <div className="nd-life-marks" aria-hidden="true">
        {MARKS.map(([day, label]) => (
          <span
            key={day}
            style={vars({ '--at': day / (total - 1) })}
            data-edge={day === 0 ? 'start' : 'end'}
          >
            <b>day {day === total - 1 ? total : day}</b>
            {label}
          </span>
        ))}
      </div>
      <ul className="nd-life-legend">
        {LEGEND.map(([name, text]) => (
          <li key={name}>
            <i data-zone={name} aria-hidden="true" />
            {text}
          </li>
        ))}
      </ul>
      <figcaption>
        Each cell is a day. The marks are the numbers the vault and the app use.
      </figcaption>
    </figure>
  );
}

/** Who talks to whom. The arrows run; the boxes list what each party sees. */
export function Parties() {
  const box = (key: string, title: string, lines: string[]) => (
    <div className="nd-p-box" data-k={key}>
      <h4>{title}</h4>
      <ul>
        {lines.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
    </div>
  );
  const link = (text: string, area: string) => (
    <div className="nd-p-link" data-a={area} aria-hidden="true">
      <span>{text}</span>
      <i />
    </div>
  );
  return (
    <figure className="nd-parties">
      <div className="nd-p-grid">
        {box('you', 'You', [
          'note secret',
          'prompts and answers',
          'proofs are made here',
          'web app or nymirad',
        ])}
        {link('lease proof, encrypted prompt', 'l1')}
        {box('op', 'Operator', [
          'lease proof, nullifier',
          'bound class, model',
          'sizes, timing, usage, charge',
          'not your prompt text',
        ])}
        {link('encrypted prompt', 'l2')}
        {box('ai', 'NEAR AI (TEE)', [
          "your prompt, as the model's enclave",
          'token counts, timing',
          'not which note',
        ])}
        <div className="nd-p-wallet" aria-hidden="true">
          <span>deposit, close, exit: from your wallet, straight to the vault</span>
          <i />
        </div>
        <div className="nd-p-down" aria-hidden="true">
          <span>sync_root, heartbeat</span>
          <i />
        </div>
        {box('vault', 'Vault (NEAR)', [
          'holds the USDC',
          'the deposit',
          'on a close: note ID, total spend,',
          'payout account',
        ])}
      </div>
      <figcaption>
        Boxes list what each party sees. The web app cannot use Tor, so the operator&apos;s entry
        relay and the RPC provider also see your IP address.
      </figcaption>
    </figure>
  );
}

/** The seven days between `request_exit` and `finalize_exit`. */
export function ExitWindow() {
  const ref = useRef<HTMLDivElement>(null);
  const [seen, setSeen] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof IntersectionObserver === 'undefined') {
      setSeen(true);
      return;
    }
    const watch = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          setSeen(true);
          watch.disconnect();
        }
      },
      { threshold: 0.5 },
    );
    watch.observe(el);
    return () => watch.disconnect();
  }, []);
  return (
    <figure className="nd-exit" ref={ref} data-seen={seen}>
      <div
        className="nd-exit-row"
        role="img"
        aria-label="Day 0, request_exit. Days 1 to 6, anyone can answer with a later state. Day 7, finalize_exit pays the payout account."
      >
        <div className="nd-exit-seg" data-k="start">
          <div className="nd-exit-cells">
            <i style={vars({ '--n': 0 })} />
          </div>
          <p>
            <b>day 0</b>
            <code>request_exit</code>
            proof of your latest state
          </p>
        </div>
        <div className="nd-exit-seg" data-k="wait">
          <div className="nd-exit-cells">
            {Array.from({ length: 6 }, (_, i) => (
              <i key={i} style={vars({ '--n': i + 1 })} />
            ))}
          </div>
          <p>
            <b>days 1 to 6</b>
            anyone can answer with a later state (<code>defeat_exit</code>) or raise the concession
          </p>
        </div>
        <div className="nd-exit-seg" data-k="end">
          <div className="nd-exit-cells">
            <i style={vars({ '--n': 7 })} />
          </div>
          <p>
            <b>day 7</b>
            <code>finalize_exit</code>
            pays the payout account
          </p>
        </div>
      </div>
    </figure>
  );
}
