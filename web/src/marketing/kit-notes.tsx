import type { ReactNode } from 'react';
import { Button, Strip, vars } from './kit';

// The parts of the Notes screen (ui/Notes.tsx): the note ledger, the passphrase field with its cell strip, and the deposit lookup row.

export function NoteLedger({
  columns,
  many = false,
  children,
}: {
  columns: readonly [string, string, string];
  /** Many rows: the list gets a fixed height and scrolls inside, the header stays. */
  many?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="nc-ledger" role="table" aria-label="Notes">
      <div className="nc-ledger-head" role="row">
        {columns.map((c) => (
          <span key={c}>{c}</span>
        ))}
        <span />
      </div>
      <div className="nc-ledger-body" role="rowgroup" data-many={many}>
        {children}
      </div>
    </div>
  );
}

export function NoteLedgerRow({
  index,
  active = false,
  pick,
  id,
  mid,
  life,
  late = false,
  actions,
}: {
  index: number;
  active?: boolean;
  /** Makes the whole row a selectable button, as in the preview. */
  pick?: { label: string; onPick: () => void };
  id: { strong: ReactNode; small: ReactNode };
  mid: ReactNode;
  life: ReactNode;
  late?: boolean;
  actions: ReactNode;
}) {
  return (
    <div className="nc-ledger-row" role="row" data-active={active} style={vars({ '--r': index })}>
      {pick && (
        <button
          type="button"
          className="nc-ledger-pick"
          aria-pressed={active}
          aria-label={pick.label}
          onClick={pick.onPick}
        />
      )}
      <span className="nc-ledger-id">
        <strong>{id.strong}</strong>
        <small>{id.small}</small>
      </span>
      <span className="nc-ledger-bal">{mid}</span>
      <span className="nc-ledger-life" data-late={late}>
        {life}
      </span>
      <span className="nc-ledger-act">{actions}</span>
    </div>
  );
}

export function PassField({
  label,
  value,
  min,
  length,
  onChange,
  testId,
}: {
  label: string;
  value: string;
  /** Characters needed; the strip fills up to it. */
  min: number;
  /** Characters typed so far, counted the way the key derivation counts them. */
  length?: number;
  onChange: (v: string) => void;
  testId?: string;
}) {
  const have = Math.min(length ?? value.length, min);
  return (
    <>
      <label className="nc-field">
        <span>{label}</span>
        <input
          type="password"
          autoComplete="off"
          value={value}
          data-testid={testId}
          onChange={(e) => onChange(e.target.value)}
        />
      </label>
      <div className="nc-pass" aria-hidden="true">
        <Strip n={min} solid={have} size="sm" />
        <small>
          {have} of {min} characters
        </small>
      </div>
    </>
  );
}

export function LookupRow({
  hash,
  onHash,
  onLookup,
  disabled,
  label,
  inputTestId,
  buttonTestId,
}: {
  hash: string;
  onHash: (v: string) => void;
  onLookup: () => void;
  disabled: boolean;
  label: string;
  inputTestId?: string;
  buttonTestId?: string;
}) {
  return (
    <div className="nc-lookup">
      <input
        value={hash}
        placeholder="transaction hash"
        aria-label="Transaction hash"
        data-testid={inputTestId}
        onChange={(e) => onHash(e.target.value.trim())}
      />
      <Button testId={buttonTestId} disabled={disabled} onClick={onLookup}>
        {label}
      </Button>
    </div>
  );
}
