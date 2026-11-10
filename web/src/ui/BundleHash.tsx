import { useEffect, useState } from 'react';

type State =
  { kind: 'loading' } | { kind: 'none' } | { kind: 'hash'; hash: string; version: string };

// The release's signed manifest is served next to the app (web/INTEGRITY.md). The footer shows the
// bundle hash it names. The host serves both, so this is not a check: `nymirad verify-web`
// compares the bundle itself with the signed manifest.
export function BundleHash() {
  const [state, setState] = useState<State>({ kind: 'loading' });
  useEffect(() => {
    let live = true;
    fetch('/manifest.json', { cache: 'no-store', credentials: 'omit' })
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then((m: { bundle_hash?: unknown; version?: unknown }) => {
        const hash = m.bundle_hash;
        const ok = typeof hash === 'string' && /^[0-9a-f]{64}$/.test(hash);
        if (live)
          setState(ok ? { kind: 'hash', hash, version: String(m.version) } : { kind: 'none' });
      })
      .catch(() => {
        if (live) setState({ kind: 'none' });
      });
    return () => {
      live = false;
    };
  }, []);
  if (state.kind === 'loading') return null;
  if (state.kind === 'none') {
    return (
      <small data-testid="bundle-hash">
        No manifest could be read. This build cannot show a bundle hash.
      </small>
    );
  }
  return (
    <details className="nm-bundle" data-testid="bundle-hash">
      <summary>
        Build {state.version} <span>Details</span>
      </summary>
      <div className="nm-bundle-detail">
        <code>{state.hash}</code>
        <p>
          The host reports this hash; it does not prove it. Check it with{' '}
          <code>nymirad verify-web</code>.
        </p>
        <a
          href="https://github.com/nymiralabs/nymira/releases"
          target="_blank"
          rel="noopener noreferrer"
        >
          GitHub releases
        </a>
      </div>
    </details>
  );
}
