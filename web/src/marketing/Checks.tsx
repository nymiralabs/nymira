import { useRef } from 'react';
import { useStepper } from './Motion';

// What the client does before a prompt leaves the device. Every line follows
// web/src/chat/attest/verify.ts, web/src/chat/e2ee.ts, the note worker and the README.
const STEPS: readonly [title: string, detail: string][] = [
  [
    'Prove the note',
    'The device builds a zero-knowledge proof that a note can pay for the call. The web app does it in a worker, after the SHA-256 of the proving key matches.',
  ],
  [
    'Check the enclave',
    'The model’s Intel TDX quote must verify against Intel’s root, with TCB status UpToDate and the debug flag off.',
  ],
  [
    'Bind the key',
    'The quote’s report data holds the nonce the client sent and the signing address, so the key you encrypt to belongs to this quote and this request.',
  ],
  [
    'Match the policy',
    'The measured deployment must be one entry of the signed attestation policy built into the app.',
  ],
  [
    'Encrypt to the model',
    'The prompt is encrypted to the attested key. The operator sees the model, sizes and timing, not the text.',
  ],
];

const LIMITS = [
  'The web app does not check NVIDIA GPU evidence. nymirad does.',
  'The proving and verifying keys come from a setup run by the team alone. If every contribution was dishonest, proofs can be forged. The circuits come from zkapi and have had no independent review.',
  'A browser cannot read or pin the gateway’s TLS certificate.',
  'Intel’s collateral comes through Phala’s PCCS. It sees your address, and Intel’s chain signs the collateral.',
] as const;

export function Checks() {
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
        <div className="nm-verify-command nm-verify-command-art">
          <span>Proof circuits</span>
          <code>ethereum/zkapi · pinned commit</code>
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
