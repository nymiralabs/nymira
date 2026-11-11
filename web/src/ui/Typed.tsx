import { useEffect, useRef, useState } from 'react';

const reduced = () =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Time the whole answer takes to appear: about 11 ms a character, never under 0.4 s or over 3.5 s. */
export const typeDuration = (chars: number) => Math.min(3500, Math.max(400, chars * 11));

/**
 * An answer that has already arrived, written out the way it would have streamed in. The text is
 * all there for the page (the rest is hidden, so nothing moves while it is written); a click, or a
 * reduced-motion setting, shows it at once.
 */
export function Typed({
  text,
  animate,
  onProgress,
}: {
  text: string;
  animate: boolean;
  onProgress?: () => void;
}) {
  const instant = !animate || reduced();
  const [shown, setShown] = useState(instant ? text.length : 0);
  const progress = useRef(onProgress);
  progress.current = onProgress;

  useEffect(() => {
    if (instant) {
      setShown(text.length);
      return;
    }
    const total = typeDuration(text.length);
    const started = performance.now();
    let frame = 0;
    let last = 0;
    const step = (now: number) => {
      const done = Math.min(1, (now - started) / total);
      // Ease out: the first words come quickly, the end settles.
      const eased = 1 - Math.pow(1 - done, 2);
      const count = Math.round(text.length * eased);
      setShown(count);
      if (now - last > 120) {
        last = now;
        progress.current?.();
      }
      if (done < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [text, instant]);

  const writing = shown < text.length;
  return (
    <p
      className="nc-typed"
      data-writing={writing || undefined}
      aria-busy={writing || undefined}
      onClick={writing ? () => setShown(text.length) : undefined}
    >
      {text.slice(0, shown)}
      {writing && (
        <>
          <i className="nc-caret" aria-hidden="true" />
          <span className="nc-typed-rest" aria-hidden="true">
            {text.slice(shown)}
          </span>
        </>
      )}
    </p>
  );
}
