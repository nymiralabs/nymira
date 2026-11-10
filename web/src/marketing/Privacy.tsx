// Where the web app and nymirad differ on what other parties learn, and what stays the same
// either way. Every line follows THREAT_MODEL.md or the README.
const DIFFERENCES: readonly [concern: string, web: string, daemon: string][] = [
  [
    'Your IP address',
    'RPC providers see it with every read and send, and the operator sees it with every request and close. The app cannot use Tor.',
    'Sends through Tor, and asks before any direct send.',
  ],
  [
    'When a close happens',
    'It goes out at once, so a close soon after a conversation is easier to tie to it.',
    'Waits a random one to twenty minutes first.',
  ],
  [
    'Attestation lookups',
    'Asks NEAR AI and Phala’s PCCS directly, so they see your address.',
    'Asks through Tor.',
  ],
  [
    'GPU evidence',
    'Not verified. The app checks the Intel TDX quote, the signed policy, the key binding and that the GPU evidence carries the nonce.',
    'Sent through the operator, which sees that a client asked and when. With --nras-direct, NVIDIA sees your IP instead.',
  ],
  [
    'The code you run',
    'The host decides which code you get. verify-web catches a change only if you run it.',
    'A release you download, with a signed SHA256SUMS.',
  ],
];

const SAME = [
  'Prompts reach the model as ciphertext. The operator sees the model, sizes and timing, not the text.',
  'NEAR AI sees token counts and timing and can tie one lease’s requests together. It does not see which note.',
  'A close shows the note ID, the total spend and the payout account on chain.',
  'You hide among the open notes, and on testnet there are few.',
  'An agent host and its model vendor see every question and answer.',
] as const;

export function Privacy() {
  return (
    <div className="nm-diff">
      <div className="nm-diff-head" aria-hidden="true">
        <span />
        <span>Web app</span>
        <span>nymirad</span>
      </div>
      {DIFFERENCES.map(([concern, web, daemon]) => (
        <div className="nm-diff-row" key={concern}>
          <h3>{concern}</h3>
          <p data-side="Web app">{web}</p>
          <p data-side="nymirad">{daemon}</p>
        </div>
      ))}
      <div className="nm-diff-same">
        <h3>Either way</h3>
        <ul>
          {SAME.map((text) => (
            <li key={text}>{text}</li>
          ))}
        </ul>
      </div>
    </div>
  );
}
