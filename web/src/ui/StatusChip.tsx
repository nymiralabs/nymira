import type { VaultCheck } from '../chain/vault';
import './statuschip.css';

/**
 * A line that says how the app stands, from the vault check the app has already made. It reads
 * nothing new, so a visitor who does not open the status page sends no extra request.
 */
export function StatusChip({
  check,
  onStatus,
  dot = false,
}: {
  check: VaultCheck | null;
  onStatus: () => void;
  /** Only the mark, for a narrow place: the words stay in its label. */
  dot?: boolean;
}) {
  const [level, text] = !check
    ? (['idle', 'Checking…'] as const)
    : check.status === 'ok'
      ? (['ok', 'Vault OK'] as const)
      : check.status === 'mismatch'
        ? (['fail', 'Vault differs'] as const)
        : check.status === 'unpinned'
          ? (['warn', 'No vault pinned'] as const)
          : (['warn', 'Chain unreachable'] as const);
  return (
    <a
      className={dot ? 'nm-statuschip nm-statuschip-dot' : 'nm-statuschip'}
      title={`${text}. Open the status page.`}
      aria-label={`${text}. Open the status page.`}
      data-level={level}
      data-testid="status-chip"
      href="#status"
      onClick={(e) => {
        e.preventDefault();
        onStatus();
      }}
    >
      <i aria-hidden="true" />
      {!dot && <span>{text}</span>}
      {!dot && <b>Status</b>}
    </a>
  );
}
