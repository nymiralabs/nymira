import { useEffect, useRef, useState, type CSSProperties, type RefObject } from 'react';
import { cellOf } from './Budget';

// Small animated scenes in the cell style of the budget graphics. Every scene plays only while it
// is on screen and the tab is visible, has a pause control where it loops, and holds a still frame
// under prefers-reduced-motion. All amounts are examples; the rules are the README's.
export const reducedMotion = () =>
  typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Counts 0..n-1 on a timer while `ref` is on screen and `playing`. */
export function useStepper(
  ref: RefObject<HTMLElement | null>,
  n: number,
  ms: number,
  playing = true,
) {
  const reduced = reducedMotion();
  const [step, setStep] = useState(reduced ? n - 1 : 0);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new IntersectionObserver(([e]) => setVisible(e?.isIntersecting ?? false));
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);
  useEffect(() => {
    if (!playing || !visible || reduced) return;
    const timer = window.setInterval(() => {
      if (!document.hidden) setStep((s) => (s + 1) % n);
    }, ms);
    return () => window.clearInterval(timer);
  }, [playing, visible, reduced, n, ms]);
  return step;
}

function Pause({ playing, onToggle }: { playing: boolean; onToggle: () => void }) {
  if (reducedMotion()) return null;
  return (
    <button type="button" className="nm-day-toggle" onClick={onToggle}>
      {playing ? 'Pause' : 'Play'}
    </button>
  );
}

const cssVars = (v: Record<string, string | number>) => v as CSSProperties;

// ---- the life of one note, in cells of 0.05 USDC ----
type Life = { balance: number; reserved: number; text: string; paid?: number };
const LIFE: readonly Life[] = [
  { balance: 1, reserved: 0, text: 'Deposit: 1 USDC in a fixed size, plus a 0.05 USDC note fee.' },
  { balance: 1, reserved: 0.1, text: 'A chat starts. The lease reserves 0.10 USDC.' },
  {
    balance: 0.986,
    reserved: 0,
    text: 'The lease closes. Only the charge leaves the note: 0.014.',
  },
  { balance: 0.986, reserved: 0.1, text: 'A second lease reserves 0.10 again.' },
  { balance: 0.955, reserved: 0, text: 'It closes at 0.031. The rest of the note is still yours.' },
  {
    balance: 0,
    reserved: 0,
    paid: 0.955,
    text: 'Close from your wallet: the vault pays the whole balance, 0.955, to the payout account. A close sent by the operator costs 0.03.',
  },
];

export function NoteLife() {
  const ref = useRef<HTMLDivElement>(null);
  const [playing, setPlaying] = useState(true);
  const step = useStepper(ref, LIFE.length, 2200, playing);
  const m = LIFE[step] ?? LIFE[0]!;
  const view = { settled: m.balance - m.reserved, reserved: m.reserved, text: '' };
  return (
    <div className="nm-motion nm-note-play" ref={ref}>
      <div
        className="nm-cells nm-day-cells"
        style={cssVars({ '--n': 20, '--drain': 0 })}
        aria-hidden="true"
      >
        {Array.from({ length: 20 }, (_, i) => {
          const c = cellOf(i, view);
          const kind =
            c.kind === 'fill'
              ? 'nm-cell-fill'
              : c.kind === 'part'
                ? 'nm-cell-part'
                : c.kind === 'lease'
                  ? 'nm-cell-lease'
                  : '';
          return (
            <span
              key={i}
              className={`${kind} ${m.paid && i < 19 ? 'nm-cell-out' : ''}`}
              style={cssVars({ '--p': c.part, transitionDelay: `${i * 22}ms` })}
            />
          );
        })}
      </div>
      <p className="nm-day-log" aria-live="polite">
        <span>{String(step + 1).padStart(2, '0')}</span>
        {m.text}
        <Pause playing={playing} onToggle={() => setPlaying(!playing)} />
      </p>
    </div>
  );
}

// ---- the seven-day wait of an exit, one cell a day ----
export function ExitDays() {
  const ref = useRef<HTMLDivElement>(null);
  const step = useStepper(ref, 10, 700);
  const days = Math.min(step, 7);
  return (
    <div className="nm-life-window" ref={ref} aria-label="Seven day waiting period">
      <div className="nm-cells" style={cssVars({ '--n': 7, '--drain': 0 })} aria-hidden="true">
        {Array.from({ length: 7 }, (_, i) => (
          <span key={i} className={i < days ? 'nm-cell-fill' : 'nm-cell-lease'} />
        ))}
      </div>
      <span>
        <code data-on={days === 0}>request_exit</code>
        <code data-on={days > 0 && days < 7}>{days > 0 ? `day ${days}` : '7 days'}</code>
        <code data-on={days === 7}>finalize_exit</code>
      </span>
    </div>
  );
}

// ---- the set size: a lease names no note, so every open note could have paid ----
const GRID = 30;
const OPEN0 = [0, 1, 3, 4, 5, 7, 8, 10, 11, 12, 14, 16, 17, 19, 20, 22, 24, 25, 27, 28];
type Beat = { open: number[]; wave: boolean; text: string };
const BEATS: readonly Beat[] = [
  { open: OPEN0, wave: false, text: 'Open notes of one size. Nothing names which one pays.' },
  {
    open: OPEN0,
    wave: true,
    text: 'A lease arrives: a proof, a nullifier, a bound class. Any open note with enough balance could have paid.',
  },
  { open: [...OPEN0, 13], wave: false, text: 'A new deposit joins the set.' },
  {
    open: [...OPEN0, 13],
    wave: true,
    text: 'The next lease has one more note that could have paid.',
  },
  { open: OPEN0.filter((c) => c !== 8), wave: false, text: 'A note closes and leaves the set.' },
];

export function SetSize() {
  const ref = useRef<HTMLDivElement>(null);
  const [playing, setPlaying] = useState(true);
  const step = useStepper(ref, BEATS.length, 2400, playing);
  const b = BEATS[step] ?? BEATS[0]!;
  return (
    <div className="nm-motion nm-set" ref={ref}>
      <div className="nm-demo-bar">
        <span>What a lease reveals about the note</span>
        <span>
          Illustration, set size {b.open.length}
          <Pause playing={playing} onToggle={() => setPlaying(!playing)} />
        </span>
      </div>
      <div className="nm-set-grid" aria-hidden="true">
        {Array.from({ length: GRID }, (_, i) => {
          const on = b.open.includes(i);
          return (
            <span
              key={i}
              data-open={on}
              data-wave={on && b.wave}
              style={cssVars({ '--d': `${(i % 10) * 60 + Math.floor(i / 10) * 90}ms` })}
            />
          );
        })}
      </div>
      <p className="nm-day-log" aria-live="polite">
        <span>{String(step + 1).padStart(2, '0')}</span>
        {b.text}
      </p>
      <p className="nm-motion-foot">
        nymirad counts the set size on your machine from the vault, and each lease records how many
        notes could have paid for it. On testnet the set is small.
      </p>
    </div>
  );
}

// ---- a note's 30 days and the reminders that come before the end ----
const DAYS = 30;
export function ExpiryLine() {
  const ref = useRef<HTMLDivElement>(null);
  const [playing, setPlaying] = useState(true);
  const step = useStepper(ref, DAYS + 6, 260, playing);
  const day = Math.min(step, DAYS);
  const left = DAYS - day;
  const note =
    day >= DAYS
      ? 'The note has expired. It can still exit; the treasury can take it 7 days later.'
      : left <= 2
        ? 'Two days left: the last calendar reminder.'
        : left <= 7
          ? 'Seven days left: the second calendar reminder.'
          : left <= 14
            ? 'Fourteen days left: the app shows a banner and the first reminder is due.'
            : 'The note is open. Spend it, or close it.';
  return (
    <div className="nm-motion nm-expiry" ref={ref}>
      <div className="nm-demo-bar">
        <span>A note’s 30 days</span>
        <span>
          Day {day} of {DAYS}
          <Pause playing={playing} onToggle={() => setPlaying(!playing)} />
        </span>
      </div>
      <div
        className="nm-cells nm-day-cells"
        style={cssVars({ '--n': DAYS, '--drain': 0 })}
        aria-hidden="true"
      >
        {Array.from({ length: DAYS }, (_, i) => (
          <span
            key={i}
            className={
              i < day
                ? 'nm-cell-fill'
                : [DAYS - 14, DAYS - 7, DAYS - 2].includes(i)
                  ? 'nm-cell-lease'
                  : ''
            }
          />
        ))}
      </div>
      <div className="nm-day-axis nm-expiry-axis" aria-hidden="true">
        <span>Deposit</span>
        <span>14 days left</span>
        <span>7</span>
        <span>2</span>
        <span>Expiry</span>
      </div>
      <p className="nm-day-log" aria-live="polite" data-refused={left <= 2 ? 'true' : 'false'}>
        <span>{String(left).padStart(2, '0')}</span>
        {note}
      </p>
    </div>
  );
}

// ---- the three zones of an agent's budget: runs alone, held for you, day ends ----
export function Zones() {
  const ref = useRef<HTMLDivElement>(null);
  const step = useStepper(ref, 5, 1400);
  const lit = Math.min(step, 3);
  return (
    <div className="nm-budget-scale nm-zones" ref={ref}>
      <div className="nm-cells" style={cssVars({ '--n': 20, '--drain': 0 })} aria-hidden="true">
        {Array.from({ length: 20 }, (_, i) => {
          const kind =
            lit === 1 && i === 0
              ? 'nm-cell-fill'
              : lit === 2 && i === 1
                ? 'nm-cell-lease'
                : lit === 3
                  ? 'nm-cell-fill'
                  : '';
          return (
            <span
              key={i}
              className={kind}
              style={cssVars({ transitionDelay: `${lit === 3 ? i * 25 : 0}ms` })}
            />
          );
        })}
      </div>
      <div>
        <span data-on={lit === 1}>Runs alone up to 0.05</span>
        <span data-on={lit === 2}>Held for you up to 0.10</span>
        <span data-on={lit === 3}>Day ends at 1.00 USD</span>
      </div>
    </div>
  );
}
