import { vars } from './kit';

// Parts of the privacy panel the console preview and the real app share.

/** The set as cells: one per note that could have paid. */
export function SetCells({ ext, team, label }: { ext: number; team: number; label: string }) {
  return (
    <div className="nc-setcells" role="img" aria-label={label}>
      {Array.from({ length: ext + team }, (_, i) => (
        <span
          key={i}
          data-k={i < ext ? (i === 0 ? 'own' : 'ext') : 'team'}
          style={vars({ '--i': i })}
        />
      ))}
    </div>
  );
}

export function SetLegend() {
  return (
    <p className="nc-setlegend">
      <span>
        <i data-k="own" /> your note
      </span>
      <span>
        <i data-k="ext" /> external
      </span>
      <span>
        <i data-k="team" /> the team’s
      </span>
    </p>
  );
}

export type Flag = { kind: 'stands' | 'small' | 'timing'; text: string };
export const FLAG_NAME = { stands: 'stands out', small: 'small set', timing: 'timing' } as const;

const SMALL_BELOW = 5;

export type SetBar = {
  key: string;
  /** Notes that could have paid, from outside the team. */
  external: number;
  total: number;
  /** The lease is still open. */
  open?: boolean;
  label: string;
};

/**
 * The set size of each lease as bars, oldest on the left. The dashed line is where an external set
 * counts as small; a bar under it is drawn in the warning colour.
 */
export function SetBars({
  bars,
  onPick,
}: {
  bars: readonly SetBar[];
  onPick?: (key: string) => void;
}) {
  const top = Math.max(SMALL_BELOW * 2, ...bars.map((b) => b.total));
  const pct = (n: number) => `${(n / top) * 100}%`;
  return (
    <figure className="nc-bars" data-testid="set-bars">
      <div className="nc-bars-plot" role="img" aria-label="Set size of each lease">
        <span className="nc-bars-line" style={{ bottom: pct(SMALL_BELOW) }}>
          <small>under {SMALL_BELOW} is small</small>
        </span>
        {bars.map((b, i) => (
          <button
            key={b.key}
            type="button"
            className="nc-bar"
            data-small={b.external < SMALL_BELOW}
            data-open={b.open}
            style={vars({ '--i': i, '--h': pct(b.total) })}
            onClick={() => onPick?.(b.key)}
            aria-label={`${b.label}: ${b.external} external, ${b.total} in all`}
          >
            <b>{b.external}</b>
            <span className="nc-bar-total">
              <span
                className="nc-bar-ext"
                style={{ height: `${(b.external / Math.max(b.total, 1)) * 100}%` }}
              />
            </span>
            <small>{b.label}</small>
          </button>
        ))}
      </div>
      <figcaption className="nc-setlegend">
        <span>
          <i data-k="ext" /> external
        </span>
        <span>
          <i data-k="team" /> the team’s
        </span>
      </figcaption>
    </figure>
  );
}

export type Stop = { key: string; at: number; label: string; tone?: 'open' | 'warn' };

const clampPct = (v: number) => Math.min(100, Math.max(0, v));

/** A time axis from `start` to `end` with a mark for each event; the marks are buttons. */
export function Timeline({
  start,
  end,
  stops,
  startLabel,
  endLabel,
  onPick,
  fill,
}: {
  start: number;
  end: number;
  stops: readonly Stop[];
  startLabel: string;
  endLabel: string;
  onPick?: (key: string) => void;
  /** Colours the axis from the start up to this time. */
  fill?: number;
}) {
  const span = Math.max(end - start, 1);
  const at = (t: number) => `${clampPct(((t - start) / span) * 100)}%`;
  return (
    <div className="nc-timeline">
      <div className="nc-timeline-axis">
        {fill !== undefined && <i style={{ width: at(fill) }} />}
        {stops.map((s) => (
          <button
            key={s.key}
            type="button"
            data-tone={s.tone}
            style={{ left: at(s.at) }}
            title={s.label}
            aria-label={s.label}
            onClick={() => onPick?.(s.key)}
          />
        ))}
      </div>
      <p>
        <span>{startLabel}</span>
        <span>{endLabel}</span>
      </p>
    </div>
  );
}

export type Stage = {
  key: string;
  label: string;
  /** Who can see this step: the chain, the operator or only you. */
  lane: 'on chain' | 'operator' | 'you';
  state: 'done' | 'now' | 'todo';
  detail: string;
};

/** Where a note is on its way: each step with who can see it, the finished ones joined by a line. */
export function Journey({ stages }: { stages: readonly Stage[] }) {
  return (
    <ol className="nc-journey" data-testid="journey">
      {stages.map((s, i) => (
        <li key={s.key} data-state={s.state} style={vars({ '--i': i })}>
          <span className="nc-journey-node" aria-hidden="true" />
          <strong>{s.label}</strong>
          <small>{s.detail}</small>
          <em data-lane={s.lane}>{s.lane}</em>
        </li>
      ))}
    </ol>
  );
}

/** The set against the line for a small set: how far the external count reaches. */
export function SetGauge({ external, total }: { external: number; total: number }) {
  const top = Math.max(SMALL_BELOW * 2, total);
  return (
    <div className="nc-gauge" role="img" aria-label={`${external} external of ${total}`}>
      <div className="nc-gauge-track">
        <i className="nc-gauge-all" style={{ width: `${(total / top) * 100}%` }} />
        <i
          className="nc-gauge-ext"
          data-small={external < SMALL_BELOW}
          style={{ width: `${(external / top) * 100}%` }}
        />
        <b style={{ left: `${(SMALL_BELOW / top) * 100}%` }}>
          <small>{SMALL_BELOW}</small>
        </b>
      </div>
      <p>
        <span>0</span>
        <span>{top}</span>
      </p>
    </div>
  );
}

/**
 * The two views of a note and what stands between them. While the note stays, the proofs leave no
 * link between the deposit and the leases, and three channels can still join them; once it has
 * started to leave, one solid line ties the leases to the note ID.
 */
export function LinkDiagram({ left }: { left: boolean }) {
  return (
    <div className="nc-linkmap" data-left={left} aria-hidden="true">
      <span className="nc-link-node">
        <b>On chain</b>
        <small>deposit</small>
      </span>
      <div className="nc-link-mid">
        {left ? (
          <p className="nc-link-solid">
            <i />
            <em>note ID</em>
          </p>
        ) : (
          <>
            <p className="nc-link-gap">
              <i />
              <em>no zero-knowledge link</em>
              <i />
            </p>
            <ul>
              <li>
                <i />
                <span>network address</span>
              </li>
              <li>
                <i />
                <span>timing and usage metadata</span>
              </li>
              <li>
                <i />
                <span>timing alone, while the set is small</span>
              </li>
            </ul>
          </>
        )}
      </div>
      <span className="nc-link-node">
        <b>Operator</b>
        <small>leases</small>
      </span>
    </div>
  );
}
