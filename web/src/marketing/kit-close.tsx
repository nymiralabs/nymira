import type { ReactNode } from 'react';
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
