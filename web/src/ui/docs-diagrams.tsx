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
