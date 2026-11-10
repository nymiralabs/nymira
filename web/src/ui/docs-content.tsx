import type { ReactNode } from 'react';

// The text of the documentation page. Every figure below was checked against the code:
// contracts/vault/src/lib.rs, web/src/profiles/testnet.ts, web/src/leave/guard.ts, crates/nymirad,
// crates/nymira-operator and mcp/src. Strings may hold `code` spans and {{new}}, which renders the
// "not in 0.1.2" tag for what exists on main and not in the live release.

export type Block =
  | { t: 'p'; text: ReactNode }
  | { t: 'h3'; id?: string; text: ReactNode }
  | { t: 'list'; items: ReactNode[] }
  | { t: 'steps'; items: [string, ReactNode][] }
  | { t: 'table'; label: string; head: string[]; rows: ReactNode[][] }
  | { t: 'code'; label?: string; text: string }
  | { t: 'note'; title: string; text: ReactNode }
  | { t: 'diagram'; id: 'note-life' };

export interface Section {
  id: string;
  title: string;
  lead?: ReactNode;
  blocks: Block[];
}

export const PAGE: { title: string; lead: ReactNode; sections: Section[] } = {
  title: 'How Nymira works',
  lead: 'Nymira sells prepaid API credits on NEAR. You deposit USDC once and spend it in short leases, one per conversation. A zero-knowledge proof shows that a deposit can pay without saying which deposit it is. This page matches the code on the main branch. Parts marked {{new}} are not in the live release.',
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
        {
          t: 'table',
          label: 'What each party sees',
          head: ['Party', 'Sees', 'Does not see'],
          rows: [
            [
              'Operator',
              'The lease proof, the nullifier, the bound class, request times, the charge, the model name, message sizes and usage. From the web app it also sees your IP address. Over Tor, `nymirad` shows a Tor exit instead.',
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
        { t: 'h3', text: 'Agents {{new}}' },
        {
          t: 'p',
          text: 'The screen builds `policy.yaml` from form fields and signs it with your wallet (NEP-413). It also signs approvals and prints the `nymirad` commands. It creates no note, shows no token and sends nothing to `nymirad`. `require_tee: true` is the only value it offers. It is hidden when no wallet can sign messages.',
        },
        { t: 'h3', text: 'What the browser stores' },
        {
          t: 'list',
          items: [
            "IndexedDB `nymira` holds each note's public record and one sealed payload. The payload is AES-256-GCM, bound to its note.",
            'The key comes from Argon2id (64 MiB, 3 passes, 1 lane, passphrase of at least 12 characters) or from a WebAuthn PRF output. It is non-extractable and lives in memory. "Stay unlocked in this tab" {{new}} keeps it in a second IndexedDB until the tab closes.',
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
            'The signed policy has version 5 or higher and an Ed25519 signature from a pinned signer. It accepts only the TCB status `UpToDate`.',
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
          text: "Policy version 5 pins exact compose hashes for the model and accepts any compose hash for the gateway. It pins two NVIDIA intermediate certificates and marks both as not verified against NVIDIA's documentation. The NVIDIA root is not pinned.",
        },
      ],
    },
    {
      id: 'vault',
      title: 'The vault contract',
      lead: 'The vault is `vault.nymira.testnet`. Its code hash is `71QXP9FT4eiRcaqjZtsHi7mXMCAexXjCFgQ338p8tur4`, built reproducibly from the tag `vault-testnet-1`.',
      blocks: [
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
          text: 'minisign -V -p web/pins/release-signing.pub -m web/releases/testnet-0.1.2/manifest.json\nnode web/scripts/integrity.mjs fingerprint web/pins/release-signing.pub',
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
          text: 'The two hashes must equal the `bundle_hash` and the `header_set_hash` of release 0.1.2: `3ee5ccb57cdaf2a1fa58cd914b6806909eac98c4dd20c013f6e0af5c926e045f` and `0a09ba64fb8a576c119a313c52b3ba78fe426454083cfae085cd43b0212e2eb8`. `nymirad verify-web` runs the same check over Tor.',
        },
        {
          t: 'code',
          label: 'The vault',
          text: `curl -s -H 'content-type: application/json' https://test.rpc.fastnear.com \\\n  -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"view_account","finality":"final","account_id":"vault.nymira.testnet"}}'`,
        },
        {
          t: 'p',
          text: 'The vault has one access key and it has full access. Check it with `view_access_key_list`. To rebuild the web release, run `web/scripts/rebuild-in-container.sh web-v0.1.2 testnet 0.1.2` (it needs Docker). To try everything with no keys and no money, run `make golden` or `make demo`.',
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
  ],
};
