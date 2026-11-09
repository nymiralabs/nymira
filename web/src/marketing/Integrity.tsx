import { useRef } from 'react';
import { useStepper } from './Motion';

// The checks `nymirad verify-web` runs, in its order, with the values of the current testnet
// release. Steps and limits follow web/INTEGRITY.md; the values follow the README.
const STEPS: readonly [title: string, detail: string][] = [
  [
    'The signature',
    'manifest.json is checked against the release key compiled into nymirad. Key id 934C176AF34D794D.',
  ],
  [
    'The listing',
    'The SHA-256 of integrity/files.txt must equal bundle_hash: 3ee5ccb57cdaf2a1fa58cd914b6806909eac98c4dd20c013f6e0af5c926e045f.',
  ],
  ['Every file', 'Each file the host serves is hashed and compared with its line in the listing.'],
  [
    'The headers',
    'The six signed headers must be on /, the wallet frame and every asset. The GitHub mirror cannot send headers, so only the Cloudflare origin is checked.',
  ],
  [
    'Twice, two ways',
    'Once through Tor and once directly, on both origins. Any difference fails the check.',
  ],
];

const LIMITS = [
  'The browser does not verify its own bundle.',
  'A host that shows the verifier a clean copy and you another one defeats the check. Running it over Tor and directly makes that harder, not impossible.',
  'A signature says who vouched for a release. It does not say what the code does.',
  'A hash in a footer proves nothing. The check only counts when you run it.',
] as const;

export function Integrity() {
  const ref = useRef<HTMLOListElement>(null);
  const step = useStepper(ref, STEPS.length + 2, 1300);
  return (
    <div className="nm-verify">
      <ol className="nm-verify-steps" ref={ref}>
        {STEPS.map(([title, detail], i) => (
          <li key={title} data-s={i < step ? 'done' : i === step ? 'now' : 'next'}>
            <h3>{title}</h3>
            <p>{detail}</p>
          </li>
        ))}
      </ol>
      <div className="nm-verify-side">
        <div className="nm-verify-command nm-verify-command-art nm-release-art">
          <span>Run it yourself</span>
          <code>
            <i>$ </i>nymirad verify-web
          </code>
        </div>
        <div className="nm-verify-limits">
          <h3>What it cannot do</h3>
          <ul>
            {LIMITS.map((text) => (
              <li key={text}>{text}</li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}
