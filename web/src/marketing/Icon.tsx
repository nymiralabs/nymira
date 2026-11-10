const PATHS = {
  check: 'M3 8.5l3.2 3.2L13 4.5',
  chevron: 'M6 3.5L10.5 8 6 12.5',
  copy: 'M5.5 5.5h7a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1zM10.5 5.5V3.5a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2',
  terminal:
    'M3.5 2.5h9a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2zM4.5 6.5l2 1.75-2 1.75M8.5 10.5h3',
  plug: 'M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3',
  branch:
    'M4.5 5v6M11.5 7.5C11.5 10 9 10.7 6.2 11M4.5 2a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3zM4.5 11a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3zM11.5 4.5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3z',
  browser: 'M3.5 2.5h9a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2zM1.5 6h13',
  braces:
    'M6 2.5C4.5 2.5 4.5 3.5 4.5 5S4.5 7 3 8c1.5 1 1.5 1 1.5 3s0 2.5 1.5 2.5M10 2.5c1.5 0 1.5 1 1.5 2.5S11.5 7 13 8c-1.5 1-1.5 1-1.5 3s0 2.5-1.5 2.5',
  swap: 'M2.5 5.5h10.5l-2.5-2.5M13.5 10.5H3l2.5 2.5',
  onion:
    'M8 1.5a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13zM8 4.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7zM8 7a1 1 0 1 0 0 2 1 1 0 0 0 0-2z',
  lock: 'M4.5 7h7a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1-1.5 1.5h-7A1.5 1.5 0 0 1 3 12.5v-4A1.5 1.5 0 0 1 4.5 7zM5 7V5a3 3 0 0 1 6 0v2',
  shield: 'M8 1.5l5 2v4c0 3.2-2.1 5.6-5 7-2.9-1.4-5-3.8-5-7v-4zM5.8 8l1.7 1.7L10.5 6.5',
  hash: 'M6 2.5L5 13.5M11 2.5L10 13.5M2.5 6h11M2 10h11',
  refresh: 'M13 8a5 5 0 1 1-1.6-3.7M13 2.5v3h-3',
  down: 'M8 3v10M4 9l4 4 4-4',
  right: 'M3 8h10M9 4l4 4-4 4',
  downRight: 'M4.5 4.5l7 7M11.5 5v6.5H5',
  upRight: 'M4.5 11.5l7-7M5 4.5h6.5V11',
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  return (
    <svg
      className="nm-icon"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.4"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}
