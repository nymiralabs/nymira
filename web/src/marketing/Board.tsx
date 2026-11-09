import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';

// Scenes are drawn on a fixed 1000 x 520 stage in pixels. The board scales the whole stage to its
// width, so boxes, wires and moving packets stay aligned. Below 900px the stage becomes a column.
export const STAGE_W = 1000;
export const STAGE_H = 520;

export type Pt = readonly [x: number, y: number];
export type Box = readonly [x: number, y: number, w: number, h: number];

export const place = ([x, y, w, h]: Box): CSSProperties =>
  ({ '--l': `${x}px`, '--t': `${y}px`, '--w': `${w}px`, '--h': `${h}px` }) as CSSProperties;

export function Board({
  id,
  hot,
  children,
  label,
  height = STAGE_H,
}: {
  id: string;
  /** Stage point the lit dots rest on: the part that is working. */
  hot: Pt;
  children: ReactNode;
  /** A board that is not a tab panel names itself instead. */
  label?: string;
  /** Stage height in pixels when the scene needs less than the full stage. */
  height?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [scale, setScale] = useState(1);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setScale(entry.contentRect.width / STAGE_W);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return (
    <div
      className="nm-show-board"
      id={label ? undefined : 'nm-show-panel'}
      role={label ? 'group' : 'tabpanel'}
      aria-label={label}
      aria-labelledby={label ? undefined : `nm-tab-${id}`}
      ref={ref}
      style={
        {
          '--board-h': `${height}px`,
          '--s': scale,
          '--hx': `${hot[0] * scale}px`,
          '--hy': `${hot[1] * scale}px`,
        } as CSSProperties
      }
    >
      <div className="nm-show-stage">{children}</div>
    </div>
  );
}

export function Zone({ box, label }: { box: Box; label: string }) {
  return (
    <div className="nm-show-zone" style={place(box)} aria-hidden="true">
      <span>{label}</span>
    </div>
  );
}

export function Boundary({ x, y, h, label }: { x: number; y: number; h: number; label: string }) {
  return (
    <div className="nm-show-boundary" style={place([x, y, 0, h])} aria-hidden="true">
      <span>{label}</span>
    </div>
  );
}

/** Credit as discrete units. `filled` are spent or held, `leased` are bound to an operator lease. */
export function Cells({
  n,
  filled = 0,
  leased = 0,
  drain = 0,
  part = 0,
  className = '',
}: {
  n: number;
  filled?: number;
  leased?: number;
  /** Share of the cell after the filled ones that is still there; the rest was charged. */
  part?: number;
  /** Share of each leased cell already charged, 0 to 1. */
  drain?: number;
  className?: string;
}) {
  return (
    <div
      className={`nm-cells ${className}`}
      style={{ '--n': n, '--drain': drain, '--part': part } as CSSProperties}
      aria-hidden="true"
    >
      {Array.from({ length: n }, (_, i) => (
        <span
          key={i}
          className={
            i < filled
              ? 'nm-cell-fill'
              : i < filled + leased
                ? 'nm-cell-lease'
                : part && i === filled
                  ? 'nm-cell-part'
                  : ''
          }
        />
      ))}
    </div>
  );
}

export function Node({
  box,
  on = false,
  className = '',
  children,
}: {
  box: Box;
  on?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      className={`nm-flow-node nm-show-node ${on ? 'nm-node-active' : ''} ${className}`}
      style={place(box)}
    >
      {children}
    </div>
  );
}

export function Head({
  title,
  hint,
  chip,
  on,
}: {
  title: string;
  hint?: string;
  chip?: string;
  on?: boolean;
}) {
  return (
    <>
      <div className="nm-node-heading">
        <span>{title}</span>
        {chip ? (
          <span className={`nm-chip ${on ? 'nm-chip-on' : ''}`}>{chip}</span>
        ) : (
          <span className="nm-state-dot" />
        )}
      </div>
      {hint && <span className="nm-node-subtitle">{hint}</span>}
    </>
  );
}

export function Tag({ at, children }: { at: Pt; children: ReactNode }) {
  return (
    <span className="nm-show-tag" style={place([at[0], at[1], 0, 0])} aria-hidden="true">
      {children}
    </span>
  );
}

export function Row({ on, children }: { on: boolean; children: ReactNode }) {
  return <p className={`nm-show-row ${on ? 'nm-show-row-on' : ''}`}>{children}</p>;
}

export function Bar({ value, className = '' }: { value: number; className?: string }) {
  return (
    <div className={`nm-show-bar ${className}`}>
      <span style={{ width: `${Math.round(value * 100)}%` }} />
    </div>
  );
}

export interface Wire {
  pts: readonly Pt[];
  on: boolean;
  reverse?: boolean;
  dashed?: boolean;
  /** Short mono label carried by the moving packet. Needs a wire at least 90px long. */
  label?: string;
}

const wireLength = (pts: readonly Pt[]): number =>
  pts.slice(1).reduce((n, p, i) => n + Math.hypot(p[0] - pts[i]![0], p[1] - pts[i]![1]), 0);

// A path through the points with rounded corners.
function route(pts: readonly Pt[], radius = 10): string {
  const [first, ...rest] = pts;
  if (!first) return '';
  let d = `M${first[0]} ${first[1]}`;
  rest.forEach((point, i) => {
    const prev = pts[i]!;
    const next = rest[i + 1];
    if (!next) {
      d += ` L${point[0]} ${point[1]}`;
      return;
    }
    const lengthIn = Math.hypot(point[0] - prev[0], point[1] - prev[1]);
    const lengthOut = Math.hypot(next[0] - point[0], next[1] - point[1]);
    const r = Math.min(radius, lengthIn / 2, lengthOut / 2);
    const before = [
      point[0] - ((point[0] - prev[0]) / lengthIn) * r,
      point[1] - ((point[1] - prev[1]) / lengthIn) * r,
    ];
    const after = [
      point[0] + ((next[0] - point[0]) / lengthOut) * r,
      point[1] + ((next[1] - point[1]) / lengthOut) * r,
    ];
    d += ` L${before[0]} ${before[1]} Q${point[0]} ${point[1]} ${after[0]} ${after[1]}`;
  });
  return d;
}

export function Wires({ wires }: { wires: readonly Wire[] }) {
  return (
    <svg
      className="nm-show-wires"
      width={STAGE_W}
      height={STAGE_H}
      viewBox={`0 0 ${STAGE_W} ${STAGE_H}`}
      aria-hidden="true"
    >
      {wires.map((wire) => {
        const d = route(wire.pts);
        const key = wire.pts.map((p) => p.join(',')).join(' ');
        const ends = [wire.pts[0]!, wire.pts[wire.pts.length - 1]!];
        return (
          <g key={key}>
            <path
              d={d}
              className={`${wire.on ? 'nm-wire-on' : ''} ${wire.dashed ? 'nm-wire-dashed' : ''} ${wire.reverse ? 'nm-wire-rev' : ''}`}
            />
            {ends.map(([x, y]) => (
              <rect
                key={`${x},${y}`}
                className={`nm-handle ${wire.on ? 'nm-handle-on' : ''}`}
                x={x - 3}
                y={y - 3}
                width="6"
                height="6"
              />
            ))}
            {wire.on && (
              <g>
                <animateMotion
                  dur={`${Math.min(1.7, Math.max(0.9, wireLength(wire.pts) / 110))}s`}
                  repeatCount="indefinite"
                  path={d}
                  {...(wire.reverse
                    ? { keyPoints: '1;0', keyTimes: '0;1', calcMode: 'linear' }
                    : {})}
                />
                {wire.label ? (
                  <>
                    <rect
                      className="nm-packet-pill"
                      x={-(wire.label.length * 5.7 + 14) / 2}
                      y={-8}
                      width={wire.label.length * 5.7 + 14}
                      height={16}
                    />
                    <text className="nm-packet-text" y={3.5} textAnchor="middle">
                      {wire.label}
                    </text>
                  </>
                ) : (
                  <>
                    <circle r="9" className="nm-packet-glow" />
                    <circle r="3" className="nm-packet" />
                  </>
                )}
              </g>
            )}
          </g>
        );
      })}
    </svg>
  );
}

const reducedMotion = (): boolean =>
  typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Counts a number from its last value to the new one. */
export function Count({ value, digits = 2 }: { value: number; digits?: number }) {
  const [shown, setShown] = useState(value);
  const from = useRef(value);
  useEffect(() => {
    if (reducedMotion()) {
      from.current = value;
      setShown(value);
      return;
    }
    const start = performance.now();
    const origin = from.current;
    let frame = 0;
    const tick = (now: number) => {
      const t = Math.min(1, (now - start) / 900);
      const eased = 1 - (1 - t) ** 3;
      from.current = origin + (value - origin) * eased;
      setShown(from.current);
      if (t < 1) frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [value]);
  return <>{shown.toFixed(digits)}</>;
}

/** Types `text` out while `run` is true and shows all of it once `done`. */
export function Typed({ text, run, done }: { text: string; run: boolean; done: boolean }) {
  const [count, setCount] = useState(0);
  useEffect(() => {
    if (done || !run || reducedMotion()) return;
    setCount(0);
    const timer = setInterval(() => setCount((c) => Math.min(c + 1, text.length)), 18);
    return () => clearInterval(timer);
  }, [run, done, text]);
  return <>{done ? text : run ? text.slice(0, count) : ''}</>;
}

const HEX = '0123456789abcdef';

/** A hash that shuffles while it is being computed and locks to its value when done. */
export function Hash({
  value,
  running,
  done,
  matched = false,
}: {
  value: string;
  running: boolean;
  done: boolean;
  matched?: boolean;
}) {
  const [noise, setNoise] = useState(value);
  useEffect(() => {
    if (!running || done || reducedMotion()) return;
    const timer = setInterval(
      () =>
        setNoise(
          value
            .split('')
            .map((c) => (c === '…' ? c : HEX[Math.floor(Math.random() * 16)]))
            .join(''),
        ),
      60,
    );
    return () => clearInterval(timer);
  }, [running, done, value]);
  return (
    <code
      className={`nm-show-hash ${done ? 'nm-show-hash-done' : ''} ${matched ? 'nm-show-hash-match' : ''}`}
    >
      {done ? value : running ? noise : '…'}
    </code>
  );
}

/** Rows of hex that keep changing while `run` is true: what a relay sees of a sealed message. */
export function Cipher({
  run,
  rows = 3,
  cols = 24,
}: {
  run: boolean;
  rows?: number;
  cols?: number;
}) {
  const make = () =>
    Array.from({ length: rows }, () =>
      Array.from({ length: cols }, () => HEX[Math.floor(Math.random() * 16)]).join(''),
    );
  const [lines, setLines] = useState<string[]>(() =>
    Array.from({ length: rows }, (_, r) =>
      Array.from({ length: cols }, (_, c) => HEX[(r * 7 + c * 5 + 3) % 16]).join(''),
    ),
  );
  useEffect(() => {
    if (!run || reducedMotion()) return;
    const timer = setInterval(() => setLines(make()), 110);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [run, rows, cols]);
  return (
    <code className={`nm-show-cipher ${run ? 'nm-show-cipher-on' : ''}`} aria-hidden="true">
      {lines.join('\n')}
    </code>
  );
}

/** A slow `0` and `~` wave, the same material as the page background, shown while `run`. */
export function Wave({ run, rows = 3, cols = 34 }: { run: boolean; rows?: number; cols?: number }) {
  const [t, setT] = useState(0);
  useEffect(() => {
    if (!run || reducedMotion()) return;
    const timer = setInterval(() => setT((x) => x + 1), 90);
    return () => clearInterval(timer);
  }, [run]);
  const text = Array.from({ length: rows }, (_, r) =>
    Array.from({ length: cols }, (_, c) => {
      const v = Math.sin(c * 0.42 + t * 0.45 + r * 1.3) + Math.sin(c * 0.17 - t * 0.2 + r) * 0.8;
      return v > 0.9 ? '0' : v > -0.2 ? '~' : ' ';
    }).join(''),
  ).join('\n');
  return (
    <code className={`nm-show-wave ${run ? 'nm-show-wave-on' : ''}`} aria-hidden="true">
      {text}
    </code>
  );
}

/** A hash as single characters, so a match can sweep across it. */
export function HashLine({ value, lit, size = 11 }: { value: string; lit: boolean; size?: number }) {
  return (
    <code className={`nm-hashline ${lit ? 'nm-hashline-lit' : ''}`} style={{ fontSize: size }}>
      {value.split('').map((c, i) => (
        <span key={i} style={{ transitionDelay: `${i * 14}ms` }}>
          {c}
        </span>
      ))}
    </code>
  );
}
