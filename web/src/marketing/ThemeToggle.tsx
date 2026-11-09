import { useId } from 'react';

// The sun and the moon are one drawing. In light mode the disc is small and the rays are out; in
// dark mode the disc grows, a second disc slides over it to cut a crescent, and the rays fold in.
export function ThemeToggle({
  theme,
  onTheme,
  className = '',
}: {
  theme: 'dark' | 'light';
  onTheme: () => void;
  className?: string;
}) {
  const id = `nm-tt-${useId().replace(/:/g, '')}`;
  return (
    <button
      className={`nm-icon-button nm-theme-toggle ${className}`}
      type="button"
      aria-label={`Switch to ${theme === 'dark' ? 'light' : 'dark'} mode`}
      onClick={onTheme}
    >
      <svg
        className="nm-tt"
        data-mode={theme}
        width="20"
        height="20"
        viewBox="0 0 24 24"
        aria-hidden="true"
      >
        <mask id={id}>
          <rect width="24" height="24" fill="#fff" />
          <circle className="nm-tt-cut" cx="17" cy="7" r="6" fill="#000" />
        </mask>
        <circle
          className="nm-tt-body"
          cx="12"
          cy="12"
          r="7"
          fill="currentColor"
          mask={`url(#${id})`}
        />
        <g className="nm-tt-rays" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
          {[0, 45, 90, 135, 180, 225, 270, 315].map((a) => (
            <path key={a} d="M12 1.5v2.2" transform={`rotate(${a} 12 12)`} />
          ))}
        </g>
      </svg>
    </button>
  );
}
