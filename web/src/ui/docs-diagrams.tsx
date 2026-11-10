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

/** True once the figure has been on screen, and whether the reader asked for less motion. */
function useSeen(ref: React.RefObject<HTMLElement | null>) {
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
      { threshold: 0.3 },
    );
    watch.observe(el);
    return () => watch.disconnect();
  }, [ref]);
  return seen;
}
const calm = () =>
  typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

type Key = 'you' | 'op' | 'ai' | 'vault';
const WHO: Record<
  Key,
  { name: string; role: string; has: string; sees: string[]; never: string[] }
> = {
  you: {
    name: 'You',
    role: 'Hold the note secret',
    has: 'Holds',
    sees: ['the note secret', 'your prompts and answers', 'the proofs, made on your device'],
    never: [],
  },
  op: {
    name: 'Operator',
    role: 'Checks proofs, opens leases',
    has: 'Sees',
    sees: ['lease proof and nullifier', 'bound class and model', 'sizes, timing, usage, charge'],
    never: ['your prompt text'],
  },
  ai: {
    name: 'NEAR AI (TEE)',
    role: 'Runs the model',
    has: 'Sees',
    sees: ["your prompt, as the model's enclave", 'token counts and timing'],
    never: ['which note it came from'],
  },
  vault: {
    name: 'Vault (NEAR)',
    role: 'Holds the USDC',
    has: 'Sees',
    sees: ['each deposit', 'on a close: note ID, total spend, payout account'],
    never: ['prompts', 'single leases'],
  },
};

/** Who talks to whom, and what each of them can read. Hover or focus a party to isolate it. */
export function Parties() {
  const ref = useRef<HTMLElement>(null);
  const seen = useSeen(ref);
  const [focus, setFocus] = useState<Key | null>(null);
  const card = (k: Key, area: string) => {
    const p = WHO[k];
    return (
      <div
        className="nd-who-card"
        data-k={k}
        data-a={area}
        data-dim={focus !== null && focus !== k}
        tabIndex={0}
        onMouseEnter={() => setFocus(k)}
        onMouseLeave={() => setFocus(null)}
        onFocus={() => setFocus(k)}
        onBlur={() => setFocus(null)}
      >
        <h4>{p.name}</h4>
        <p className="nd-who-role">{p.role}</p>
        <b>{p.has}</b>
        <ul>
          {p.sees.map((t) => (
            <li key={t}>{t}</li>
          ))}
        </ul>
        {p.never.length > 0 && (
          <>
            <b>Never sees</b>
            <ul data-never="true">
              {p.never.map((t) => (
                <li key={t}>{t}</li>
              ))}
            </ul>
          </>
        )}
      </div>
    );
  };
  return (
    <figure className="nd-who" ref={ref} data-seen={seen}>
      <div className="nd-who-grid">
        {card('you', 'you')}
        <div className="nd-who-link" data-a="l1" style={vars({ '--d': '0s' })} aria-hidden="true">
          <span>lease proof, sealed prompt</span>
          <i />
        </div>
        {card('op', 'op')}
        <div className="nd-who-link" data-a="l2" style={vars({ '--d': '1.6s' })} aria-hidden="true">
          <span>sealed prompt</span>
          <i />
        </div>
        {card('ai', 'ai')}
        <div className="nd-who-drop" data-a="d1" style={vars({ '--d': '3.4s' })} aria-hidden="true">
          <span>deposit, close, exit: from your wallet, straight to the vault</span>
          <i />
        </div>
        <div className="nd-who-drop" data-a="d2" style={vars({ '--d': '4.4s' })} aria-hidden="true">
          <span>sync_root, heartbeat</span>
          <i />
        </div>
        {card('vault', 'vault')}
      </div>
      <figcaption>
        Hover a party to read only its side. The web app cannot use Tor, so the operator&apos;s
        entry relay and the RPC provider also see your IP address.
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

const STEPS: { lane: 0 | 1 | 2; verb: string; text: string }[] = [
  { lane: 0, verb: 'Prove', text: 'The note can pay, without saying which note.' },
  { lane: 0, verb: 'Seal', text: "The prompt is encrypted to the model's attested key." },
  { lane: 1, verb: 'Check', text: 'Proof and nullifier are valid; a lease opens.' },
  { lane: 2, verb: 'Answer', text: 'The model answers inside its enclave and signs.' },
  {
    lane: 0,
    verb: 'Verify',
    text: "Your client checks the model's signature, and says so only if it holds.",
  },
  { lane: 1, verb: 'Settle', text: 'The charge is taken; the rest of the lease is free again.' },
];
const LANES = ['You', 'Operator', 'NEAR AI'];
const LANE_H = 64;
const nodeX = (i: number) => (i / (STEPS.length - 1)) * 100;
const nodeY = (i: number) => (STEPS[i]?.lane ?? 0) * LANE_H + LANE_H / 2;

/**
 * One request, start to finish, as a sequence across three lanes. A packet walks the six steps;
 * a click on a node or a step stops the walk and shows that step.
 */
export function Lease() {
  const ref = useRef<HTMLElement>(null);
  const seen = useSeen(ref);
  const [now, setNow] = useState(0);
  const [auto, setAuto] = useState(() => !calm());
  useEffect(() => {
    if (!seen || !auto) return;
    const tick = setInterval(() => setNow((v) => (v + 1) % STEPS.length), 2600);
    return () => clearInterval(tick);
  }, [seen, auto]);
  const pick = (i: number) => {
    setAuto(false);
    setNow(i);
  };
  return (
    <figure className="nd-seq" ref={ref} data-seen={seen}>
      <div className="nd-seq-bar">
        <span>One request, start to finish</span>
        <button type="button" onClick={() => setAuto((a) => !a)} aria-pressed={auto}>
          {auto ? 'Pause' : 'Play'}
        </button>
      </div>
      <div className="nd-seq-stage" style={vars({ '--h': `${LANE_H * 3}px` })}>
        {LANES.map((name, l) => (
          <div key={name} className="nd-seq-lane" style={vars({ '--l': l })}>
            <span>{name}</span>
          </div>
        ))}
        <svg
          className="nd-seq-wires"
          viewBox={`0 0 100 ${LANE_H * 3}`}
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          {STEPS.slice(0, -1).map((_, j) => (
            <line
              key={j}
              x1={nodeX(j)}
              y1={nodeY(j)}
              x2={nodeX(j + 1)}
              y2={nodeY(j + 1)}
              vectorEffect="non-scaling-stroke"
              data-s={j < now ? 'done' : 'todo'}
            />
          ))}
        </svg>
        <div className="nd-seq-nodes">
          {STEPS.map((s, i) => (
            <button
              key={s.verb}
              type="button"
              className="nd-seq-node"
              data-s={i < now ? 'done' : i === now ? 'now' : 'todo'}
              style={vars({ '--x': `${nodeX(i)}%`, '--y': `${nodeY(i)}px` })}
              aria-label={`Step ${i + 1}: ${s.verb}`}
              aria-current={i === now ? 'step' : undefined}
              onClick={() => pick(i)}
            >
              {i + 1}
            </button>
          ))}
          <i
            className="nd-seq-packet"
            aria-hidden="true"
            style={vars({ '--x': `${nodeX(now)}%`, '--y': `${nodeY(now)}px` })}
          />
        </div>
      </div>
      <ol className="nd-seq-steps">
        {STEPS.map((s, i) => (
          <li key={s.verb} data-s={i === now ? 'now' : i < now ? 'done' : 'todo'}>
            <button type="button" onClick={() => pick(i)}>
              <b>{i + 1}</b>
              <span>
                <strong>{s.verb}</strong>
                {s.text}
              </span>
            </button>
          </li>
        ))}
      </ol>
      <figcaption>
        The operator never holds the prompt text, and your wallet is not involved until you close or
        exit.
      </figcaption>
    </figure>
  );
}
