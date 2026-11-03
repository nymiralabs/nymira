# web

The Nymira web app. Funding (#36) gives the landing page, wallet connection, vault checks, the Fund screen, note storage, backup and restore. Chat (#37) adds the Chat screen. Close and exit (#38) and hosting (#39) build on both.

The app is a static bundle: Vite 8, React 19, TypeScript. It has no backend and no third-party script, font or analytics. Everything below says what it does and what it cannot promise.

## Build and test

```sh
web/scripts/build-wasm.sh      # once, and after changing the vendored zkapi or the codec
cd web
npm ci                         # lockfile install, scripts off (.npmrc)
npm run build                  # testnet profile
npm test                       # typecheck, prettier, executor pins, unit tests, Playwright
```

`build:wasm` needs `wasm-bindgen-cli` 0.2.117 (it installs it if missing) and the pinned Rust toolchain. Playwright needs `cargo` (it runs the fake NEAR RPC from this workspace) and `npx playwright install chromium webkit`. The test wallet comes from `web-testkit/` (run `npm ci` there once; `web-testkit/README.md`).

| Command                                              | What it does                                                                                                                                                      |
| ---------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `npm run build` / `build:mainnet` / `build:mock`     | One bundle per profile (below).                                                                                                                                   |
| `npm run check:bundle -- dist testnet`               | Fails when a deployable bundle holds test-only content (loopback, mock vault, test wallet).                                                                       |
| `npm run check:executors`                            | Fails when a vendored wallet executor and its pin disagree.                                                                                                       |
| `npm run pin-vault -- --rpc <url> --vault <account>` | Prints the `pins` object for a profile from a deployed vault.                                                                                                     |
| `npm run test:unit`                                  | Vitest, including the real zkapi wallet core in Node.                                                                                                             |
| `npm run test:e2e`                                   | Playwright on Chromium; WebKit runs the tests tagged `@webkit`. The chat tests start a mock operator and gateway inside the test worker (`e2e/mock-operator.ts`). |

## Layout

```
web/
  index.html, wallet-host.html     the two pages of the bundle
  headers.ts                       the response headers the app is written for (CSP etc.)
  vite.config.ts                   profile alias, preview headers
  pins/executors.json              SHA-256 of each vendored wallet executor
  public/executors/                the vendored executors (third-party code, not formatted)
  test-fixtures/                   mock vault, test-only executor (mock builds only)
  scripts/                         build-wasm.sh, check-*.mjs, pin-vault.mjs
  src/
    profile.ts, profiles/          the build profile: network, accounts, RPC URLs, pins
    chain/                         rpc.ts (fallback JSON-RPC), vault.ts (pin check),
                                   token.ts (payout checks), receipt.ts (deposit event),
                                   tree.ts (leaves at one final block)
    wallet/                        host.ts (runs NEAR Connect in a sandboxed frame), client.ts
                                   (the app's side), manifest.ts, pinned-fetch.ts, nep413.ts
    crypto/                        kdf.ts (Argon2id), aead.ts (AES-GCM), wrap.ts (wrapping key,
                                   passphrase or WebAuthn PRF)
    store/                         db.ts (IndexedDB), types.ts (NoteRecord and its phases)
    note/core.ts                   everything that touches a plain note secret (funding, backup)
    note/lease.ts                  the same for a chat: request proof, signed successor, lease
                                   records. rawjson.ts keeps u64 digits intact.
    worker/                        the note worker that runs note/core.ts, its protocol and client
    backup/                        format.ts (the file), ics.ts (calendar reminder)
    funding/                       flow.ts (the funding steps), oneclick.ts (dry quotes)
    chat/                          session.ts (one conversation: attest, lease, encrypted chat,
                                   settle), operator.ts (HTTP client and error texts), e2ee.ts,
                                   gate.ts (status nonce, parameters), charge.ts, models.ts,
                                   puzzle.ts, backup-policy.ts, runtime.ts (page wiring),
                                   attest/ (policy, evidence checks, Intel quote, signatures)
    ui/                            React screens
    wasm/                          build output of build-wasm.sh (not committed)
  pins/                            executors.json, attestation-policy.json and policy-signer.pub
                                   (copies of what nymirad ships; a unit test compares them)
  test-fixtures/near-ai-recorded/  real NEAR AI evidence recorded on 2026-11-02 (test data)
  e2e/                             Playwright tests and support code
  test/                            unit tests
```

## Profiles

A bundle is built for one profile (`vite build --mode <profile>`) and contains that profile's constants only. The network, vault, RPC endpoints, operator URL and pins are compiled in. Nothing in the URL, local storage or IndexedDB changes them, in any build.

| Profile   | Use                                                                | Vault pins                                                                         |
| --------- | ------------------------------------------------------------------ | ---------------------------------------------------------------------------------- |
| `testnet` | The hackathon deployment (ADR-0019).                               | `null` until the judge vault exists. The Fund screen then says it is switched off. |
| `mainnet` | The later launch.                                                  | `null`. No public mainnet vault during the hackathon.                              |
| `mock`    | Playwright only. Loopback RPC, mock vault, a test wallet executor. | Fixture.                                                                           |

`check:bundle` runs on the testnet and mainnet bundles in CI. The app also refuses to run from an origin other than the profile's `appOrigin`, and makes no request when it does.

## Trust boundaries

**Vault tripwire** (`chain/vault.ts`). At load and before every deposit the app compares the vault's code hash, `vk_hashes()`, every `config()` value except `tvl_cap` and `current_tvl`, and its access keys with the pins. Any difference, or a vault it cannot reach, stops funding and shows what differs. The deposit button re-runs the check, so a vault changed after the page loaded is caught.

**Secrets** (`note/core.ts`, `worker/`). A note secret is made in a Web Worker by the vendored zkapi wallet core. The worker seals it with AES-256-GCM under a non-extractable key and returns only ciphertext and the commitment. The page's main thread never holds a plain secret. The sealing key comes from an Argon2id passphrase (hash-wasm, 64 MiB, 3 passes, 1 lane) or from a WebAuthn PRF output through HKDF. It lives in memory for the session. IndexedDB holds the public record and one sealed payload. The payload is bound to the note by its additional data, so a sealed payload cannot be moved to another note.

**Backup** (`backup/format.ts`). A JSON envelope with the same Argon2id parameters and an AES-256-GCM body. The passphrase has at least 12 characters. The app refuses files that ask for far weaker or far heavier parameters. No transfer is offered until the file was downloaded and chosen again and decrypted, and the decrypted note matches the sealed one.

**Funding order** (`funding/flow.ts`). Secret sealed and stored, backup downloaded and test-restored, vault re-checked, `sending` stored, wallet asked, hash stored, receipt read. A crash anywhere leaves a note that can be found again from its deposit receipt (Notes tab, with the transaction hash).

**Acceptance** (`chain/receipt.ts`). From the `deposit` event of the user's own transaction: note ID, commitment, denomination and payout account must all match. The wallet's receipt tree is used when it has the event. Otherwise the app asks the RPC provider for the transaction and tells the user what that shows. `note_status` is never called and no nullifier is queried; a test checks the wire.

**Wallets** (`wallet/`). NEAR Connect 0.11.4 injects a `<style>` element, writes `innerHTML` and builds a `srcdoc` iframe with an inline `<script>` for every wallet. None of that can run under the app's policy (`script-src 'self'`, `style-src 'self'`, Trusted Types), so the library runs in `wallet-host.html`: a page of this origin, embedded with `sandbox="allow-scripts allow-popups"` (no `allow-same-origin`) and served with its own, relaxed policy. It has an opaque origin and cannot read the app's storage or DOM. The app and the frame talk by `postMessage`; each checks the other's window and origin. The manifest is a constant object, so no manifest is fetched. Each executor is served from this origin, and `pinned-fetch.ts` answers NEAR Connect's request for it only after the bytes match the SHA-256 in `pins/executors.json`. The app checks the NEP-413 signature a wallet returns at sign-in.

Vendored executors are snapshots of third-party code (Meteor's is 2 MB). They can fall behind a wallet's own changes; update them on purpose, with a diff (`pins/executors.json` says how).

## Wallet checks

CI drives the sandbox path with a test executor and the injected test wallet. Two opt-in tests load the real, vendored executors and reach the wallets' servers:

```sh
PROBE_REAL_WALLETS=1 npx playwright test --project=chromium real-wallets
```

They show that Intear's connect chooser renders in the frame under the production policy, and that Meteor's dialog renders and its Meteor Web button opens the testnet login. Not tested here: signing in with a real account, `signMessage` and a transaction. Do that by hand with Meteor and Intear on testnet. The hosts each wallet needs are in `wallet-hosts.ts`; they came from the policy violations seen at load, so extend them from what the full flow reports. Meteor's interface also asks for two web fonts from third-party hosts. The frame's policy refuses them and the dialog falls back to a system font.

## What the app does not promise

- It cannot hide the user's IP address from the operator, an RPC provider or 1Click. It says so on every screen and points to Tor Browser or `nymirad`.
- A compromised host could serve a different app, and nothing in the browser checks the bundle. Hosting and `nymirad verify-web` (#39) narrow that; they do not remove it.
- Anonymity on testnet is small and made of test notes.
- The web app cannot use Tor and cannot bind the gateway's TLS certificate. The operator, NEAR AI's gateway and the PCCS see the user's IP address and the time. It cannot verify NVIDIA's GPU evidence (see Chat).
- The main thread never holds a plain note secret, but the worker's memory does, and a script that can run in the page can ask the worker to do what the page can do. The strict policy is what keeps such a script out; see the hosting ticket.
- Real Meteor and Intear wallets were not driven in the tests (see the pull request). The sandbox path is tested with a test executor.

## Chat

One conversation is one lease on one note (`chat/session.ts`, shown by `ui/Chat.tsx`). The order is fixed:

1. The vault tripwire passes, the operator posts no emergency notice, and the vault lists the lease class (0.10 USDC, or 0.50 USDC with the "long job" option, off by default).
2. The model is attested before anything is encrypted (below).
3. `/v1/status` is read and its nonce signature is checked with the state key from the pins (the zkapi Schnorr check, in `nymira-web-wasm`). A stale nonce, a missing or wrong signature, or lease parameters that differ from the pinned `config()` stop the chat, and no proof leaves. `request_time` is the signed operator time rounded down to 60 s.
4. The tree is read with `get_leaves` (pages of at most 100) at one final block. The wallet core refuses it unless it reproduces `current_root`.
5. The worker makes the request proof. The proving key loads into the worker when the page opens, and its SHA-256 is checked first. The proof for the first lease is made while the user types, and set aside again if the user leaves without sending. The sealed state, which now records the bound, is stored before the request leaves.
6. The lease request carries the hashcash header when `puzzle_difficulty` is above 0 (solved in the worker). `root_stale` rebuilds the tree, makes a new proof and retries once. Other errors are shown by their `code` (`describeOperatorError`); capacity and provider rate limit have different texts.
7. Each message is end-to-end encrypted to the model's attested key (`chat/e2ee.ts`, the suite `nymirad` uses), with every field encrypted, canonical model ids and usage always requested. `max_tokens` is `(bound × 0.9 − input estimate) / output price`. Non-streamed is the default. Streaming is an option and is expected to come back gateway-signed.
8. Leaving the conversation (or the button) retires the lease. The app waits for the operator's signed successor state and installs it after the wallet core's checks. It verifies the signature of each answer against the attested keys, recomputes the charge from the signed usage and the pinned values, and shows the badge: "model-signed response", "gateway-signed response" or "not verified" with the reason. A charge more than one step above the recomputed one is a warning. The signatures exist only once the operator has settled the lease, so the badge appears then.
9. A backup is written to a chosen file when the browser has File System Access. Otherwise a download is prompted, after every retire above 0.50 USDC and after every tenth lease below it. The sealed payload carries the lease records (root, leaf count, snapshot block, bound class, times, charge, lease id, model, sampling, message count and size, token usage; never content), and the backup carries them.

### What the attestation check covers

`chat/attest/` follows the checks `nymirad` runs. It verifies the signed policy (Ed25519, pinned signer, strict `UpToDate` TCB, minimum version). Then, for the model and for the gateway, it checks the Intel TDX quote (`dcap-qvl` compiled to WASM, collateral from Phala's PCCS because Intel's own service sends no CORS headers), the debug flag, the nonce and signing key in the report data, the event log replayed to RTMR3, the compose file against `mr_config_id` and the deployment against the policy. For the model it also checks that the key chats go to is the attested signer. This is tested on the evidence NEAR AI served on 2026-11-02 (`test-fixtures/near-ai-recorded/`), at the time it was recorded, including a real `provider_tee` signature.

What a browser cannot do:

- **NVIDIA GPU evidence.** NRAS refuses cross-origin requests (checked with `curl`: "Invalid CORS request" on the POST), so the app checks only that the GPU payload carries the nonce. The result says `nonce_only`, and the chat screen says the GPU evidence itself is not verified here. `nymirad` verifies it.
- **The gateway's TLS certificate.** A browser cannot read or pin it, so a gateway signature shows who signed, not which server answered. The badge area says so.
- `mr_seam`, `xfam` and the TD attributes other than debug are covered by the quote's signature, but the policy does not name them.

The mock profile's reports carry a test quote (`chat/attest/testquote.ts`) so Playwright can drive the checks that come after the quote. The mock build alone imports it, and `check:bundle` refuses its marker string in a deployed bundle. The same goes for the dev-setup wallet core (`zkapi-browser-dev`) and the development proving key (`dist/setup/`).

### Build and test notes

`build-wasm.sh` builds five modules: the ceremony-setup wallet core (`zkapi-browser`), the dev-setup one (`zkapi-browser-dev`, mock profile), `near-codec`, `web-checks` (status signature and Intel quote) and `web-checks-test` (a test operator: its state key and signed responses; Node tests only). The unit tests run a real proof with the dev setup (about 10 s).

Profiles with no published setup (`testnet`, `mainnet`) carry `provingKey: null`, and the Chat screen says chat is off. Fill it with the setup's key URL and SHA-256 when the setup is published.

## For the next tickets (#38 close and exit, #39 hosting)

- `ChatSession` (`chat/session.ts`) owns a note's sealed payload during a conversation. Close and exit act on the note when no lease is open. `session.view()` and the worker operation `readNote` give the balance, `boundAtState`, whether a request is pending, the lease in progress and the finished records.
- Worker operations (`worker/protocol.ts`): `loadProver`, `prepareLease`, `leaseOpened`, `recordExchange`, `completeLease`, `discardPending`, `readNote`, `solvePuzzle`. For #38, add `prepareWithdrawal` in the same style (the wasm export is `browser_prepare_withdrawal`; it needs the withdrawal key, which loads like the request key). Exit with concession 0 when no request left the state and with `boundAtState` otherwise. Refuse to start a lease after an exit proof left the worker.
- Sealed payload shape: `{state, extra}`. `extra` is JSON text of `NoteExtra` (`note/lease.ts`): `journal` (a prepared request without an installed response), `active` (the lease in progress, with its token) and `leases` (finished records). The backup carries both (`BackupNote.extra`).
- `pending` in `readNote` means a lease request was proven, and maybe sent, and its response is not installed. `ChatSession.end()` resolves it. A note in that state has to be settled before anything else.
- `chat/runtime.ts` `chatDeps()` builds the dependencies (operator client, policy, quote verifier, signature check). `warmProver()` is the proving key load. The hosts the chat connects to come from the profile (`gatewayUrl`, `pccsUrl`, `operatorUrl`) through `headers.ts`.
- Tests: `e2e/chat-support.ts` has `fundNote` (funding through the UI, then the tree views the app reads), `scriptTree` and a worker-scoped `stack` (`MockStack`, `e2e/mock-operator.ts`) with scenario switches: puzzle, `root_stale`, limits, an inflated charge, signature mode, TCB, an unbound key, a bad status signature, an emergency notice. A close and exit test can extend `MockStack` with `/v1/close` and the clearance route.

From the funding ticket, still true:

- `walletClient` (`wallet/client.ts`): `connect(id)`, `send(receiver, action)` for any function call (`FunctionCallAction`), `connected.accountId`. #38 sends `mutual_close`, `request_exit`, `finalize_exit` and `claim_payout` through `send`; `GAS` and the outcome handling are the caller's.
- `makeBackupFile`, `loadNotes`, `backupScope` (`funding/flow.ts`): the backup after every retire and close is `makeBackupFile` plus `download`. Backup records carry whatever `NoteRecord` and the sealed payload carry; add the lease record to the payload (not to the public fields) so it travels with the backup.
- Header set: `headers.ts` is the only place that knows the policy. Keep new hosts there.
