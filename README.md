# Nymira

Private, prepaid API credits on NEAR.

**This is a testnet project with test funds.** The vault holds Circle's testnet USDC, which has no value. The circuit has had no independent review, and zkapi does not use it on its own mainnet yet. The anonymity set on testnet is small and made of test notes, so a lease can be tied to a note more easily than it could be on a busy network. Read [THREAT_MODEL.md](THREAT_MODEL.md) before you trust any sentence here. Use of the deployment is under the [terms of use](TERMS.md).

Want to try it? [JUDGES.md](JUDGES.md) is a 20-minute path from a new wallet to a first answer and a close.

## Live deployment

The vault runs on NEAR testnet with Circle's test USDC (token account `3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af`). Every value below can be checked against the chain.

| What | Value |
|---|---|
| Vault account | `vault.nymira.testnet`, deployed in transaction `G1LGwd1oxcrza7VSwNuGYf9xjaMeNJzYdEZ2EFamjQVY` (`new()` ran in the same transaction) |
| Vault code hash (reproducible build) | `71QXP9FT4eiRcaqjZtsHi7mXMCAexXjCFgQ338p8tur4` in base58; as SHA-256 of the wasm, `5940b853e51476c69ab2cd0ffba2fb76ffd3fedb621a2471c9ccd2bdb62ad4f5`. Built from the tag `vault-testnet-1` (commit `18107d876dd9d3912796f4b002e9a006c74b1152`) with `scripts/reproducible-smoke.sh` (cargo-near 0.22.0, rustc 1.99.0, image `sourcescan/cargo-near@sha256:51ab40efd798eae26c0dc71180909303f10bb4988b15ce3a18522c5be06e277e`). The on-chain hash was checked against the build with `view_account`, and testnet.nearblocks.io shows the contract as source code verified (<https://testnet.nearblocks.io/address/vault.nymira.testnet/contract/code>). Two builds on one Mac (arm64) were equal, and a build on an amd64 CI runner (`vault-reproducible` workflow, 5 November 2026) gave the same hash. |
| `vk_hashes()` next to the ceremony transcript hashes | On chain: request `0xf3092e0d550f696a6507f279862afb43d0c22663de56f1b47160b5db6fc6fa1b`, withdrawal `0x3469ebb3245101549343dbe5939427d3dc7706c314f281f9b5935416fc720c8d`. These equal the verifying key keccak256 values in the `final` release of [nymira-ceremony](https://github.com/nymiralabs/nymira-ceremony), whose transcript SHA-256 is `e75cae40c203b08f8b9e559a2fa4b18f464e709d68abe01318c2d8a7aa4b7e07` (beacon: NEAR mainnet block 222942364). Proving key SHA-256: `request.pk` `b5cdb12000cf51cf566375aadbc2a27cf875438f646f751fa857371cd6cb7022`, `withdrawal.pk` `89e03ac8f9d6e90d189e9a3eb6b0c9c0126e1b121e78955a11f0f70a44d0161d`. |
| `view_access_key_list` of the vault | One key, full access: `ed25519:DJc2C6WWveyUY1VvLEdUPjBggzKrMuEwx7GGp1BLHMze`. It is the team's, and it can replace the vault code (see [THREAT_MODEL.md](THREAT_MODEL.md)). |
| Vault config (`config()`) | Operator `operator.nymira.testnet`, owner `owner.nymira.testnet`, treasury `treasury.nymira.testnet`. Denominations 1 and 5 USDC, bound classes 0.10 and 0.50 USDC, charge step 0.001, minimum charge 0.002, deposit fee 0.05 USDC, close fee 0.03 USDC, exit delay 604800 s (7 days), note TTL 2592000 s (30 days), cap on open deposits 100 USDC. |
| Web bundle | Release testnet 0.1.2 (tag `web-v0.1.2`, commit `d00dc17ea51c409a9bfe7cec1490d8dc841aa352`), served at <https://nymira.pages.dev> and mirrored at <https://nymiralabs.github.io/nymira/>. `bundle_hash` `3ee5ccb57cdaf2a1fa58cd914b6806909eac98c4dd20c013f6e0af5c926e045f`, `header_set_hash` `0a09ba64fb8a576c119a313c52b3ba78fe426454083cfae085cd43b0212e2eb8`. The signed manifest is in `web/releases/testnet-0.1.2/`; the signing key is `web/pins/release-signing.pub` (minisign key id `934C176AF34D794D`). A public Sigstore Rekor entry (index 3700443670) holds the hash of a text with these two hashes and the key; see `web/INTEGRITY.md`. Cloudflare adds `access-control-allow-origin: *` to some pages while the signed header set has none; `nymirad verify-web` prints those as accepted notes. No outside party has rebuilt the bundle and attested to it yet. |
| Keys the vault uses | The vault account holds only the full-access key above. `operator.nymira.testnet` holds function-call keys limited to `vault.nymira.testnet`: `heartbeat` (`ed25519:ByNe51phL1HLruA2m5mpUNjYU8N8fdAiv2vzjTUPg8Cp`), `mutual_close` (`ed25519:6Sz6SKZd4VRYSiXFANiFS32yNhSfAMwDarHmbmjMiacZ`), and two upkeep keys for `sync_root`, `retire_closed_leaf`, `claim_expired`, `claim_expired_batch` and `finalize_exit` (`ed25519:EcnUgf2F3nVVGh3VJDxydjxfo8asTGJrv2oPaTEns3Di` and, from a second machine, `ed25519:FQZbqfECB8XpMVP42osm5HadqiZpqUyuVzF2cEBmFM7s`). The operator account also still carries two old gas keys for a test vault that was deleted; they are harmless. `answer.nymira.testnet` holds the `defeat_exit` and `raise_concession` keys: `ed25519:3aQcKFzVdwHVBM6vjES6LYimcwDZWRwr7V2grAVg1qRC` (laptop) and `ed25519:HBqKTwBSqY62moLVqCK7MM7DZ36hAcQ2jTo2P95J5KbC` (second machine); its full-access key is `ed25519:396S8bP3UUU4zgRLjjYGGP7rEZMi47SzS7T6MKMUnEcf`. |
| Operator status | <https://nymira-operator.tail0484c4.ts.net/v1/status>, served from a home machine that can be offline. |

The proving and verifying keys come from a setup run by the team alone (the log, scripts and the accepted contributions are in [nymira-ceremony](https://github.com/nymiralabs/nymira-ceremony)). They are sound if at least one contributor really was honest, and the team cannot prove that: if every contribution was dishonest or its machine was compromised, proofs can be forged and the vault emptied. zkapi's development keys, whose secrets are public, never run on the final public vault or on mainnet. The deployed vault uses the final ceremony keys (see the table). A deposit fee is read from the vault's `config()`; on testnet it is a placeholder in test USDC and is not revenue.

The proving keys are release assets of the ceremony repository (`request.pk`, `withdrawal.pk`, tag `final`). The web app serves them from its own signed bundle and checks their SHA-256 before use. For `nymirad`, download both files into one directory and pass it once: `nymirad init ... --setup-dir DIR`. `init` refuses files whose hash differs from the one compiled into the binary, and without the directory chat and close stay off. `nymirad terms` prints the terms of use.

## Limits

These are the rules of the vault contract and of the testnet deployment. The vault's `config()` shows the values. The operator cannot send your money to another account or block your exit: with `request_exit` you leave to your payout account after 7 days, and the operator can deduct at most the bound of the one lease in flight. If you lose your latest wallet state, an exit from an older state can be defeated and you can lose the remaining balance, so keep your encrypted backup. A note expires 30 days after the deposit; seven days after that, the treasury can take it unless an exit is pending. The team can still update the vault code, and open user deposits in the vault are capped at 100 USDC (test USDC on testnet; the cap bounds open deposits, not what the provider is paid).

The first sentence holds while you keep your latest state; see the first point below. The cap is a vault parameter that the owner can change after a 48-hour delay announced by an event.

- An exit from an old state can be defeated: if you lose your latest state, the operator can answer the exit with a later request and keep what it charged, up to the whole remaining balance. Keep a backup.
- A `nymirad` backup is an age file (X25519) written to the places you chose at setup. With a passphrase instead of a key pair, the age key is derived from it with Argon2id (64 MiB of memory, 3 passes, 1 lane), and the passphrase needs at least 12 characters. A restore needs only the private key or the passphrase, so treat either as you would the notes themselves. `nymirad restore-secret` rebuilds one note from its secret alone, and that note can then only be exited, not spent. Keep a second copy on another device.
- A note expires at the end of the UTC day 30 days after the deposit. The treasury can take it only 7 days after the later of that expiry and the last exit activity on the note, and never while an exit is pending; until then an expired note can still exit and finalize.
- If the operator is offline for a whole 7-day exit window, a user can leave with an older, higher balance than the real one. The loss falls on the operator, and the 100 USDC cap on open deposits bounds it.
- Closing shows your note ID, total spend and payout account on chain, next to the account that sends the close.
- The web app cannot use Tor, so the operator and the RPC provider see your IP address. Use Tor Browser or `nymirad` for IP protection.
- GPU evidence goes through the operator relay and is checked in your client. NVIDIA's attestation service (NRAS) answers 403 to Tor exit nodes ([issue 165](https://github.com/nymiralabs/nymira/issues/165)), so `nymirad` cannot send the model's GPU evidence there over Tor. The operator forwards it instead and returns the answer unchanged. It learns that some client asked for a GPU check, and when, and keeps nothing. Your client checks the signature, the nonce, the times, and that the signing key's certificate chain reaches a certificate pinned in the signed attestation policy. The call uses its own Tor circuit, apart from your lease.
- The signed policy (version 5) pins two NVIDIA intermediate certificates. They come from NVIDIA's public key set, where on 5 November 2026 every key carried a two-certificate chain. On 5 November 2026 the 005 certificate matched, byte for byte, the one served by NVIDIA's own PKI host (`prod.vault.nvidia.com`), and both intermediates verify under NVIDIA's "Attestation Service CA 001" root published there (SHA-256 `CDE269E3…1A1E`). No NVIDIA documentation page lists these fingerprints, so the policy still marks the pins as unverified, and the root is not pinned. One chat through `nymirad` over Tor with the relay passed the GPU check and ended model-signed on 5 November 2026.
- `nymirad serve --nras-direct` sends the GPU evidence straight to NVIDIA, without Tor, so NVIDIA sees your IP address and when you ask. It is off by default, `nymirad status` says when it is on, and `nymirad` never switches to it by itself.
- The web app does not check NVIDIA GPU evidence. It checks the Intel TDX quote, the signed policy and the key binding.
- A compromised host could serve a different web app. `nymirad verify-web` catches that only if you run it and the host does not cloak, so it does not replace trusting the host you load the app from.
- Everything the other parties see is in [THREAT_MODEL.md](THREAT_MODEL.md).

Nymira reuses the zero-knowledge circuits of [zkapi](https://github.com/ethereum/zkapi) (`ethereum/zkapi`), vendored at a pinned commit in `vendor/zkapi/`. `NOTICE` lists the exact commit and every patch we apply. `PINS.md` records the pins and the toolchain.

## Download `nymirad`

Each `nymirad-vX.Y.Z` release on GitHub has a signed `SHA256SUMS` (`SHA256SUMS.minisig`, key in `web/pins/release-signing.pub`) that lists every file. Check the signature with `minisign -V -p web/pins/release-signing.pub -m SHA256SUMS`, then run `shasum -a 256 -c SHA256SUMS`.

- **Linux x86_64** (`nymirad-X.Y.Z-x86_64-linux`): reproducible. Anyone can rebuild it from the tag with the pinned container and get the same bytes (`scripts/release-nymirad.sh build`).
- **macOS arm64** (`nymirad-X.Y.Z-aarch64-macos`): built by a GitHub Actions workflow on a macOS runner. The signature shows that the project published these exact bytes. It does not show that the binary matches the source, because the macOS toolchain's output is not byte-stable and nobody can rebuild it to the same hash. If that matters to you, build from the tag yourself or use the Linux binary.

`nymirad` starts its own Tor, and it runs only a Tor executable whose SHA-256 is compiled into the build. A build pins one hash per platform. Today the only pinned package is Homebrew's `tor` 0.4.9.15 for macOS arm64 (`c0dcd5d18c534d1e7049c5105bfc39189527b33be65b3d064052c213e7404a46`). No Linux package is pinned yet, and `arti` is not pinned.

On a Mac with Apple silicon:

1. `brew install tor`, then check `shasum -a 256 "$(brew --prefix tor)/bin/tor"` prints the hash above. Homebrew moves to newer versions; a different version has a different hash and `nymirad` refuses to start it. In that case use option 3.
2. Copy that file to `~/.nymira/tor/package/tor` (or set `tor_package` in `config.json` to its path). Without a pinned Tor package or your own SOCKS proxy, chat and `close` do not start.
3. Or run a Tor you trust yourself and give `nymirad` its loopback SOCKS port: `nymirad init ... --socks 127.0.0.1:9050`. Then `nymirad` does not check the Tor executable; you do.

On Linux the same applies: no Tor package is pinned, so use option 3 until one is.

## Set size in `nymirad status`

Each lease records how many notes could have paid for it. `nymirad` counts them on your machine from the vault's `note_records`, read at the same final block as the tree your proof used, and takes the team's notes out. The team's IDs are the list in `crates/nymira-operator/team-note-ids.json`, which each release compiles in, plus any IDs the operator adds in `/v1/status`. `nymirad status` prints the result for each lease as `set size: N (external), total T`, and a `now:` line for the same count at the current block.

Name a second, independent provider with `nymirad init ... --rpc-second URL`. The count is then read from both, the lower figure is used and a difference is shown. Without it the count comes from one provider and status says so.

The figure is an upper bound. Balances are private, and N includes notes the operator or the team control and did not list. With 5 or more external notes, 5 is still not a safe number. The sentences status prints are in `shared/privacy-strings.json`.

## Build

The toolchain is pinned in `rust-toolchain.toml` (rustc 1.99.0, `wasm32-unknown-unknown`). Install cargo-near 0.22.0, then:

```bash
cargo check --target wasm32-unknown-unknown --locked -p nymira-vault -p nymira-near-codec -p nymira-verifier -p verifier-probe -p mock-usdc -p mock-ft-receiver -p nymira-near-codec-wasm
cargo test --workspace --locked
```

`cargo test --workspace --locked` also runs the web interop tests, which need Node 24, `wasm-bindgen` 0.2.117 and the web dependencies first:

```bash
web/scripts/build-wasm.sh && (cd web && npm ci --ignore-scripts)
```

## Try it locally

A local stack needs no keys and no money: a sandbox vault with a mock USDC token, the operator, a fake NEAR AI gateway and a fake 1Click service. The demo prints an Intents deposit command that pays a fake address through a sandbox stand-in.

```bash
make golden   # a note is deposited, used, settled, closed and retired, asserted step by step
make demo     # the stack stays up and prints the `nymirad` commands to try against it
```

Both need cargo-near 0.22.0. The `mock` build of `nymirad` that `make demo` uses talks to the sandbox without Tor and trusts a throwaway attestation policy; a release build has neither.

## Intents funding in nymirad

The mainnet profile supports `deposit --source list --amount 1000000` and `deposit --source ethereum-usdc --amount 1000000 --payout ACCOUNT --refund SOURCE_WALLET`. The client asks once per attempt before contacting 1Click directly, outside Tor. It checks signed exact-output quotes, saves the pending note encrypted and verifies its encrypted backup before showing a deposit address. Resume with `recover --intents COMMITMENT`; restarting requires a new direct-access confirmation.

Recovery saves the destination transaction before reading its receipts. Set `archival_rpc_url` in `config.json` to an archival endpoint for old mainnet receipts; RPC recovery and the payout-account balance check use Tor. With the daemon running, `claim-payout --intents COMMITMENT` claims the saved payout account even when no note ID was created. Its submitter needs NEAR for gas. Pending payout balances are account totals and do not confirm a note. Dust and unclaimed amounts go to the treasury balance; recovery explains them without offering a claim.

1Click sees the source wallet, note commitment, payout account and IP address on this path. Its quote signature identifies the issuer; it does not guarantee delivery. The client accepts a note only from a matching final vault receipt. Testnet offers signed quotes labelled "quote only" over Tor and makes no Intents payment. Use Circle's faucet for testnet USDC.

The CLI integration tests deliver through the open-source Verifier, release v0.4.2, and through the demo stand-in. They cover a pending-backup restore, renewed consent, receipt acceptance and a lease on the new note. No live cross-chain swap is claimed. A mainnet build carries one Intents path, fixed at build time: `two-step` (1Click pays your own NEAR account, then `nymirad` sends the USDC to the vault). That is the decision because 1Click has given no written answer and the mainnet probe has not run, so the path has not been run against the live 1Click. The live destination-signer lookup and batching shape still need the mainnet probe; no mainnet vault is pinned in this build.

## Budget notes for agents

A budget note is a note you fund for one agent. Its payout account is your own account, fixed at deposit, so whatever the agent leaves comes back to you. The agent gets a token and can spend that one note on inference. It cannot withdraw anywhere else.

```bash
nymirad agent create --name cursor --transport tcp --key-file KEY --amount 1000000
nymirad agent create --name worker --transport unix --agent-user agent1 --token-file /srv/agent/worker.token --key-file KEY --amount 1000000
nymirad agent list
nymirad agent disable --name cursor
nymirad agent policy --name cursor --file cursor-policy.signed.json
```

`agent create` is the normal deposit plus a registry entry, written before the transfer is signed. On chain the note looks like any other note of its denomination. It prints a reminder that a fresh 1 USDC note each day is the hard way to cap a day's spend: it costs the note fee every day and leaves a daily deposit pattern on your account. The registry (`agents.json` in the state directory, mode 0600) keeps the agent name, a hash of the token, the budget note, your account and the policy version. Tokens are never stored. Agent tokens start with `nyma_`, the owner token with `nymo_`, so a tool can accept only the agent prefix.

- **Own note only.** An agent token draws on its budget note and nothing else. Your own token never selects a budget note, and `nymirad status` marks them.
- **One transport per token.** A `unix` token works on the agent socket only and a `tcp` token on loopback port 8788 only. A remote agent reached through an SSH forward gets a `tcp` token.
- **Routes.** An agent token reaches `POST /v1/chat/completions`, `GET /v1/models`, `GET /v1/nymira/balance` and `POST /v1/nymira/quote` on both listeners and nothing else, and works as an OpenAI API key on the first two (the last two are described under "Agent API" below). The owner token is not accepted on the agent socket.
- **Disabling.** `agent disable` takes effect on the next request, which gets 401 `token_disabled`.
- **No policy, no spending.** Until a signed policy is loaded for the agent, every agent request gets 402 `policy_missing` and nothing reaches the operator. A policy that is unsigned, changed after signing, signed by another account or by a function-call key, signed for another agent or note, older than one already seen, or that misses a limit key or has an unknown one counts as no policy.
- **Signed policy.** `policy.yaml` has `allowed_models`, `max_lease_usd`, `max_call_usd`, `max_daily_usd`, `confirm_above_usd`, `rate_limit_per_min`, `require_tee` (omitted means true) and `allowed_origins` (empty for now; a non-empty list is refused until paying sellers exists). You sign it with your NEAR account (NEP-413) and a full-access key. The signed message is JSON, `{"kind":"nymira-policy","version":N,"policy":"<the yaml text>"}`, and the recipient is `nymirad:<agent name>:<note commitment>`. `agent policy` checks the signature, reads your account's access keys over the configured RPC endpoints to confirm the key is full-access, prints a summary and asks you to type yes. The policy applies from the agent's next request. The YAML is a small subset (one `key: value` per line, plain-word lists, no anchors, tags, merge keys or quotes) and is parsed only after the signature over its exact bytes has verified. The signed file is kept in the state directory (`policy-<agent>.json`, mode 0600) and checked again on every request.
- **Model allowlist.** A request for a model outside `allowed_models` gets 402 `model_not_allowed` before anything leaves the machine, also on a lease that is already open. `GET /v1/models` lists only the allowed models.
- **Spending limits.** Every agent request, also one on an open lease, is checked in this order: token enabled, valid signed policy, model, rate, call cost, the day, and last the note's balance. `rate_limit_per_min` caps requests in a minute (429 `policy_rate_limit` with `Retry-After`). `max_call_usd` lowers the `max_tokens` that is sent so the call's worst case stays within it; a request whose own `max_tokens` asks for more, or whose input alone passes the limit, gets 402 `call_limit`. `max_daily_usd` counts, for the UTC day, what settled leases charged plus the bound of the open lease plus the worst case of calls in flight; a request that would pass it gets 402 `daily_limit`. Opening a lease reserves its bound, and settlement replaces the reservation with the charge. The day's total is kept in `counters-<agent>.json` in the state directory and after a restore is never lower than the charges in the lease records. Leases use the 0.10 USDC bound class; the 0.50 class only when the request asks for it with `X-Nymira-Max-Lease-Usd` and `max_lease_usd` is at least 0.5. The call limit lowers `max_tokens`, never the bound. A note that cannot cover the bound gets 402 `budget_exhausted`, whatever your other notes hold. A third request while the note's lease has two in flight gets 409 `busy`. A call whose worst case is above `confirm_above_usd` is held for your approval and gets 402 `approval_required`.
- **`require_tee`.** With `require_tee: true`, the default, an agent gets an answer only after `nymirad` has checked its signature against the exact bytes exchanged. The model's attestation report, and the binding of the encryption key to it, are checked before anything is encrypted, and the answer opens only with that key. The provider's signatures can only be read once a lease is closed, so each such call closes its lease, waits up to 120 s for the operator to store the signature, and only then releases the answer. A streamed answer is held and released in one piece. The reply carries `X-Nymira-Signature-Kind`, `model-signed` or `gateway-signed`. `require_tee` means the attestation verified and the key is bound to it; a gateway-signed answer passes, and it does not show that the model itself signed. Streams are expected to be gateway-signed. An answer that fails gets 502 `not_verified` and no content. The closed lease has to settle before the note can serve the next call. With `require_tee: false` the lease stays open and answers arrive at once.

### Agent API

An agent calls these with `Authorization: Bearer nyma_...` on the agent socket or on loopback port 8788. Every number is in micro-USD (1 USDC is 1,000,000) unless the name says USD.

- `GET /v1/nymira/balance` returns `balance_micro` (the deposit less the charges of settled leases; a lease still open counts when it settles), `spendable_micro` (the balance when it can pay for a lease of the agent's class and the note is open, else 0), `lease_bound_micro`, `spent_today_micro`, `note` (`status`: `open`, `expiring`, `revoking`, `exiting`, `closed` or `unfunded`; `closing`; `expires_at` in Unix seconds), `charge_warning` and `limits` (the policy's limits). It names no other note.
- `POST /v1/nymira/quote` takes the same body as a chat request and runs the same checks, in the same order, as `POST /v1/chat/completions`. It moves no counter, holds no approval, opens no lease and sends nothing. It answers `{"decision": "allow" | "needs_approval" | "deny", "reason": ..., "estimated_cost_micro", "max_tokens", "input_tokens", "model"}`; a `deny` also carries `http_status`, `retry_after_secs` for a rate limit and `detail` for `unavailable`. The balance and the lease are read from local state, so a refusal the operator would make at the lease (its own limits) shows only when the call is made.
- `X-Nymira-Max-Cost-Usd` on a chat or quote request caps the call's cost in USD with at most 6 decimals, for example `0.002`. It lowers the `max_tokens` that is sent and is checked together with `max_call_usd`; a request whose own `max_tokens` would pass it, or a cap below the cost of the prompt and one answer token, gets 402 `call_limit`.
- `Idempotency-Key` on a chat request (1 to 128 printable characters, not with `stream: true`): a retry of the same request with the same key returns the first answer, with `X-Nymira-Idempotent-Replay: true`, and spends nothing. A retry that arrives while the first request runs waits for it. The answer is found by token, note, key and request, so another token, another note or another request finds nothing. It is kept in memory for 5 minutes, at most 64 at once, never written to disk or a backup. Only answers are kept; a refusal or failure runs again on retry.
- `X-Nymira-Approval-Id` on a chat request is the retry of a held call. The 402 `approval_required` that held it carries the ID as `error.approval_id` and in the `X-Nymira-Approval-Id` response header.
- `X-Nymira-Max-Lease-Usd` asks for the 0.50 lease class, as above.

Errors use the OpenAI shape, `{"error": {"message", "type", "param", "code"}}`, plus `approval_id` for `approval_required` and `detail` for `unavailable`. On the agent API the `code` is one of the reasons below and the `message` is a fixed text or one `nymirad` wrote from its own figures; nothing the operator or the model wrote is passed on.

| Status | `code` | Meaning |
|---|---|---|
| 401 | `token_disabled` | the owner disabled this token |
| 402 | `policy_missing` | no valid signed policy for this agent |
| 402 | `model_not_allowed` | the model is not in `allowed_models` |
| 402 | `call_limit` | the call would pass `max_call_usd` or the cost cap |
| 402 | `daily_limit` | the call would pass `max_daily_usd` |
| 402 | `approval_required` | held for the owner; carries the approval ID |
| 402 | `budget_exhausted` | the note cannot cover the lease |
| 402 | `note_expiring` | the note is in its last 5 days, or a revocation, close or exit has begun |
| 402 | `no_note_can_cover_request` | the lease cannot pay for this prompt and an answer |
| 429 | `policy_rate_limit` | `rate_limit_per_min` reached; `Retry-After` given |
| 429 | `operator_capacity` | the operator's admission, in-flight limit or daily provider budget refused; `Retry-After` given, to the next UTC midnight when the daily budget is used up |
| 429 | `provider_rate_limit` | the model provider is rate limiting; `Retry-After` given |
| 409 | `busy` | the note's lease already has 2 requests in flight |
| 502 | `not_verified` | `require_tee` withheld an answer whose signature did not verify |
| 503 | `unavailable` | `detail` says why: `tor_down`, `tor_refused`, `vault_pins_changed`, `operator_emergency`, `charge_warning` (see below), `operator_unreachable`, or the code of any other operator error |

Approval requests also use `approval_unknown` (404), `approval_used` (409), `approval_expired` (410), `approval_mismatch` (409) and `approval_limit` (429); a malformed body is 400 `invalid_request`.

A **charge warning** is raised when a lease settles with a charge more than one charge step above the one recomputed from the signed usage. From then on every request on that agent's note gets 503 `unavailable` with `detail: charge_warning`, `nymirad serve` prints a warning, and `agent show` marks it, until you run `nymirad agent ack-warning --name NAME`. The mark is a file in the state directory and part of the backup.

### Another OS user on your machine

Use `--transport unix --agent-user USER --token-file PATH`. `nymirad serve` opens a socket at `/tmp/nymira-agent-<your uid>/agent.sock` (or `agent_socket_dir` in `config.json`), mode 0660 in a directory of mode 0750, both in the agent group. The group is the agent user's primary group, or `--group`. Every connection's peer user ID is checked: your own OS user is refused, as is any user no socket agent is registered for, and each token works only for its own user. The token file is mode 0640, owned by you, in the agent group, so the agent user can read it and nobody else can. If you are not a member of that group, you cannot give files to it: the command then prints `sudo chgrp GROUP FILE` for the token file, and `serve` prints the `sudo chgrp` and `chmod 2750` commands for the socket directory. Restart `nymirad serve` after the first socket agent is created.

The peer check on the socket is the only OS-user check there is. Loopback TCP has none, so a `tcp` token is a bearer secret for anything on the machine that can read it. An agent that runs as your own OS user (Cursor, IronClaw, any tool started from your shell) can read your owner token. Run it against a dedicated `nymirad` instance with its own `--home`, port and owner token that holds only its budget note and none of your personal notes. `agent create` refuses `--agent-user` set to your own user.

### Another machine

Requests arrive on the loopback listener, so they pass the same checks as yours: loopback peer, `Host: localhost:8788`, no `Origin`, no cross-site fetch. Start the forward from your own machine, so the agent host holds no credential for it:

```bash
ssh -N -R 8788:localhost:8788 AGENT_HOST
```

The forwarded port binds to the loopback of the agent host as long as `GatewayPorts` stays off there. A direct connection to your machine over a tailnet or LAN is refused. If the forward has to start on the agent host, restrict that key in your `authorized_keys` with `restrict,port-forwarding,permitopen="localhost:8788",command="/bin/false"` and run `ssh -N -L 8788:localhost:8788 OWNER_HOST` there. That restriction is standard OpenSSH behavior and has not been tested against this release; `scripts/agent-ssh-forward-check.sh [HOST]` runs both forwards and the refusals against an ssh host, and `scripts/agent-os-user-check.sh` does the same for the socket and token file with a throwaway second OS user (macOS, needs `sudo`).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option. Code taken from other projects is listed in [NOTICE](NOTICE).
