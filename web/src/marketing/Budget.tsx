import { useEffect, useRef, useState, type CSSProperties } from 'react';
import { Cells } from './Board';

// The checks follow gateway.rs `preflight` and the README: token, signed policy, the note,
// model, rate, call cost, the day, then the owner's approval, then the balance. Codes are the
// README's agent API table. The numbers belong to the example policy shown above this block.
type Step = { name: string; detail: string; code: string };

const STEPS: readonly Step[] = [
  {
    name: 'Token on',
    detail: 'agent disable takes effect on the next request.',
    code: '401 token_disabled',
  },
  {
    name: 'Signed policy',
    detail: 'Unsigned, altered, stale or for another note counts as none.',
    code: '402 policy_missing',
  },
  {
    name: 'Note healthy',
    detail: 'Not revoked, closed or in its last days. No charge warning.',
    code: 'refused',
  },
  {
    name: 'Model on the list',
    detail: 'Checked before anything leaves your machine.',
    code: '402 model_not_allowed',
  },
  { name: 'Rate', detail: 'At most 10 requests in a minute.', code: '429 policy_rate_limit' },
  { name: 'Call cost', detail: 'The worst case stays within 0.10 USD.', code: '402 call_limit' },
  {
    name: 'The day',
    detail: 'Settled, reserved and in flight stay within 1.00 USD.',
    code: '402 daily_limit',
  },
  {
    name: 'Your approval',
    detail: 'Only for a worst case above 0.05 USD.',
    code: '402 approval_required',
  },
  {
    name: 'Note balance',
    detail: 'It has to cover a lease of the agent’s class.',
    code: 'refused',
  },
];

type Scenario = {
  label: string;
  stop: number | null;
  verdict: string;
  text: string;
};

const SCENARIOS: readonly Scenario[] = [
  {
    label: '0.03 USD call',
    stop: null,
    verdict: 'Allowed',
    text: 'A lease opens and the call goes out encrypted. The agent never waits on you.',
  },
  {
    label: '0.08 USD call',
    stop: 7,
    verdict: 'Held for you',
    text: 'The agent gets approval_required and an approval ID. You approve it, and the agent sends the same request again with that ID.',
  },
  {
    label: '0.14 USD call',
    stop: 5,
    verdict: 'Refused',
    text: 'The request asks for more than the 0.10 USD ceiling. Nothing reached the operator.',
  },
  {
    label: 'Another model',
    stop: 3,
    verdict: 'Refused',
    text: 'The model is not in allowed_models, also on a lease that is already open. Nothing reached the operator.',
  },
  {
    label: 'Day already spent',
    stop: 6,
    verdict: 'Refused',
    text: 'Today’s settled charges, the open lease and calls in flight already reach 1.00 USD. The next UTC day starts fresh.',
  },
];

const ROUTES = [
  ['POST', '/v1/chat/completions'],
  ['GET', '/v1/models'],
  ['GET', '/v1/nymira/balance'],
  ['POST', '/v1/nymira/quote'],
] as const;

function Mark({ state }: { state: 'pass' | 'stop' | 'idle' }) {
  return (
    <svg width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
      <circle cx="9" cy="9" r="8" stroke="currentColor" />
      {state === 'pass' && <path d="M5.5 9.2l2.4 2.4 4.6-5" stroke="currentColor" />}
      {state === 'stop' && <path d="M6 6l6 6M12 6l-6 6" stroke="currentColor" />}
    </svg>
  );
}

export function Ladder() {
  const [pick, setPick] = useState(1);
  const scenario = SCENARIOS[pick] ?? SCENARIOS[0]!;
  const last = scenario.stop ?? STEPS.length - 1;
  return (
    <div className="nm-ladder">
      <div className="nm-demo-bar">
        <span>A request walks the checks</span>
        <span>Order from nymirad</span>
      </div>
      <div className="nm-ladder-body">
        <div className="nm-ladder-pick" role="group" aria-label="Choose an example request">
          {SCENARIOS.map((s, i) => (
            <button
              key={s.label}
              type="button"
              aria-pressed={i === pick}
              onClick={() => setPick(i)}
            >
              {s.label}
            </button>
          ))}
        </div>
        <ol className="nm-ladder-steps" key={pick}>
          {STEPS.map((step, i) => {
            const state =
              i < last || (scenario.stop === null && i === last)
                ? 'pass'
                : i === last
                  ? 'stop'
                  : 'idle';
            return (
              <li key={step.name} data-state={state} style={{ '--i': i } as CSSProperties}>
                <Mark state={state} />
                <div>
                  <h4>{step.name}</h4>
                  <p>{step.detail}</p>
                </div>
                <code>{state === 'stop' ? step.code : ''}</code>
              </li>
            );
          })}
        </ol>
        <div
          className="nm-ladder-verdict"
          data-kind={scenario.stop === null ? 'allow' : scenario.stop === 7 ? 'hold' : 'deny'}
          aria-live="polite"
        >
          <strong>{scenario.verdict}</strong>
          <p>{scenario.text}</p>
        </div>
      </div>
    </div>
  );
}

// One UTC day on the note, replayed. Each cell is 0.05 USD. The figures are an example; the rules
// are the README's: the day counts settled charges, the bound of an open lease and the worst case
// of calls in flight, and settling replaces a reservation with the charge.
type Moment = { settled: number; reserved: number; refused?: boolean; text: string };

const DAY: readonly Moment[] = [
  { settled: 0.3, reserved: 0, text: 'So far today, 0.30 USD is settled.' },
  { settled: 0.3, reserved: 0.1, text: 'A lease opens and reserves its bound: 0.10 USD.' },
  {
    settled: 0.3,
    reserved: 0.2,
    text: 'A call goes out. Its worst case, 0.10 USD, counts as well.',
  },
  {
    settled: 0.314,
    reserved: 0,
    text: 'The lease settles at 0.014 USD. The reservation is replaced by the charge.',
  },
  { settled: 0.75, reserved: 0, text: 'Hours later, 0.75 USD is settled.' },
  { settled: 0.75, reserved: 0.2, text: 'Another lease and a call in flight: 0.20 USD reserved.' },
  {
    settled: 0.75,
    reserved: 0.2,
    refused: true,
    text: 'A call with a worst case of 0.08 USD would reach 1.03. It gets 402 daily_limit.',
  },
  { settled: 0, reserved: 0, text: 'At 00:00 UTC the day starts again.' },
];

const CELLS = 20;
const STEP = 0.05;
const usd = (n: number) => n.toFixed(2);

export function cellOf(
  i: number,
  m: Moment,
): { kind: 'fill' | 'part' | 'lease' | 'free'; part: number } {
  const lo = i * STEP;
  const settledIn = Math.min(Math.max((m.settled - lo) / STEP, 0), 1);
  if (settledIn >= 0.999) return { kind: 'fill', part: 1 };
  if (settledIn > 0) return { kind: 'part', part: settledIn };
  const reservedIn = Math.min(Math.max((m.settled + m.reserved - lo) / STEP, 0), 1);
  return reservedIn > 0.001 ? { kind: 'lease', part: 1 } : { kind: 'free', part: 0 };
}

export function DayGraph() {
  const reduced =
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  const [step, setStep] = useState(reduced ? 2 : 0);
  const [playing, setPlaying] = useState(!reduced);
  const [visible, setVisible] = useState(false);
  const holder = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = holder.current;
    if (!element) return;
    const observer = new IntersectionObserver(([entry]) =>
      setVisible(entry?.isIntersecting ?? false),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!playing || !visible || reduced) return;
    const timer = window.setInterval(() => setStep((n) => (n + 1) % DAY.length), 2000);
    return () => window.clearInterval(timer);
  }, [playing, visible, reduced]);

  const m = DAY[step] ?? DAY[0]!;
  const free = Math.max(1 - m.settled - m.reserved, 0);
  return (
    <div className="nm-day" ref={holder}>
      <div className="nm-demo-bar">
        <span>One UTC day on the note</span>
        <span>
          Example, 1.00 USD limit
          {!reduced && (
            <button type="button" className="nm-day-toggle" onClick={() => setPlaying(!playing)}>
              {playing ? 'Pause' : 'Play'}
            </button>
          )}
        </span>
      </div>
      <div className="nm-day-body">
        <div
          className="nm-cells nm-day-cells"
          style={{ '--n': CELLS, '--drain': 0 } as CSSProperties}
          aria-hidden="true"
        >
          {Array.from({ length: CELLS }, (_, i) => {
            const c = cellOf(i, m);
            const last = i === CELLS - 1;
            return (
              <span
                key={i}
                className={`${c.kind === 'fill' ? 'nm-cell-fill' : c.kind === 'lease' ? 'nm-cell-lease' : c.kind === 'part' ? 'nm-cell-part' : ''} ${m.refused && last ? 'nm-day-refused' : ''}`}
                style={{ '--p': c.part, transitionDelay: `${i * 18}ms` } as CSSProperties}
              />
            );
          })}
        </div>
        <div className="nm-day-axis" aria-hidden="true">
          <span>0.00</span>
          <span>0.50</span>
          <span>1.00 USD</span>
        </div>
        <p className="nm-day-log" aria-live="polite" data-refused={m.refused ? 'true' : 'false'}>
          <span>{String(step + 1).padStart(2, '0')}</span>
          {m.text}
        </p>
        <ul className="nm-day-key">
          <li>
            <i data-k="fill" />
            <span>Settled</span>
            <code>{usd(m.settled)}</code>
            <p>Leases that closed today, at their charge.</p>
          </li>
          <li>
            <i data-k="lease" />
            <span>Reserved</span>
            <code>{usd(m.reserved)}</code>
            <p>The bound of the open lease, plus the worst case of a call in flight.</p>
          </li>
          <li>
            <i data-k="free" />
            <span>Free</span>
            <code>{usd(free)}</code>
            <p>A new call counts its worst case against this.</p>
          </li>
        </ul>
        <p className="nm-day-foot">
          The settled part sits in <code>counters-NAME.json</code> in the state directory, and a
          restart or a restore never lowers it below what the lease records show.
        </p>
      </div>
    </div>
  );
}

export function Reach() {
  return (
    <div className="nm-reach">
      <div className="nm-demo-bar">
        <span>What an agent token reaches</span>
        <span>Nothing else</span>
      </div>
      <ul>
        {ROUTES.map(([method, path]) => (
          <li key={path}>
            <span>{method}</span>
            <code>{path}</code>
          </li>
        ))}
      </ul>
      <p>
        One token, one note, one transport: a unix token works on the agent socket, a tcp token on
        loopback port 8788. Your owner token is not accepted on the agent socket.
      </p>
    </div>
  );
}
