import { Icon } from './Icon';
import { useEffect, useRef, useState } from 'react';
import { nymiraMark } from './brand';

export function TypedCommand({ text }: { text: string }) {
  const [typed, setTyped] = useState({ text, count: 0 });
  useEffect(() => {
    const motion = matchMedia('(prefers-reduced-motion: reduce)');
    let count = 0;
    const timer = setInterval(() => {
      count = motion.matches ? text.length : Math.min(count + 1, text.length);
      setTyped({ text, count });
      if (count === text.length) clearInterval(timer);
    }, 55);
    return () => clearInterval(timer);
  }, [text]);
  return (
    <>
      <span className="nm-sr-only">{text}</span>
      <span aria-hidden="true">{typed.text === text ? text.slice(0, typed.count) : ''}</span>
    </>
  );
}

export function LogoBackdrop() {
  const surface = useRef<HTMLPreElement>(null);

  useEffect(() => {
    const element = surface.current;
    const container = element?.parentElement;
    if (!element || !container) return;
    const motion = matchMedia('(prefers-reduced-motion: reduce)');
    const resolution = 160;
    const image = new Image();
    let mask: Uint8ClampedArray | null = null;
    let bounds = { left: 0, top: 0, width: resolution, height: resolution };
    let columns = 1;
    let rows = 1;
    let width = 1;
    let height = 1;
    let visible = true;
    let disposed = false;
    let frame = 0;
    let last = -100;
    let time = 0;

    const sample = (u: number, v: number) => {
      if (!mask || u < 0 || u >= 1 || v < 0 || v >= 1) return false;
      const x = Math.floor(bounds.left + u * bounds.width);
      const y = Math.floor(bounds.top + v * bounds.height);
      return mask[(y * resolution + x) * 4 + 3]! > 180;
    };
    const draw = () => {
      if (!mask) return;
      const lines: string[] = [];
      for (let y = 0; y < rows; y++) {
        const v = (y / rows) * 5;
        let line = '';
        for (let x = 0; x < columns; x++) {
          const u = (x / columns) * 15;
          const logoU = (x / columns - 0.24) / 0.7;
          const logoV = y / rows + Math.sin(time * 0.15) * 0.04;
          const fold = sample(logoU, logoV) ? 0.22 : 0;
          const bend = u + Math.sin(v * 0.9 + time * 0.3) * 1.7 + fold;
          // Thin the field behind the headline and let the folds of the mark carry the weight.
          const calm = Math.max(0, Math.min(0.9, (0.6 - x / columns) * 1.5));
          const field =
            Math.sin(bend - time * 0.4) +
            Math.cos(v - u * 0.2 + time * 0.16) -
            calm +
            (fold ? 0.4 : 0);
          line += field > 0.55 ? '0' : field > -0.2 ? '~' : ' ';
        }
        lines.push(line);
      }
      element.textContent = lines.join('\n');
    };
    const animate = (now: number) => {
      if (now - last >= 90) {
        time += last < 0 ? 0 : Math.min((now - last) / 1000, 0.15);
        draw();
        last = now;
      }
      frame = requestAnimationFrame(animate);
    };
    const schedule = () => {
      cancelAnimationFrame(frame);
      last = -100;
      if (disposed || !mask) return;
      draw();
      if (visible && !document.hidden && !motion.matches) frame = requestAnimationFrame(animate);
    };
    const resize = new ResizeObserver(([entry]) => {
      if (!entry) return;
      width = entry.contentRect.width;
      height = entry.contentRect.height;
      columns = Math.ceil(width / 5.2);
      rows = Math.ceil(height / 6.8) + 1;
      draw();
    });
    const observer = new IntersectionObserver(([entry]) => {
      visible = entry?.isIntersecting ?? false;
      schedule();
    });
    image.onload = () => {
      if (disposed) return;
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = resolution;
      const context = canvas.getContext('2d', { willReadFrequently: true });
      if (!context) return;
      context.drawImage(image, 0, 0, resolution, resolution);
      mask = context.getImageData(0, 0, resolution, resolution).data;
      let left = resolution,
        top = resolution,
        right = 0,
        bottom = 0;
      for (let y = 0; y < resolution; y++) {
        for (let x = 0; x < resolution; x++) {
          if (mask[(y * resolution + x) * 4 + 3]! > 230) {
            left = Math.min(left, x);
            right = Math.max(right, x);
            top = Math.min(top, y);
            bottom = Math.max(bottom, y);
          }
        }
      }
      if (right <= left || bottom <= top) return;
      bounds = { left, top, width: right - left + 1, height: bottom - top + 1 };
      schedule();
    };
    resize.observe(container);
    observer.observe(container);
    document.addEventListener('visibilitychange', schedule);
    motion.addEventListener('change', schedule);
    image.src = nymiraMark;
    return () => {
      disposed = true;
      image.onload = null;
      cancelAnimationFrame(frame);
      resize.disconnect();
      observer.disconnect();
      document.removeEventListener('visibilitychange', schedule);
      motion.removeEventListener('change', schedule);
    };
  }, []);

  return (
    <div className="nm-logo-backdrop" aria-hidden="true">
      <pre ref={surface} />
    </div>
  );
}
