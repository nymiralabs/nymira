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
