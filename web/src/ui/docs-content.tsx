import type { ReactNode } from 'react';

// The text of the documentation page. Every figure below was checked against the code:
// contracts/vault/src/lib.rs, web/src/profiles/testnet.ts, web/src/leave/guard.ts, crates/nymirad,
// crates/nymira-operator and mcp/src. Strings may hold `code` spans.

export type Block =
  | { t: 'p'; text: ReactNode }
  | { t: 'h3'; id?: string; text: ReactNode }
  | { t: 'list'; items: ReactNode[] }
  | { t: 'steps'; items: [string, ReactNode][] }
  | { t: 'table'; label: string; head: string[]; rows: ReactNode[][] }
  | { t: 'code'; label?: string; text: string }
  | { t: 'note'; title: string; text: ReactNode }
  | { t: 'diagram'; id: 'note-life' | 'parties' | 'exit' | 'lease' }
  | { t: 'calc' }
  | { t: 'calendar' }
  | { t: 'vault-compare' }
  | { t: 'ip' }
  | { t: 'compare'; items: { name: string; rows: [string, string][] }[] }
  | { t: 'faq'; items: [string, ReactNode][] };

export interface DocLink {
  label: string;
  href: string;
  /** Leaves the page: opens in a new tab with `rel="noopener noreferrer"`. */
  ext?: boolean;
}

export interface Section {
  id: string;
  title: string;
  lead?: ReactNode;
  blocks: Block[];
  /** Where to read more: the sources and the other pages that cover the same ground. */
  links?: DocLink[];
}

const REPO = 'https://github.com/nymiralabs/nymira';
const src = (path: string, label = path): DocLink => ({
  label,
  href: `${REPO}/blob/main/${path}`,
  ext: true,
});
const tree = (path: string, label = path): DocLink => ({
  label,
  href: `${REPO}/tree/main/${path}`,
  ext: true,
});
const to = (id: string, label: string): DocLink => ({ label, href: `#docs/${id}` });

export const PAGE: { title: string; lead: ReactNode; sections: Section[] } = {
  title: 'How Nymira works',
  lead: 'Nymira sells prepaid API credits on NEAR. You deposit USDC once and spend it in short leases, one per conversation. A zero-knowledge proof shows that a deposit can pay without saying which deposit it is. This page describes release 0.1.3 and the code it was built from.',
  sections: [
    {
      id: 'overview',
      title: 'What Nymira is',
      blocks: [
        {
          t: 'p',
          text: 'A vault contract on NEAR holds USDC. A model call is a lease: a short right to spend up to a fixed bound from one note. When the lease ends, only what it used is charged and the rest stays in the note.',
        },
        {
          t: 'p',
          text: "This deployment runs on NEAR testnet with Circle's test USDC, which has no value. The proof circuits have had no independent review, and the anonymity set is small and made of test notes. Read the limits before you trust any sentence on this page.",
        },
        { t: 'diagram', id: 'parties' },
        { t: 'diagram', id: 'lease' },
        {
          t: 'table',
          label: 'The four parties',
          head: ['Party', 'What it does'],
          rows: [
            [
              'You',
              "Hold the note secret. Your device builds the proofs and encrypts each prompt to the model's attested key. You use the web app or `nymirad`, a local gateway.",
            ],
            [
              'Operator',
              'Checks the proof, opens the lease, signs the charge and the next state. It runs on a home machine and can be offline.',
            ],
            [
              'Model provider',
              "NEAR AI Cloud. It answers inside a trusted execution environment. The operator reaches it with a key made for one lease and capped at that lease's bound.",
            ],
            [
              'Vault',
              'The NEAR contract `vault.nymira.testnet`. It holds the USDC, the tree of commitments, the spent nullifiers and the exit records.',
            ],
          ],
        },
      ],
    },
    {
      id: 'try',
      title: 'Try it',
      lead: 'You need a desktop browser, a NEAR testnet account and about 20 minutes. Everything here uses test USDC.',
      blocks: [
        {
          t: 'steps',
          items: [
            [
              'Connect a wallet',
              'Open the app, press Fund and connect Meteor or Intear Wallet on testnet. Meteor works on testnet. Intear signs in, but its approval window can fail to report back, and the app then reads the vault to see whether the transaction landed.',
            ],
            [
              'Get test USDC',
              'Ask the Circle faucet for USDC on Near Testnet. A request gives 20 USDC, and an address can ask once every 2 hours.',
            ],
            [
              'Register with the token',
              'The USDC contract only accepts accounts that paid for storage. Call `storage_deposit` on the token once for your account, with 0.00125 NEAR attached. The command is in the guide for judges.',
            ],
            [
              'Fund a note',
              'Pick the 1 USDC note. The app shows the price, 1.05 USDC with the fee. Set a passphrase, download the encrypted backup and send. The app reads the `deposit` event from your own receipts.',
            ],
            [
              'Chat',
              "Open Chat, pick the note and a model, and ask something. If the note is not in the tree yet, wait for the operator's next root update. The badge under the answer says who signed it.",
            ],
            [
              'Close',
              'Open Close and sign with your wallet. The vault pays the rest to the payout account. Check the payout in the explorer.',
            ],
          ],
        },
        {
          t: 'p',
          text: 'The operator runs on a home machine. If its status page does not answer, it is offline, so wait and try later. To try everything with no keys and no money, run the local stack with `make golden` or `make demo`.',
        },
      ],
      links: [
        { label: 'Open the app', href: '#fund' },
        { label: 'Circle faucet', href: 'https://faucet.circle.com', ext: true },
        src('JUDGES.md', 'The full guide for judges'),
        {
          label: 'Operator status',
          href: 'https://nymira-operator.tail0484c4.ts.net/v1/status',
          ext: true,
        },
        { label: 'Explorer', href: 'https://testnet.nearblocks.io', ext: true },
      ],
    },
    {
      id: 'judges',
      title: 'For judges',
      lead: 'What was built, where it touches NEAR, how to check it, and what is not built. The guide in the repository is the 20-minute path.',
      blocks: [
        {
          t: 'table',
          label: 'What was built, by track',
          head: ['Track', 'What to look at', 'Where to check it'],
          rows: [
            [
              'Privacy',
              "Prompts are encrypted to a key the model's attestation binds to a measured deployment. A zero-knowledge proof pays for a lease without naming the note. `nymirad` sends through Tor.",
              'The sections "What is checked, and where" and "Who sees what", and the threat model.',
            ],
            [
              'Agents',
              'Budget notes cap what one agent can spend. A signed policy sets models, limits and approvals. An MCP server and an OpenAI-compatible gateway connect tools. Funding from other chains goes through NEAR Intents (quote only on testnet).',
              'The section "Budget notes for agents" and the Agents screen.',
            ],
            [
              'Security',
              'The vault is a reproducible build whose code hash and verifying keys are pinned and checked at load. The proving keys come from our own ceremony. Releases are signed. The limits are written down.',
              'The sections "The vault contract", "Verify it yourself" and "Limits".',
            ],
          ],
        },
        { t: 'h3', text: 'Not built' },
        {
          t: 'list',
          items: [
            'Paying sellers (x402) and a mainnet vault. No mainnet vault is pinned in this build.',
            'An independent review of the circuits or the vault, and an outside rebuild of a release.',
            "A trusted setup beyond the team's own: the ceremony had three contributions and a NEAR beacon, all by us.",
          ],
        },
        {
          t: 'note',
          title: 'Testnet with test funds',
          text: "Everything runs on NEAR testnet with Circle's test USDC. The operator runs on a home machine and can be offline. Use the status page to see what answers right now.",
        },
      ],
      links: [
        src('JUDGES.md', 'The guide for judges'),
        src('README.md', 'README'),
        src('THREAT_MODEL.md', 'Threat model'),
        { label: 'Status', href: '#status' },
      ],
    },
    {
      id: 'note-life',
      title: 'The life of a note',
      lead: 'A note is a balance only you can spend. On chain it shows as a commitment, a deposit amount, an expiry and a payout account.',
      blocks: [
        { t: 'diagram', id: 'note-life' },
        {
          t: 'steps',
          items: [
            [
              'Deposit',
              'Send USDC to the vault with `ft_transfer_call`. The amount is a denomination (1 or 5 USDC) plus the 0.05 USDC note fee, which is not refunded. The message carries the commitment of your note secret and, optionally, a payout account. The default payout account is the sender. The vault gives the note the next ID and sets its expiry to the end of the UTC day 30 days later. A wrong amount, a repeated commitment or a full cap returns the whole amount.',
            ],
            [
              'Lease',
              "To chat, your device proves that the note can pay a lease up to a bound: 0.10 USDC, or 0.50 USDC for a long job. The operator checks the proof and a signed status, then opens the lease. A request proof is valid for 300 seconds. Prompts travel encrypted to the model's attested key.",
            ],
            [
              'Settle',
              'When the lease ends, the operator reads the usage and signs a charge: usage plus 3% plus a slot fee of 0.1% of the bound per minute held, rounded up to 0.001 USDC. The charge is at least 0.002 USDC and never above the bound. The operator signs the next state only after a second machine has stored the record.',
            ],
            [
              'Close',
              'Your device asks the operator for a clearance, then makes a close proof valid for 3 hours and calls `mutual_close`. The vault pays the balance to the payout account and books what you spent to the treasury. Sent from your own wallet, a close has no fee. If the operator sends it for you, the operator keeps 0.03 USDC. `nymirad` waits 1 to 20 minutes before it asks for the clearance. The web app does not wait.',
            ],
            [
              'Exit',
              "Without the operator, call `request_exit` with a proof of your latest state. After a 7-day window (604800 seconds) anyone can call `finalize_exit`, and the vault pays the balance to the payout account. During the window the operator's watcher can answer with a later state. That is how an exit from an old state is defeated.",
            ],
            [
              'Expiry',
              'A note expires 30 days after the deposit and can still close or exit. Seven days after the later of the expiry and the last exit activity, the treasury can take the deposit with `claim_expired`, unless an exit is pending. The expiry guard acts earlier: notices at 14, 7 and 2 days left, a close attempt at 5 days and, if the close failed, an exit at 3 days.',
            ],
          ],
        },
        { t: 'h3', text: 'Three ways out' },
        {
          t: 'compare',
          items: [
            {
              name: 'Close',
              rows: [
                ['Needs', 'The operator online, for a clearance.'],
                [
                  'Time',
                  'Minutes. `nymirad` waits 1 to 20 minutes first; the web app does not wait.',
                ],
                ['Cost', 'No fee from your wallet. If the operator sends it, 0.03 USDC.'],
                ['Risk', 'None beyond the operator being offline.'],
              ],
            },
            {
              name: 'Exit',
              rows: [
                ['Needs', 'A proof of your latest state. No operator.'],
                ['Time', '7 days (604800 seconds), then anyone can finalize.'],
                ['Cost', 'Gas. The wallet account must hold about 0.02 NEAR.'],
                [
                  'Risk',
                  'From an old state, the operator can answer with a later one and keep what it charged. Keep your backup.',
                ],
              ],
            },
            {
              name: 'Expiry',
              rows: [
                ['Needs', 'Nothing. It happens by itself.'],
                ['Time', '30 days after the deposit, then 7 more.'],
                ['Cost', 'The whole remaining balance goes to the treasury.'],
                [
                  'Risk',
                  'You lose the balance if you do nothing. The expiry guard tries a close at 5 days left and an exit at 3.',
                ],
              ],
            },
          ],
        },
        { t: 'h3', text: 'The dates of your note' },
        {
          t: 'p',
          text: 'Pick the day you deposited. The dates follow the numbers above: the expiry at the end of day 30 (UTC), the expiry guard at 14, 7, 5, 3 and 2 days left, and the treasury 7 days after the expiry.',
        },
        { t: 'calendar' },
        { t: 'h3', text: 'The exit window' },
        {
          t: 'p',
          text: 'An exit needs no operator and no clearance. Anyone can call `finalize_exit` once the 7 days are over, so a vanished operator cannot hold your money.',
        },
        { t: 'diagram', id: 'exit' },
        { t: 'h3', text: 'What a lease costs' },
        {
          t: 'p',
          text: "The charge is the usage at the pinned prices plus 3%, plus a slot fee of 0.1% of the bound for each minute the lease is held, rounded up to 0.001 USDC. It is at least 0.002 USDC and never above the bound. The calculator below runs the app's own function with this build's vault parameters. A short chat usually pays the minimum.",
        },
        { t: 'calc' },
        {
          t: 'note',
          title: 'Keep the latest state',
          text: 'If you lose the latest state of a note (the browser storage and the backup), the operator can answer your exit with a later request and keep what that request charged, up to the whole remaining balance. Keep an encrypted backup.',
        },
      ],
    },
    {
      id: 'words',
      title: 'Words',
      blocks: [
        {
          t: 'table',
          label: 'Terms used in the app and the contract',
          head: ['Term', 'Meaning'],
          rows: [
            [
              'Note',
              'A private balance. On chain only its commitment, deposit amount, expiry and payout account show.',
            ],
            [
              'Commitment',
              'The public hash of the note secret, stored at deposit. The vault accepts each commitment once.',
            ],
            ['Denomination', 'The fixed deposit size: 1 or 5 USDC.'],
            ['Note fee', '0.05 USDC added to every deposit. It is not refunded.'],
            [
              'Lease',
              'A short right to spend up to a bound from one note. It lasts at most 600 seconds.',
            ],
            ['Bound class', 'A fixed lease bound the vault publishes: 0.10 or 0.50 USDC.'],
            ['Request proof', 'A Groth16 proof that a state can pay a lease up to a bound.'],
            [
              'Nullifier',
              'A one-time value revealed when a state is spent. The vault keeps the set of used ones.',
            ],
            [
              'Clearance',
              "The operator's signature that a note has no open lease. Signing it spends the current nullifier for good.",
            ],
            [
              'Payout account',
              'The only account a note can be paid to. It is fixed at deposit, so a stolen secret cannot redirect the money.',
            ],
            ['Exit window', 'The 7 days between `request_exit` and the earliest `finalize_exit`.'],
            ['Concession', 'What an exit may lose to a lease that was in flight. It only grows.'],
            [
              'Treasury',
              'One booked balance for fees, spent amounts, swept deposits and dust. Only `claim_treasury` pays it out.',
            ],
            [
              'TVL cap',
              'The most the open deposits may add up to: 100 USDC, counting each deposit with its fee.',
            ],
            [
              'Set size',
              'How many notes could have paid for a lease. The app counts them from the vault and shows the figure as an upper bound.',
            ],
          ],
        },
      ],
    },
    {
      id: 'privacy',
      title: 'Who sees what',
      lead: 'The design keeps deposits and calls apart. How well it does that depends on how many notes are open and on whether your address is hidden. The web app cannot use Tor.',
      blocks: [
        { t: 'h3', text: 'Your IP address' },
        { t: 'ip' },
        { t: 'h3', text: 'What each party sees' },
        {
          t: 'table',
          label: 'What each party sees',
          head: ['Party', 'Sees', 'Does not see'],
          rows: [
            [
              'Operator',
              'The lease proof, the nullifier, the bound class, request times, the charge, the model name, message sizes and usage. Its public entry point, a Tailscale Funnel relay, sees your IP address and the time of every connection. The operator code reads no address and keeps none. Over Tor, `nymirad` shows a Tor exit instead.',
              'Your prompt text. Every field the model reads is encrypted to its attested key. The operator also does not see which note a lease belongs to.',
            ],
            [
              'Operator, on a close',
              'The ownership proof, your IP address and the time. `nymirad` waits 1 to 20 minutes first. The web app does not, so a close soon after a conversation is easier to tie to it.',
              '',
            ],
            [
              'NEAR AI',
              "The model, token counts, timing and the usage of the key made for one lease. It can tie the requests of one lease together. The web app also asks it for attestation reports directly, so it sees the visitor's address.",
              'Which note the lease belongs to. Your prompt, except as ciphertext for the attested key.',
            ],
            [
              'RPC provider',
              'Your IP address on every read and send. With a deposit: the account, the transaction hash (so the note ID, the commitment and the payout account) and the time. On leaving: the note ID and the payout account.',
              '',
            ],
            [
              'Web host',
              'Your IP address and which files you ask for. It decides which code you get. A compromised host could serve a different app.',
              '',
            ],
            [
              '1Click',
              'The IP address and the asset pair of whoever asks for a quote. The web app asks only after you press "Show sources" and sends no note data.',
              'Anything on testnet beyond quotes: no cross-chain payment exists there.',
            ],
            ['Circle faucet', 'The account that asks for test USDC.', ''],
            [
              'Attestation services',
              "The address that asks and the time, for NEAR AI's endpoint and the Phala PCCS. `nymirad` asks through Tor.",
              '',
            ],
            [
              'Chain',
              'The deposit. On a close or exit: the note ID, the total spend, the payout account and the account that sends the transaction.',
              '',
            ],
          ],
        },
        {
          t: 'p',
          text: "A close sent from the web app puts the wallet account on chain next to the note ID. A connected account other than the funding account keeps the two apart, and the app does not create one. `nymirad` sends from a submitter account instead, one per family of notes, and it sends over Tor on its own circuit. When Tor is down it asks in fixed words before any direct send. The clearance request and the operator's close intake never go direct.",
        },
      ],
    },
    {
      id: 'web-app',
      title: 'The web app',
      lead: 'The app runs in your browser. It talks to a NEAR wallet in a sandboxed frame, to an RPC provider, to the operator and to NEAR AI for attestation.',
      blocks: [
        { t: 'h3', text: 'Chat' },
        {
          t: 'p',
          text: 'One conversation is one lease on one note. Before anything is encrypted, the app checks the model\'s attestation. It then reads the signed status nonce, builds the tree at one final block and makes the proof. The lease bound is 0.10 USDC, or 0.50 USDC with "Long job". After the lease ends, the badge reads "model-signed response", "gateway-signed response" or "not verified".',
        },
        {
          t: 'table',
          label: 'Models and prices pinned in the app',
          head: ['Model', 'Input per million tokens', 'Output per million tokens'],
          rows: [
            ['`z-ai/glm-5.3-flash` (default)', '0.08 USD', '0.50 USD'],
            ['`Qwen/Qwen3.6-35B-A3B-FP8`', '0.17 USD', '1.10 USD'],
            ['`Qwen/Qwen3.8-27B`', '0.04 USD', '1.35 USD'],
            ['`Qwen/Qwen3-VL-30B-A3B-Instruct`', '0.15 USD', '0.55 USD'],
          ],
        },
        { t: 'h3', text: 'Fund' },
        {
          t: 'p',
          text: 'The order is fixed. The app seals the note secret and stores it, downloads a backup and test-restores it, checks the vault against its pins, then asks the wallet to send `ft_transfer_call`. It reads acceptance from the `deposit` event in your own receipts. If the wallet never answers, the app reads the vault: it calls a refusal "rejected" only after 20 seconds and stops waiting after 180 seconds. A payout account other than the sender needs a fingerprint confirmation.',
        },
        { t: 'h3', text: 'Notes' },
        {
          t: 'p',
          text: 'The screen lists your notes, downloads and restores the backup, and finds a note again from its transaction hash. A restore keeps a newer state that the browser already holds.',
        },
        { t: 'h3', text: 'Close' },
        {
          t: 'p',
          text: 'A close sends the ownership proof for a clearance, makes the close proof and calls `mutual_close` from your wallet, or through the operator, which keeps the close fee. An exit needs no clearance. The wallet account must hold at least 0.02 NEAR. The expiry guard is on by default and runs while the app is open, unlocked and connected to a wallet. It can be switched off, and closing at the end of a session is off unless you turn it on.',
        },
        { t: 'h3', text: 'Agents' },
        {
          t: 'p',
          text: 'The screen builds `policy.yaml` from form fields and signs it with your wallet (NEP-413). It also signs approvals and prints the `nymirad` commands. It creates no note, shows no token and sends nothing to `nymirad`. `require_tee: true` is the only value it offers. It is hidden when no wallet can sign messages.',
        },
        { t: 'h3', text: 'What the browser stores' },
        {
          t: 'list',
          items: [
            "IndexedDB `nymira` holds each note's public record and one sealed payload. The payload is AES-256-GCM, bound to its note.",
            'The key comes from Argon2id (64 MiB, 3 passes, 1 lane, passphrase of at least 12 characters) or from a WebAuthn PRF output. It is non-extractable and lives in memory. The passphrase you typed also stays in memory until you lock or reload, so the backup step can reuse it if you tick the box. "Stay unlocked in this tab" keeps it in a second IndexedDB until the tab closes.',
            'The wallet frame keeps nothing on the device. `localStorage` holds only the theme.',
            'A web backup is JSON: Argon2id and AES-256-GCM, file extension `.nymira-backup`. The app asks for one after a close or a finalized exit. It writes to a file you chose, or prompts for a download after every retire above 0.50 USDC and after every tenth lease below it.',
          ],
        },
      ],
    },
    {
      id: 'attestation',
      title: 'What is checked, and where',
      lead: "Prompts are encrypted to a key that the model's attestation report binds to a measured deployment. The web app and `nymirad` check that report differently.",
      blocks: [
        {
          t: 'list',
          items: [
            'The signed policy has version 5 or higher and an Ed25519 signature from a pinned signer. It accepts only the TCB status `UpToDate`. The site may serve a newer policy beside the release; the page uses it only when the pinned signer signed it and its version is higher.',
            "The Intel TDX quote is verified in the browser with `dcap-qvl` compiled to WebAssembly. Collateral comes from the Phala PCCS, because Intel's own service sends no CORS headers.",
            'The debug flag is off. The nonce and the signing address are bound in the report data. The event log replays to RTMR3, and the compose file hashes to `mr_config_id`.',
            'The measured deployment is in the signed policy. For the model, the chat key equals the attested signer.',
          ],
        },
        {
          t: 'p',
          text: "The web app does not check NVIDIA GPU evidence. It only confirms that the payload carries the nonce. It cannot read or pin the gateway's TLS certificate either. `nymirad` also checks NVIDIA's verdict: the ES384 signature, the issuer, the times, the nonce and a certificate chain that reaches a pinned certificate. It does not check per-device tokens or revocation.",
        },
        {
          t: 'p',
          text: 'A signed answer comes in two kinds. A `provider_tee` signature covers the model name and the hashes of the request and the response, and its signer is the attested model key. A `gateway` signature covers the two hashes, and its signer is the gateway key. Neither proves which server answered. Streamed answers are expected to be gateway-signed.',
        },
        {
          t: 'note',
          title: 'Pins not yet verified',
          text: "Policy version 6 pins exact compose hashes for the model and accepts any compose hash for the gateway. It pins two NVIDIA intermediate certificates and marks both as not verified against NVIDIA's documentation. The NVIDIA root is not pinned.",
        },
      ],
    },
    {
      id: 'vault',
      title: 'The vault contract',
      lead: 'The vault is `vault.nymira.testnet`. Its code hash is `71QXP9FT4eiRcaqjZtsHi7mXMCAexXjCFgQ338p8tur4`, built reproducibly from the tag `vault-testnet-1`.',
      blocks: [
        { t: 'h3', text: 'Compare with the chain' },
        { t: 'vault-compare' },
        { t: 'h3', text: 'Configuration' },
        {
          t: 'table',
          label: 'Vault configuration on testnet',
          head: ['Setting', 'Value'],
          rows: [
            ['Denominations', '1 and 5 USDC'],
            ['Bound classes', '0.10 and 0.50 USDC'],
            ['Charge step, minimum charge', '0.001 and 0.002 USDC'],
            [
              'Note fee, close fee',
              '0.05 USDC (every deposit) and 0.03 USDC (only when the operator sends the close)',
            ],
            ['Exit delay, note lifetime', '604800 seconds (7 days) and 2592000 seconds (30 days)'],
            ['Cap on open deposits', '100 USDC'],
            [
              'Token',
              "Circle's testnet USDC, account `3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af`",
            ],
          ],
        },
        {
          t: 'table',
          label: 'Entry points by caller',
          head: ['Caller', 'Methods'],
          rows: [
            [
              'Anyone',
              '`ft_on_transfer`, `sync_root`, `mutual_close`, `request_exit`, `defeat_exit`, `raise_concession`, `finalize_exit`, `claim_expired`, `claim_expired_batch`, `retire_closed_leaf`, `claim_payout`, `claim_treasury`',
            ],
            ['Operator account', '`heartbeat`. It opens no exit and changes no key.'],
            [
              'Vault owner',
              '`propose_tvl_cap`, `apply_tvl_cap`, `propose_params`, `apply_params`. Each applies 48 hours after its proposal at the earliest, and only a full-access key can call them.',
            ],
            [
              'Views',
              '`config`, `note_records`, `get_leaves`, `exit_requests`, `current_root`, `note_status`, `vk_hashes`, `treasury_balance`, `pending_payout` and others. Pages stop at 100 rows.',
            ],
          ],
        },
        {
          t: 'table',
          label: 'Keys and what each can do',
          head: ['Key', 'Can do'],
          rows: [
            [
              'Note secret (yours)',
              'Builds proofs for the note. The payout account is fixed, so it cannot redirect the money.',
            ],
            [
              'State key `operator_pk`',
              'Signs note states. Set when the vault is created, with no setter.',
            ],
            ['Clearance key `clearance_pk`', 'Signs clearances. Must differ from the state key.'],
            [
              'Operator account',
              'Sends `heartbeat` and the closes it signs, and earns the close fee.',
            ],
            [
              'Vault owner',
              'Changes only the TVL cap and the lease parameters, 48 hours after a proposal.',
            ],
            [
              'Vault account access key',
              'One full-access key, held by the team. It can replace the vault code. Clients detect a change but cannot stop it.',
            ],
            [
              'Submitter (`nymirad`)',
              'A function-call key limited to `request_exit`, `finalize_exit`, `mutual_close` and `claim_payout`.',
            ],
          ],
        },
        {
          t: 'p',
          text: 'The vault is created only if the keccak-256 of both verifying keys equals the hashes compiled into the contract. The keys come from a three-contribution ceremony run by the team alone. The setup is sound if at least one contribution kept its secret, which nobody outside can check.',
        },
      ],
    },
    {
      id: 'nymirad',
      title: 'nymirad',
      lead: '`nymirad` is a local gateway on loopback port 8788. It speaks the OpenAI API, holds your notes sealed on disk, makes the proofs and runs the lease lifecycle. It sends through Tor and asks before it goes direct.',
      blocks: [
        { t: 'h3', text: 'Install and check' },
        {
          t: 'code',
          label: 'Verify a release',
          text: 'minisign -V -p web/pins/release-signing.pub -m SHA256SUMS\nshasum -a 256 -c SHA256SUMS',
        },
        {
          t: 'p',
          text: 'Each release lists its files in a signed `SHA256SUMS`. `nymirad` runs only a Tor executable whose SHA-256 is compiled into the build. Three packages are pinned: Homebrew `tor` 0.4.9.15 for macOS arm64, and Debian 12 `tor` 0.4.9.11 for amd64 and arm64 {{new}}. Pass the loopback address of your own Tor with `nymirad init ... --socks HOST:PORT` if you run one. Then `nymirad` does not check the Tor executable.',
        },
        {
          t: 'table',
          label: 'Where to get what',
          head: ['What', 'Where'],
          rows: [
            [
              'Web app',
              'https://nymira.pages.dev, mirrored at https://nymiralabs.github.io/nymira/. The mirror cannot send the security headers, so the app refuses to run there.',
            ],
            [
              '`nymirad`',
              'The releases page of the repository. Release 0.1.3 has a Linux x86_64 binary and a macOS arm64 binary, the signed `SHA256SUMS` and `attestation-policy.json`. It includes budget notes for agents (`nymirad agent`), the set size option and the Debian Tor pins.',
            ],
            [
              'Proving keys',
              'Release `final` of the nymira-ceremony repository: `request.pk` and `withdrawal.pk`. `nymirad init --setup-dir DIR` checks their hashes. The web app serves them from its own signed bundle.',
            ],
            [
              'Vault source',
              'The tag `vault-testnet-1` (commit `18107d876dd9d3912796f4b002e9a006c74b1152`), built reproducibly with `scripts/reproducible-smoke.sh`.',
            ],
            [
              'MCP server',
              'No release yet. Build it from a checkout: `cd mcp && npm ci && npm run build`.',
            ],
          ],
        },
        {
          t: 'code',
          label: 'Set up and run',
          text: 'nymirad init --rpc URL --operator URL --account ACCOUNT --setup-dir DIR --place DIR\nnymirad serve',
        },
        { t: 'h3', text: 'Commands' },
        {
          t: 'table',
          label: 'nymirad commands',
          head: ['Command', 'What it does'],
          rows: [
            [
              '`init`',
              'Writes `config.json`. It dies unless you choose a backup place, `--local-only` or `--no-backup`.',
            ],
            [
              '`serve [--nras-direct]`',
              'Runs the gateway. `--nras-direct` sends GPU evidence straight to NVIDIA and shows your IP address. It is never the default.',
            ],
            [
              '`status`',
              'Shows Tor, the operator, the vault pins, notes, agents, set size and the GPU chain line.',
            ],
            [
              '`deposit`, `recover`',
              'Fund a note and find notes again after a crash. Cross-chain funding through 1Click exists on mainnet only.',
            ],
            [
              '`close`, `exit`, `finalize`, `claim-payout`',
              'Leave a note. `close` accepts `--delay SECS` and `--via-operator`.',
            ],
            [
              '`backup`, `restore`, `restore-secret`',
              'Write a backup, restore one into an empty home, or rebuild one note from its secret. A note rebuilt that way can only be exited.',
            ],
            [
              '`submitter add|list|assign`',
              'Manage submitter accounts and the family each note uses.',
            ],
            [
              '`verify-web`',
              'Compares the web hosts with the signed manifest, over Tor and then directly after a typed yes.',
            ],
            [
              '`agent create|list|show|disable|policy|approvals|approve|revoke|ack-warning`',
              'Manage budget notes for agents.',
            ],
            [
              '`agent move`, `agent import-state`',
              "Move a budget note to the agent's machine and bring its state back. Opt-in.",
            ],
          ],
        },
        { t: 'h3', text: 'What goes through Tor' },
        {
          t: 'p',
          text: 'RPC reads, operator calls, the lease proof and the NVIDIA relay call use Tor. If Tor is not ready, nothing is sent. Chain transactions go through Tor on their own circuit. Three things ask first. A close or exit when Tor is down asks, in fixed words, before it sends directly. A cross-chain funding attempt asks before it contacts 1Click outside Tor. `verify-web` asks before its direct fetch. The expiry guard never goes direct. It waits.',
        },
        {
          t: 'p',
          text: 'A backup is an age file. With a passphrase, the age key comes from Argon2id (64 MiB, 3 passes, 1 lane) and the passphrase needs at least 12 characters. Whoever holds a backup and its key can spend your notes.',
        },
      ],
    },
    {
      id: 'agents',
      title: 'Budget notes for agents',
      lead: 'A budget note is a note you fund for one agent. Its payout account is your own, fixed at deposit. The agent gets a token and can spend only that note.',
      blocks: [
        {
          t: 'code',
          label: 'Create and sign',
          text: 'nymirad agent create --name worker --transport tcp --key-file KEY --amount 1000000\nnymirad agent policy --name worker --file worker-policy.signed.json',
        },
        {
          t: 'p',
          text: 'Agent tokens start with `nyma_` and your owner token with `nymo_`. A `unix` token works on the agent socket only. A `tcp` token works on loopback port 8788 only. An agent token reaches `POST /v1/chat/completions`, `GET /v1/models`, `GET /v1/nymira/balance` and `POST /v1/nymira/quote`, and nothing else.',
        },
        { t: 'h3', text: 'The signed policy' },
        {
          t: 'table',
          label: 'Policy keys',
          head: ['Key', 'Meaning'],
          rows: [
            ['`allowed_models`', 'The pinned models the agent may use. Each appears once.'],
            ['`max_lease_usd`', 'The largest lease bound. At least 0.10.'],
            ['`max_call_usd`', 'The most one call may cost. `nymirad` lowers `max_tokens` to fit.'],
            [
              '`max_daily_usd`',
              "The most one UTC day may cost: settled leases plus the open lease's bound.",
            ],
            [
              '`confirm_above_usd`',
              'A call whose worst case is above this is held for your approval.',
            ],
            ['`rate_limit_per_min`', 'Requests per minute. A whole number above 0.'],
            [
              '`require_tee`',
              'Omitted means true. An answer is released only after its signature is checked.',
            ],
            ['`allowed_origins`', 'Empty for now. A non-empty list is refused.'],
          ],
        },
        {
          t: 'p',
          text: "You sign the policy with your NEAR account (NEP-413). The message is JSON with the kind `nymira-policy`, a version and the YAML text. The recipient is `nymirad:<agent name>:<note commitment>`. `agent policy` checks the signature and reads your account's access keys over every configured RPC. The key must be full access. With no valid signed policy, every agent request gets 402 `policy_missing` before anything leaves the machine.",
        },
        { t: 'h3', text: 'The order of checks' },
        {
          t: 'list',
          items: [
            'The token is known, enabled and on the right transport, and the stored policy verifies.',
            'The note is not moved, revoking, closing, exiting or in its last 5 days.',
            'No charge warning is active.',
            'The model is in `allowed_models`.',
            'The rate limit, then the call cost, then the day total.',
            'Your approval, if the worst case is above `confirm_above_usd`.',
            "The note's balance and the lease bound.",
          ],
        },
        {
          t: 'table',
          label: 'Errors an agent can get',
          head: ['Status', 'Code', 'Meaning'],
          rows: [
            ['401', '`token_disabled`', 'You disabled the token.'],
            ['402', '`policy_missing`', 'No valid signed policy.'],
            ['402', '`model_not_allowed`', 'The model is not in `allowed_models`.'],
            ['402', '`call_limit`, `daily_limit`', 'The call or the day would pass its limit.'],
            ['402', '`approval_required`', 'Held for you. The response carries the approval ID.'],
            [
              '402',
              '`budget_exhausted`, `no_note_can_cover_request`',
              'The note cannot cover the lease or this request.',
            ],
            [
              '402',
              '`note_expiring`, `note_moved`',
              "The note is in its last 5 days or leaving, or it moved to the agent's machine.",
            ],
            [
              '429',
              '`policy_rate_limit`, `operator_capacity`, `provider_rate_limit`',
              'A limit was reached. `Retry-After` is given.',
            ],
            ['409', '`busy`', "The note's lease already has 2 requests in flight."],
            [
              '502',
              '`not_verified`',
              '`require_tee` withheld an answer whose signature did not verify.',
            ],
            [
              '503',
              '`unavailable`',
              'The detail names why, such as `tor_down` or `operator_emergency`.',
            ],
          ],
        },
        { t: 'h3', text: 'What the chain enforces and what `nymirad` enforces' },
        {
          t: 'p',
          text: "The vault enforces the ceiling: an agent can spend at most the balance of its note, and a close can only pay your payout account. That rests on the vault matching the client's pins, and the team can still update the vault code. The policy is as strong as the machine that runs `nymirad`. That machine is yours by default. After `agent move` it is the agent's, the policy is advisory and revoking is not guaranteed. There is no remote rate limit and no remote kill switch.",
        },
        { t: 'h3', text: 'MCP server' },
        {
          t: 'p',
          text: 'The server in `mcp/` speaks stdio and gives an agent three tools. It adds no authority. It holds the agent token it reads from a private file and forwards to your `nymirad`. The agent host and its model vendor see every question and answer that passes through `nymira_ask`.',
        },
        {
          t: 'table',
          label: 'MCP tools',
          head: ['Tool', 'Does'],
          rows: [
            [
              '`nymira_balance`',
              "Returns the balance, the spendable amount, the lease bound, today's spend, the note status and the limits.",
            ],
            [
              '`nymira_quote`',
              'Takes a model and `max_usd`, plus messages or an input token count. Returns allow, needs approval or deny, and moves nothing.',
            ],
            [
              '`nymira_ask`',
              'Takes a model, messages and `max_usd`. It is the only tool that spends. It cannot approve a call.',
            ],
          ],
        },
      ],
    },
    {
      id: 'verify',
      title: 'Verify it yourself',
      lead: 'Each step below uses commands that exist in the repository.',
      blocks: [
        {
          t: 'code',
          label: 'The signed release',
          text: 'minisign -V -p web/pins/release-signing.pub -m web/releases/testnet-0.1.3/manifest.json\nnode web/scripts/integrity.mjs fingerprint web/pins/release-signing.pub',
        },
        {
          t: 'p',
          text: 'The key id is `934C176AF34D794D`. The release signs a listing of every file and the headers the site must send. Compare the live host with it:',
        },
        {
          t: 'code',
          label: 'The live host',
          text: 'curl -s https://nymira.pages.dev/integrity/files.txt | shasum -a 256\ncurl -s https://nymira.pages.dev/integrity/headers.txt | shasum -a 256',
        },
        {
          t: 'p',
          text: 'The two hashes must equal the `bundle_hash` and the `header_set_hash` in the signed `manifest.json` of the release you loaded. The footer of the app shows its version and bundle hash, and `https://nymira.pages.dev/manifest.json` serves the same manifest. `nymirad verify-web` runs the same check over Tor.',
        },
        {
          t: 'code',
          label: 'The vault',
          text: `curl -s -H 'content-type: application/json' https://test.rpc.fastnear.com \\\n  -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"view_account","finality":"final","account_id":"vault.nymira.testnet"}}'`,
        },
        {
          t: 'p',
          text: 'The vault has one access key and it has full access. Check it with `view_access_key_list`. To rebuild the web release, run `web/scripts/rebuild-in-container.sh web-v0.1.3 testnet 0.1.3` (it needs Docker). To try everything with no keys and no money, run `make golden` or `make demo`.',
        },
      ],
    },
    {
      id: 'faq',
      title: 'Questions',
      blocks: [
        {
          t: 'faq',
          items: [
            [
              'Does Nymira see my prompts?',
              "No. Every field the model reads is encrypted to the model's attested key. The operator sees the model name, message sizes, timing and usage.",
            ],
            [
              'Can the operator take my money?',
              'Not to another account: a close or exit pays only the payout account fixed at deposit. While you keep your latest state, the operator can deduct at most the bound of the lease in flight. If you lose it, an exit from an older state can be defeated and you can lose the remaining balance.',
            ],
            [
              'What if the operator disappears?',
              'Leave with `request_exit` and, after 7 days, `finalize_exit`. The vault does not need the operator. If the operator and every watcher stay offline for the whole window, you leave with an older, higher balance than the real one. That loss falls on the operator, and the 100 USDC cap bounds it.',
            ],
            [
              'What if I lose my browser data?',
              'Restore the encrypted backup file. With only the note secret, `nymirad restore-secret` rebuilds a note that can be exited, not spent, and an exit from an old state can be defeated.',
            ],
            [
              'Is it anonymous?',
              "It keeps deposits and calls apart. It is not a promise. The anonymity set is small and made of test notes, the web app cannot use Tor, and timing and usage can still link a user's requests.",
            ],
            [
              'Which wallets work?',
              'Meteor and Intear Wallet on testnet. Other wallets may work and are not promised.',
            ],
            [
              'Has it been audited?',
              'No. The proof circuits and the vault have had no independent review.',
            ],
            [
              'Is there a mainnet?',
              'No. This build runs on NEAR testnet with test USDC, and no mainnet vault is pinned in it.',
            ],
          ],
        },
      ],
    },
    {
      id: 'develop',
      title: 'Build and contribute',
      lead: 'The code is open under the Apache License 2.0 or the MIT license, at your option. The proof circuits come from zkapi, vendored at a pinned commit and credited in `NOTICE`.',
      blocks: [
        {
          t: 'table',
          label: 'The repository',
          head: ['Path', 'What it holds'],
          rows: [
            ['`contracts/vault`', 'The NEAR vault contract.'],
            [
              '`crates/nymira-operator`',
              'The operator: admission, leases, charges, the signer, the closer and the watcher.',
            ],
            ['`crates/nymirad`', 'The local gateway and CLI.'],
            [
              '`crates/nymira-near-codec`, `crates/nymira-verifier`',
              'The shared NEAR codec and the on-chain proof verifier.',
            ],
            ['`web`', 'The web app, the landing page and this documentation.'],
            ['`mcp`', 'The MCP server for agents.'],
            ['`vendor/zkapi`', 'The zkapi circuits, vendored at a pinned commit.'],
          ],
        },
        {
          t: 'code',
          label: 'Build and test',
          text: 'cargo check --target wasm32-unknown-unknown --locked -p nymira-vault -p nymira-near-codec -p nymira-verifier\ncargo test --workspace --locked\nweb/scripts/build-wasm.sh && (cd web && npm ci --ignore-scripts)',
        },
        {
          t: 'p',
          text: 'The toolchain is pinned in `rust-toolchain.toml` (rustc 1.99.0, `wasm32-unknown-unknown`). Install cargo-near 0.22.0 for the contract builds. The workspace tests also run the web interop tests, which need Node 24 and `wasm-bindgen` 0.2.117. Report problems in the issue tracker.',
        },
      ],
    },
    {
      id: 'limits',
      title: 'Limits',
      lead: 'These hold today.',
      blocks: [
        {
          t: 'list',
          items: [
            "Testnet only. The USDC is Circle's test token. The circuits have had no independent review, and the anonymity set is made of test notes, so a lease is easier to tie to a note than it would be on a busy network.",
            'Open deposits are capped at 100 USDC. The owner can change the cap after a 48-hour delay announced by an event.',
            'A note expires 30 days after the deposit. The treasury can take it 7 days after the later of the expiry and the last exit activity, and never while an exit is pending.',
            'If the operator is offline for a whole 7-day window, a user can leave with an older, higher balance than the real one. The loss falls on the operator, and the cap bounds it.',
            'If you lose your latest state, an exit from an older state can be defeated and you can lose the remaining balance.',
            'The web app cannot use Tor. The operator and the RPC provider see your IP address.',
            "The team's full-access key on the vault account can replace the vault code.",
            'The proof keys come from a setup run by the team alone. If every contribution was dishonest, proofs can be forged and the vault emptied.',
            "No outside party has rebuilt a release yet. The macOS `nymirad` binary cannot be rebuilt to the same hash, because the macOS toolchain's output is not byte-stable.",
            'The operator runs on a home machine and can be offline. Exit still works after 7 days.',
          ],
        },
      ],
    },
    {
      id: 'changes',
      title: 'What changed in 0.1.3',
      lead: 'Release 0.1.3 of the web app and `nymirad`, compared with 0.1.2.',
      blocks: [
        { t: 'h3', text: 'The web app' },
        {
          t: 'list',
          items: [
            'A documentation page and a status page. The status page reads the operator, the vault, the RPC endpoints and the release when you ask, and shows what it read.',
            'The Agents screen: it builds and signs a budget-note policy and prints the `nymirad` commands. It creates no note and sends nothing to `nymirad`.',
            'The privacy panel, with one lease at a time and charts of who sees what.',
            'The Intents screen: a direct path and a two-step path for funding from another chain.',
            'A deposit whose event does not match stays pending. It is not marked rejected, and the app does not imply a refund.',
            'A stale tab cannot overwrite a newer sealed state of a note. The write is refused and the tab says so.',
            'Notes: a status filter, status marks, and a list that scrolls inside itself. The expiry guard shows its last 14 days.',
          ],
        },
        { t: 'h3', text: '`nymirad`' },
        {
          t: 'list',
          items: [
            "Budget notes for agents: `nymirad agent`, a signed policy, tokens that reach chat and models only, and an opt-in mode that moves the note to the agent's machine.",
            'The set size option `--rpc-second`, which asks a second provider and shows the lower figure.',
            'Pinned Tor packages for Debian 12 on amd64 and arm64, next to the macOS one.',
            'Message content sent as a list of text parts is accepted, so IronClaw on Linux works. Anything that is not text is still refused.',
          ],
        },
        { t: 'h3', text: 'Unchanged' },
        {
          t: 'p',
          text: 'The vault, the operator and the pinned verifying keys are the ones from release 0.1.2. No redeploy was needed.',
        },
        {
          t: 'note',
          title: 'Not in this release',
          text: 'The MCP server is not released; build it from a checkout of `mcp/`. No outside party has rebuilt a release yet.',
        },
      ],
    },
  ],
};

const SECTION_LINKS: Record<string, DocLink[]> = {
  changes: [
    { label: 'Releases', href: `${REPO}/releases`, ext: true },
    { label: 'Status', href: '#status' },
  ],
  overview: [
    { label: 'Open the app', href: '#fund' },
    src('README.md', 'README'),
    src('JUDGES.md', 'Guide for judges'),
    src('THREAT_MODEL.md', 'Threat model'),
    { label: 'Releases', href: `${REPO}/releases`, ext: true },
  ],
  'note-life': [
    to('vault', 'Vault entry points'),
    src('contracts/vault/src/lib.rs'),
    src('web/src/leave/guard.ts', 'Expiry guard'),
  ],
  words: [to('vault', 'Vault contract'), to('privacy', 'Who sees what')],
  privacy: [
    src('THREAT_MODEL.md', 'Threat model'),
    src('TERMS.md', 'Terms of use'),
    to('limits', 'Limits'),
  ],
  'web-app': [
    src('web/README.md', 'Web app notes'),
    tree('web/src/ui', 'Screens'),
    to('verify', 'Verify a release'),
  ],
  attestation: [
    src('web/pins/attestation-policy.json', 'Signed policy'),
    tree('web/src/chat/attest', 'Attestation code'),
    to('privacy', 'Who sees what'),
  ],
  vault: [
    src('contracts/vault/src/lib.rs'),
    {
      label: 'Vault account on the explorer',
      href: 'https://testnet.nearblocks.io/address/vault.nymira.testnet',
      ext: true,
    },
    src('PINS.md', 'Pins'),
    to('verify', 'Verify it yourself'),
  ],
  nymirad: [
    tree('crates/nymirad'),
    { label: 'Releases', href: `${REPO}/releases`, ext: true },
    to('agents', 'Budget notes for agents'),
    to('verify', 'Verify a release'),
  ],
  agents: [
    src('BUDGET_NOTES.md', 'Budget notes guide'),
    src('mcp/README.md', 'MCP server notes'),
    tree('crates/nymirad/src', 'nymirad source'),
  ],
  verify: [
    src('web/INTEGRITY.md', 'How releases are signed'),
    tree('web/releases/testnet-0.1.3', 'Release 0.1.3'),
    src('web/pins/release-signing.pub', 'Signing key'),
  ],
  faq: [to('limits', 'Limits'), src('THREAT_MODEL.md', 'Threat model')],
  develop: [
    tree('', 'Repository'),
    { label: 'Issues', href: `${REPO}/issues`, ext: true },
    src('NOTICE', 'NOTICE'),
    {
      label: 'Ceremony repository',
      href: 'https://github.com/nymiralabs/nymira-ceremony',
      ext: true,
    },
  ],
  limits: [src('THREAT_MODEL.md', 'Threat model'), src('TERMS.md', 'Terms of use')],
};

for (const section of PAGE.sections) section.links ??= SECTION_LINKS[section.id];
