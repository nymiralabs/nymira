import { useEffect, useMemo, useRef, useState } from 'react';
import { Markdown, parseMarkdown, textLength } from './markdown';

const reduced = () =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Time the whole answer takes to appear: about 11 ms a character, never under 0.4 s or over 3.5 s. */
export const typeDuration = (chars: number) => Math.min(3500, Math.max(400, chars * 11));

/**
 * An answer that has already arrived, drawn as markdown and written out the way a streamed one
 * would come in. The whole text is in place from the first frame (the part not yet written is
 * hidden), so nothing moves while it appears; a click, or a reduced-motion setting, shows it at once.
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
  const tree = useMemo(() => parseMarkdown(text), [text]);
  const total = useMemo(() => textLength(tree), [tree]);
  const instant = !animate || reduced();
  const [shown, setShown] = useState(instant ? total : 0);
  const progress = useRef(onProgress);
  progress.current = onProgress;

  useEffect(() => {
    if (instant) {
      setShown(total);
      return;
    }
    const duration = typeDuration(total);
    const started = performance.now();
    let frame = 0;
    let last = 0;
    let lastCount = -1;
    const step = (now: number) => {
      const done = Math.min(1, (now - started) / duration);
      // Ease out: the first words come quickly, the end settles.
      const count = Math.round(total * (1 - Math.pow(1 - done, 2)));
      if (count !== lastCount) {
        lastCount = count;
        setShown(count);
      }
      if (now - last > 120) {
        last = now;
        progress.current?.();
      }
      if (done < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [total, instant]);

  const writing = shown < total;
  return (
    <div
      className="nc-typed nc-md"
      data-writing={writing || undefined}
      aria-busy={writing || undefined}
      onClick={writing ? () => setShown(total) : undefined}
    >
      <Markdown tree={tree} shown={shown} />
    </div>
  );
}
