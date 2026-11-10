import { useEffect, useRef, useState } from 'react';
import { Cells } from './Board';
import sharedNoteFlow from './assets/shared-note-flow.webp';

// One agent budget reached through two doors. The policy is the example from the agents section:
// 1.00 USD a day, 0.10 per call, approval above 0.05. Amounts are illustrative; the answers
// (allowed, approval_required, call_limit) are the ones the README documents.
const DAY = 1;
const CELL = 0.05;

type Outcome = 'allowed' | 'held' | 'refused';
const EVENTS: readonly {
  via: 'MCP' | 'HTTP' | 'You';
  call: string;
  outcome: Outcome;
  answer: string;
  cost: number;
}[] = [
  {
    via: 'MCP',
    call: 'nymira_ask · max_usd 0.02',
    outcome: 'allowed',
    answer: 'charged 0.012',
    cost: 0.012,
  },
  {
    via: 'HTTP',
    call: 'POST /v1/chat/completions',
    outcome: 'allowed',
    answer: 'charged 0.021',
    cost: 0.021,
  },
  {
    via: 'MCP',
    call: 'nymira_ask · max_usd 0.08',
    outcome: 'held',
    answer: '402 approval_required',
    cost: 0,
  },
  { via: 'You', call: 'Approve the held call', outcome: 'allowed', answer: 'owner only', cost: 0 },
  {
    via: 'MCP',
    call: 'nymira_ask + approval_id',
    outcome: 'allowed',
    answer: 'charged 0.062',
    cost: 0.062,
  },
  {
    via: 'HTTP',
    call: 'POST · worst case 0.15',
    outcome: 'refused',
    answer: '402 call_limit',
    cost: 0,
  },
];

export function Workflow() {
  const reduced =
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  const [shown, setShown] = useState(reduced ? EVENTS.length : 0);
  const holder = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = holder.current;
    if (!element || reduced) return;
    let visible = false;
    const observer = new IntersectionObserver(([entry]) => {
      visible = entry?.isIntersecting ?? false;
    });
    observer.observe(element);
    const timer = setInterval(() => {
      if (!visible || document.hidden) return;
      setShown((n) => (n > EVENTS.length + 1 ? 0 : n + 1));
    }, 1200);
    return () => {
      clearInterval(timer);
      observer.disconnect();
    };
  }, [reduced]);

  const done = Math.min(shown, EVENTS.length);
  const spent = EVENTS.slice(0, done).reduce((sum, e) => sum + e.cost, 0);
  const held = done >= 3 && done < 4;

  return (
    <div className="nm-flow" ref={holder}>
      <ol className="nm-flow-log" aria-label="Calls from two clients on one budget">
        {EVENTS.map((e, i) => (
          <li
            key={`${e.call}-${i}`}
            data-hidden={i >= done ? 'true' : 'false'}
            data-outcome={e.outcome}
            data-latest={i === done - 1 ? 'true' : 'false'}
          >
            <span className="nm-flow-via">{e.via}</span>
            <code>{e.call}</code>
            <span className="nm-flow-answer">{e.answer}</span>
          </li>
        ))}
      </ol>
      <div className="nm-flow-note">
        <div className="nm-flow-note-head">
          <span>Budget note · agent worker</span>
          <span data-held={held ? 'true' : 'false'}>{held ? 'waiting for you' : 'open'}</span>
        </div>
        <Cells n={Math.round(DAY / CELL)} filled={Math.ceil(spent / CELL - 1e-9)} />
        <dl>
          <div>
            <dt>Spent today</dt>
            <dd>
              {spent.toFixed(3)} of {DAY.toFixed(2)} USD
            </dd>
          </div>
          <div>
            <dt>Per call</dt>
            <dd>up to 0.10 USD</dd>
          </div>
          <div>
            <dt>Asks first above</dt>
            <dd>0.05 USD</dd>
          </div>
        </dl>
        <div className="nm-flow-caption" data-held={held}>
          <img
            src={sharedNoteFlow}
            alt=""
            draggable={false}
            loading="lazy"
            width="900"
            height="384"
          />
          <p>Both doors share one note and one policy. Illustration, example amounts.</p>
        </div>
      </div>
    </div>
  );
}
