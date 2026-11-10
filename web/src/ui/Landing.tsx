import type { ReactNode } from 'react';
import { Site } from '../marketing/Site';
import { sentence } from '../privacy/strings';
import { PROFILE } from '../profile';

export function Notices() {
  return (
    <>
      <p className="notice" data-testid="tor-notice">
        This web app cannot use Tor. The operator sees your IP address and the time of each request.
        For IP protection use Tor Browser, or run <code>nymirad</code> on your own machine.
      </p>
      {PROFILE.network === 'testnet' && (
        <p className="notice" data-testid="testnet-notice">
          This is a testnet deployment with test funds only. The anonymity set is small and made of
          test notes, and the contract has had no independent review.
        </p>
      )}
    </>
  );
}

export function WhoSeesWhat() {
  return (
    <figure className="who" data-testid="who-sees-what">
      <svg viewBox="0 0 720 230" role="img" aria-labelledby="who-title who-desc">
        <title id="who-title">Who sees what</title>
        <desc id="who-desc" data-testid="who-desc">
          You hold the note secret and your prompts. {sentence('who_operator')}{' '}
          {sentence('who_model')} {sentence('who_chain')}
        </desc>
        <g className="box">
          <rect x="8" y="30" width="160" height="150" rx="8" />
          <rect x="190" y="30" width="160" height="150" rx="8" />
          <rect x="372" y="30" width="160" height="150" rx="8" />
          <rect x="554" y="30" width="158" height="150" rx="8" />
        </g>
        <g className="arrow">
          <path d="M168 105 H190" />
          <path d="M350 105 H372" />
          <path d="M532 105 H554" />
        </g>
        <g className="head">
          <text x="88" y="56">
            You
          </text>
          <text x="270" y="56">
            Operator
          </text>
          <text x="452" y="56">
            Model (NEAR AI TEE)
          </text>
          <text x="633" y="56">
            NEAR chain
          </text>
        </g>
        <g className="body">
          <text x="88" y="86">
            note secret
          </text>
          <text x="88" y="106">
            prompts
          </text>
          <text x="88" y="126">
            answers
          </text>
          <text x="270" y="86">
            your IP address
          </text>
          <text x="270" y="106">
            request times
          </text>
          <text x="270" y="126">
            proofs, charges
          </text>
          <text x="270" y="150">
            not your prompts
          </text>
          <text x="452" y="86">
            your prompts
          </text>
          <text x="452" y="106">
            token counts
          </text>
          <text x="452" y="126">
            not which note
          </text>
          <text x="633" y="86">
            your deposit
          </text>
          <text x="633" y="106">
            on close: note ID,
          </text>
          <text x="633" y="126">
            total spend,
          </text>
          <text x="633" y="146">
            payout account
          </text>
        </g>
      </svg>
    </figure>
  );
}

/** The landing page is the site itself; "Open web app" goes to the Fund screen. */
export function Landing({
  onOpen,
  onDocs,
  theme,
  onTheme,
  footerStatus,
}: {
  onOpen: () => void;
  onDocs: () => void;
  theme: 'dark' | 'light';
  onTheme: () => void;
  footerStatus: ReactNode;
}) {
  return (
    <Site
      theme={theme}
      onTheme={onTheme}
      onOpenApp={onOpen}
      onOpenDocs={onDocs}
      footerStatus={footerStatus}
    />
  );
}
