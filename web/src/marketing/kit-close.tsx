import { useState, type ReactNode } from 'react';
import { Icon } from './Icon';
import { Button, Strip, vars } from './kit';

/** A note close to its expiry: cells for the 14 days, the notice and a way to the Close screen. */
export function ExpiryBanner({
  left,
  testId,
  due,
  children,
  onOpen,
}: {
  /** Whole days left, 0 to 14. */
  left: number;
  testId?: string;
  due?: string;
  children: ReactNode;
  onOpen?: () => void;
}) {
  return (
    <div className="nc-banner" role="status" data-testid={testId} data-due={due}>
      <Strip n={14} solid={14 - left} size="sm" label={`${left} days left`} />
      <p>{children}</p>
      {onOpen && <Button onClick={onOpen}>Open the Close screen</Button>}
    </div>
  );
}

export type ExpiryItem = {
  id: string;
  noteId?: number;
  /** Whole days left, 0 to 14. */
  left: number;
  due?: string;
  /** The notice in full: shown for the soonest note. */
  text: ReactNode;
  /** The same in short, for the list: "expires in 2 days". */
  when: string;
  /** Notice level in days (14, 7 or 2). */
  level?: number;
};

/** The last 14 days as a rail: each note stands in the column of the days it has left. */
function Runway({ items, soonest }: { items: readonly ExpiryItem[]; soonest: string }) {
  const [picked, setPicked] = useState(soonest);
  const [hover, setHover] = useState<string | null>(null);
  /** The day whose stack is shown in full; the others fold into "+N" past three notes. */
  const [wide, setWide] = useState<number | null>(null);
  const days = Array.from({ length: 15 }, (_, i) => 14 - i);
  const at = items.find((n) => n.id === (hover ?? picked)) ?? items[0]!;
  const count = (f: (n: ExpiryItem) => boolean) => items.filter(f).length;
  const FOLD = 3;

  // Arrow keys walk the markers: left and right across days, up and down inside a stack.
  const walk = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const keys = ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'];
    if (!keys.includes(e.key)) return;
    const all = [...e.currentTarget.querySelectorAll<HTMLButtonElement>('.nc-mk')];
    const i = all.indexOf(document.activeElement as HTMLButtonElement);
    if (i < 0) return;
    e.preventDefault();
    const col = (el: HTMLElement) => el.closest('.nc-day');
    const same = all.filter((m) => col(m) === col(all[i]!));
    const j = same.indexOf(all[i]!);
    const to =
      e.key === 'ArrowRight'
        ? all.slice(i + 1).find((m) => col(m) !== col(all[i]!))
        : e.key === 'ArrowLeft'
          ? [...all.slice(0, i)].reverse().find((m) => col(m) !== col(all[i]!))
          : e.key === 'ArrowUp'
            ? same[j + 1]
            : same[j - 1];
    to?.focus();
  };

  return (
    <div className="nc-runway" data-testid="expiry-runway">
      <div className="nc-runway-sum">
        <span data-due="exit">
          <b>{count((n) => n.due === 'exit')}</b> exit due
        </span>
        <span data-due="close">
          <b>{count((n) => n.due === 'close')}</b> close due
        </span>
        <span>
          <b>{count((n) => n.due !== 'exit' && n.due !== 'close')}</b> notice only
        </span>
      </div>
      <div className="nc-runway-rail" role="group" aria-label="Notes by days left" onKeyDown={walk}>
        {days.map((d, col) => {
          const here = items.filter((n) => n.left === d);
          const folded = here.length > FOLD && wide !== d;
          const shown = folded ? here.slice(0, FOLD - 1) : here;
          return (
            <div
              key={d}
              className="nc-day"
              style={{ '--c': col } as React.CSSProperties}
              data-zone={d <= 3 ? 'exit' : d <= 5 ? 'close' : undefined}
              data-notice={d === 14 || d === 7 || d === 2 ? '' : undefined}
              data-full={here.length > 0 ? '' : undefined}
            >
              <div className="nc-day-stack">
                {shown.map((n) => (
                  <button
                    key={n.id}
                    type="button"
                    className="nc-mk"
                    data-due={n.due}
                    style={
                      {
                        '--u': n.due === 'exit' ? 0 : n.due === 'close' ? 1 : 2,
                      } as React.CSSProperties
                    }
                    aria-pressed={n.id === at.id}
                    aria-label={`${n.noteId === undefined ? 'Note' : `Note #${n.noteId}`}, ${n.when}`}
                    onClick={() => setPicked(n.id)}
                    onMouseEnter={() => setHover(n.id)}
                    onMouseLeave={() => setHover(null)}
                    onFocus={() => setHover(n.id)}
                    onBlur={() => setHover(null)}
                  >
                    <span>{n.noteId === undefined ? '·' : `#${n.noteId}`}</span>
                  </button>
                ))}
                {folded && (
                  <button
                    type="button"
                    className="nc-mk nc-mk-more"
                    data-due={here[0]!.due}
                    aria-label={`${here.length - shown.length} more notes with ${d} days left`}
                    onClick={() => setWide(d)}
                  >
                    <span>+{here.length - shown.length}</span>
                  </button>
                )}
              </div>
              <small>{d}</small>
            </div>
          );
        })}
      </div>
      <div className="nc-runway-foot">
        <p className="nc-runway-info" aria-live="polite" key={at.id}>
          <strong>{at.noteId === undefined ? 'A note' : `Note #${at.noteId}`}</strong>
          <span>{at.when}</span>
          {at.level !== undefined && <em>{at.level}-day notice</em>}
          {(at.due === 'exit' || at.due === 'close') && <b data-due={at.due}>{at.due} due</b>}
          <span className="nc-runway-more">
            {at.due === 'exit'
              ? 'The app starts an exit on this note. It waits for your wallet signature.'
              : at.due === 'close'
                ? 'The app no longer opens leases on it and starts a close; at 3 days it starts an exit. Each waits for your wallet signature.'
                : 'A notice only for now. At 5 days the app stops opening leases on it and starts a close.'}
          </span>
        </p>
        <p className="nc-runway-key">
          <span>
            <i data-zone="close" /> close at 5 days
          </span>
          <span>
            <i data-zone="exit" /> exit at 3 days
          </span>
        </p>
      </div>
    </div>
  );
}

/**
 * Several notes near their expiry in one banner. The soonest one reads exactly like a single
 * banner; the others sit behind "N more", each in the same row layout.
 */
export function ExpiryStack({
  items,
  onOpen,
}: {
  items: readonly ExpiryItem[];
  onOpen?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [first, ...rest] = [...items].sort((a, b) => a.left - b.left);
  if (!first) return null;
  return (
    <div
      className="nc-banner nc-banner-stack"
      role="status"
      data-testid="expiry-banner"
      data-due={first.due}
      data-open={open}
    >
      <div className="nc-banner-row">
        <Strip n={14} solid={14 - first.left} size="sm" label={`${first.left} days left`} />
        <p>{first.text}</p>
        <div className="nc-banner-act">
          {rest.length > 0 && (
            <button
              type="button"
              className="nc-banner-more"
              aria-expanded={open}
              onClick={() => setOpen(!open)}
            >
              {open ? 'Show less' : `${rest.length} more`}
              <Icon name="chevron" size={12} />
            </button>
          )}
          {onOpen && <Button onClick={onOpen}>Open the Close screen</Button>}
        </div>
      </div>
      {rest.length > 0 && (
        <div className="nc-banner-rest" data-open={open}>
          <div className="nc-banner-fold">
            <Runway items={items} soonest={first.id} />
          </div>
        </div>
      )}
    </div>
  );
}

/** The seven waiting days of an exit as cells, with the two calls at its ends. */
export function ExitDays({
  days,
  requested,
  children,
}: {
  /** Days passed, 0 to 7. */
  days: number;
  requested: boolean;
  children?: ReactNode;
}) {
  return (
    <div className="nc-exit">
      <Strip n={7} solid={days} size="lg" label="Waiting days" />
      <div className="nc-exit-key">
        <span data-on={!requested}>request_exit</span>
        <span data-on={requested && days < 7}>{days > 0 ? `day ${days} of 7` : '7 days'}</span>
        <span data-on={days === 7}>finalize_exit</span>
      </div>
      {children}
    </div>
  );
}

/** Numbered stages of a close or an exit: done, now or next. */
export function FlowSteps({
  steps,
}: {
  steps: readonly { text: string; s: 'done' | 'now' | 'next' }[];
}) {
  return (
    <ol className="nc-flow">
      {steps.map(({ text, s }, i) => (
        <li key={text} data-s={s}>
          <span aria-hidden="true">{s === 'done' ? '✓' : i + 1}</span>
          {text}
        </li>
      ))}
    </ol>
  );
}

const GUARD_MARKS: readonly [number, string][] = [
  [14, 'notice'],
  [7, 'notice'],
  [5, 'no new leases · close'],
  [3, 'exit'],
  [2, 'notice'],
  [0, 'to the treasury'],
];

/** The days at which the expiry guard acts, on the last 14 days. */
export function GuardTimeline({ off }: { off: boolean }) {
  return (
    <div className="nc-guard" data-off={off}>
      <Strip n={14} solid={0} size="lg" label="The last 14 days" />
      <ol>
        {GUARD_MARKS.map(([d, t]) => (
          <li key={d} style={vars({ '--d': d })}>
            <b>{d}d</b>
            <span>{t}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}
