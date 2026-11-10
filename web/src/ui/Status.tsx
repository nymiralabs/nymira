import { useEffect, useState } from 'react';
import { Wordmark } from '../marketing/Wordmark';
import { ThemeToggle } from '../marketing/ThemeToggle';
import { vars } from '../marketing/kit';
import { PROFILE } from '../profile';
import { reportText, type Check, type Level } from '../status/live';
import { useReport } from '../status/useReport';
import './status.css';

const WORD: Record<Level, string> = { ok: 'OK', warn: 'NOTE', fail: 'FAIL' };
const ORDER: Check['id'][] = ['operator', 'vault', 'rpc', 'release'];
const TITLES: Record<Check['id'], string> = {
  operator: 'Operator',
  vault: 'Vault',
  rpc: 'NEAR RPC',
  release: 'This release',
};
const HINT: Record<Check['id'], string> = {
  operator: 'Signs leases and charges. Runs on a home machine.',
  vault: 'Holds the USDC. Compared with the pins in this bundle.',
  rpc: 'Where this app reads the chain. Each endpoint is asked on its own.',
  release: 'The manifest this host serves for the bundle you loaded.',
};

const hostOf = (u: string) => {
  try {
    return new URL(u).host;
  } catch {
    return u;
  }
};

export function Status({
  theme,
  onTheme,
  onHome,
  onDocs,
  onOpenApp,
}: {
  theme: 'dark' | 'light';
  onTheme: () => void;
  onHome: () => void;
  onDocs: () => void;
  onOpenApp: () => void;
}) {
  const [auto, setAuto] = useState(false);
  const { report, running, history, run } = useReport(auto);
  const [copied, setCopied] = useState(false);
  const [left, setLeft] = useState(0);

  // One check when the screen opens: the app reads the same hosts at load anyway.
  useEffect(() => {
    void run();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The countdown to the next automatic check.
  useEffect(() => {
    if (!auto || !report) return;
    const tick = setInterval(
      () => setLeft(Math.max(0, 30 - Math.floor((Date.now() - report.at) / 1000))),
      500,
    );
    return () => clearInterval(tick);
  }, [auto, report]);

  const byId = new Map(report?.checks.map((c) => [c.id, c]));
  const level: Level | 'idle' = report?.overall ?? 'idle';
  const hosts = [PROFILE.operatorUrl, ...PROFILE.rpcUrls].map(hostOf);

  return (
    <div className="nm-site nm-app ns" data-theme={theme} data-testid="status">
      <header className="ns-header">
        <a
          className="ns-brand"
          href="#home"
          aria-label="Nymira home"
          draggable={false}
          onClick={(e) => {
            e.preventDefault();
            onHome();
          }}
        >
          <Wordmark />
        </a>
        <span className="ns-where">Status</span>
        <span className="ns-spacer" />
        <button type="button" className="ns-link" onClick={onDocs}>
          Docs
        </button>
        <ThemeToggle theme={theme} onTheme={onTheme} />
        <button type="button" className="ns-primary" onClick={onOpenApp}>
          Open the app
        </button>
      </header>

      <main className="ns-main">
        <section className="ns-verdict" data-level={level} aria-live="polite" aria-busy={running}>
          <div className="ns-mark" aria-hidden="true">
            {ORDER.map((id, i) => (
              <i
                key={id}
                data-level={byId.get(id)?.level ?? 'idle'}
                data-run={running}
                style={vars({ '--n': i })}
              />
            ))}
          </div>
          <div className="ns-verdict-text">
            <h1>
              <span className="ns-word" data-testid="status-word">
                {report ? WORD[report.overall] : running ? '…' : '—'}
              </span>
              {report ? report.headline : running ? 'Checking.' : 'Not checked yet.'}
            </h1>
            <p>
              {report
                ? `Checked ${new Date(report.at).toLocaleTimeString()} on ${PROFILE.network}. The numbers are what this browser read just now, not a history.`
                : `The operator, the vault, the RPC endpoints and this release, read from your browser.`}
            </p>
          </div>
          <div className="ns-actions">
            <button
              type="button"
              className="ns-primary"
              onClick={() => void run()}
              disabled={running}
            >
              {running ? 'Checking…' : 'Check now'}
            </button>
            <button
              type="button"
              role="switch"
              aria-checked={auto}
              className="ns-auto"
              onClick={() => setAuto((a) => !a)}
            >
              <i aria-hidden="true" />
              <span>
                Check again by itself
                <small>
                  every 30 s{auto && report ? ` · next in ${left} s` : auto ? '' : ' · off'}
                </small>
              </span>
            </button>
          </div>
        </section>

        <ol className="ns-rows">
          {ORDER.map((id) => {
            const c = byId.get(id);
            return (
              <li key={id} data-level={c?.level ?? 'idle'}>
                <div className="ns-row-head">
                  <i className="ns-cell" aria-hidden="true" data-level={c?.level ?? 'idle'} />
                  <h2>{TITLES[id]}</h2>
                  <span className="ns-state">
                    {c ? WORD[c.level] : running ? 'checking' : 'not checked'}
                  </span>
                  {c?.ms !== undefined && <span className="ns-ms">{c.ms} ms</span>}
                </div>
                <p className="ns-summary">{c ? c.summary : HINT[id]}</p>
                {c && c.facts.length > 0 && (
                  <dl className="ns-facts">
                    {c.facts.map(([k, v], i) => (
                      <div key={`${k}${i}`}>
                        <dt>{k}</dt>
                        <dd>{v}</dd>
                      </div>
                    ))}
                  </dl>
                )}
              </li>
            );
          })}
        </ol>

        <section className="ns-aside">
          <div>
            <h2>This tab&apos;s checks</h2>
            <div
              className="ns-history"
              role="img"
              aria-label={`${history.length} checks in this tab`}
            >
              {history.length === 0 ? (
                <span className="ns-none">None yet.</span>
              ) : (
                history.map((s) => (
                  <i
                    key={s.at}
                    data-level={s.level}
                    title={`${new Date(s.at).toLocaleTimeString()}: ${WORD[s.level]}`}
                  />
                ))
              )}
            </div>
          </div>
          <div>
            <h2>Who is asked</h2>
            <p>
              Each check sends a request from your browser, so these hosts see your IP address and
              the time: {[...new Set([location.host, ...hosts])].join(', ')}. Nothing about a note
              is sent.
            </p>
          </div>
          <div className="ns-tools">
            <button
              type="button"
              disabled={!report}
              onClick={() => {
                if (!report) return;
                void navigator.clipboard
                  ?.writeText(reportText(report, PROFILE))
                  .then(() => {
                    setCopied(true);
                    setTimeout(() => setCopied(false), 1400);
                  })
                  .catch(() => undefined);
              }}
            >
              {copied ? 'Report copied' : 'Copy report'}
            </button>
            <a
              href="https://github.com/nymiralabs/nymira/issues/new?title=Status%3A%20"
              target="_blank"
              rel="noopener noreferrer"
            >
              Report a problem
            </a>
          </div>
        </section>
      </main>
    </div>
  );
}
