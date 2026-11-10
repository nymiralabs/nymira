# web

[README](../README.md) · [Website](https://nymira.pages.dev) · [Documentation](https://nymira.pages.dev/#docs) · [Try it in 20 minutes](../JUDGES.md) · [Threat model](../THREAT_MODEL.md) · [Releases](https://github.com/nymiralabs/nymira/releases)

The Nymira web app: a landing page, a documentation page (`#docs`) and the app screens Chat, Fund, Notes, Close and Agents. A URL hash such as `#fund` opens a screen. Fund can also show cross-chain quotes from 1Click. Unknown paths show a 404 page. Release 0.1.3 is the one served at <https://nymira.pages.dev>.

The app is a static bundle: Vite 8, React 19, TypeScript. It has no backend and loads no script, font or analytics from another host. It ships two self-hosted IBM Plex fonts. Everything below says what it does and what it cannot promise.

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
| `npm run check:deps -- <lock> <exceptions>`          | Minimum release age, registry hash and provenance of every locked package (`scripts/check-deps.mjs`).                                                             |
| `node scripts/integrity.mjs assemble\|verify\|hash`  | Builds the signed-release site from a bundle, verifies one against a public key (`INTEGRITY.md`).                                                                 |
| `scripts/repro-check.sh`                             | Builds the bundle twice from one commit and compares the hashes.                                                                                                  |
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
    note/exit.ts                   the exit proofs (ownership, close, escape) and the exit lock
    leave/                         flow.ts (close, exit, finalize, claim), guard.ts (expiry rules),
                                   store.ts (what the screens share), text.ts (fixed wording)
    worker/                        the note worker that runs note/core.ts, its protocol and client
    backup/                        format.ts (the file), ics.ts (calendar reminder)
    funding/                       flow.ts (the funding steps), oneclick.ts and quote-check.ts (quotes
                                   and their signature checks), intents.ts (funding from another chain),
                                   watch.ts (finds a deposit without the wallet)
    chat/                          session.ts (one conversation: attest, lease, encrypted chat,
                                   settle), operator.ts (HTTP client and error texts), e2ee.ts,
                                   gate.ts (status nonce, parameters), charge.ts, models.ts,
                                   puzzle.ts, backup-policy.ts, runtime.ts (page wiring),
                                   attest/ (policy, evidence checks, Intel quote, signatures)
    privacy/                       the set size rule (setsize.ts), cover figures, panel text;
                                   strings.ts imports ../shared/privacy-strings.json
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

| Profile   | Use                                                                | Vault pins                                                                  |
| --------- | ------------------------------------------------------------------ | --------------------------------------------------------------------------- |
| `testnet` | The hackathon deployment.                                          | `vault.nymira.testnet`: code hash, `vk_hashes`, `config()` and access keys. |
| `mainnet` | The later launch.                                                  | `null`. No public mainnet vault during the hackathon.                       |
| `mock`    | Playwright only. Loopback RPC, mock vault, a test wallet executor. | Fixture.                                                                    |

`check:bundle` runs on the testnet and mainnet bundles in CI. The app also refuses to run from an origin other than the profile's `appOrigin`, and makes no request when it does.

## Trust boundaries

**Vault tripwire** (`chain/vault.ts`). At load and before every deposit the app compares the vault's code hash, `vk_hashes()`, every `config()` value except `tvl_cap` and `current_tvl`, and its access keys with the pins. Any difference, or a vault it cannot reach, stops funding and shows what differs. The deposit button re-runs the check, so a vault changed after the page loaded is caught.

**Secrets** (`note/core.ts`, `worker/`). A note secret is made in a Web Worker by the vendored zkapi wallet core. The worker seals it with AES-256-GCM under a non-extractable key and returns only ciphertext and the commitment. The page's main thread never holds a plain secret. The sealing key comes from an Argon2id passphrase (hash-wasm, 64 MiB, 3 passes, 1 lane) or from a WebAuthn PRF output through HKDF. By default it lives in memory for the session. If you switch on "Stay unlocked in this tab", the non-extractable key is also kept in a second IndexedDB until the tab closes, so a reload of that tab stays unlocked. IndexedDB holds the public record and one sealed payload. The payload is bound to the note by its additional data, so a sealed payload cannot be moved to another note.

**Backup** (`backup/format.ts`). A JSON envelope with the same Argon2id parameters and an AES-256-GCM body. The passphrase has at least 12 characters. The app refuses a backup whose memory or pass count is below the app's own values or more than four times them, or whose lane count is not 1. No transfer is offered until the file was downloaded and chosen again and decrypted, and the decrypted note matches the sealed one.

**Funding order** (`funding/flow.ts`). Secret sealed and stored, backup downloaded and test-restored, vault re-checked, `sending` stored, wallet asked, hash stored, receipt read. A crash anywhere leaves a note that can be found again from its deposit receipt (Notes tab, with the transaction hash).

**Acceptance** (`chain/receipt.ts`). From the `deposit` event of the user's own transaction: note ID, commitment, denomination and payout account must all match. The wallet's receipt tree is used when it has the event. Otherwise the app asks the RPC provider for the transaction and tells the user what that shows. `note_status` is never called and no nullifier is queried; a test checks the wire.

**Wallets** (`wallet/`). NEAR Connect 0.11.4 injects a `<style>` element, writes `innerHTML` and builds a `srcdoc` iframe with an inline `<script>` for every wallet. None of that can run under the app's policy (`script-src 'self'`, `style-src 'self'`, Trusted Types), so the library runs in `wallet-host.html`: a page of this origin, embedded with `sandbox="allow-scripts allow-popups allow-popups-to-escape-sandbox"` (no `allow-same-origin`) and served with its own, relaxed policy. It has an opaque origin and cannot read the app's storage or DOM. The app and the frame talk by `postMessage`; each checks the other's window and origin. The manifest is a constant object, so no manifest is fetched. Each executor is served from this origin, and `pinned-fetch.ts` answers NEAR Connect's request for it only after the bytes match the SHA-256 in `pins/executors.json`. The app checks the NEP-413 signature a wallet returns at sign-in.

Vendored executors are snapshots of third-party code (Meteor's is 2 MB). They can fall behind a wallet's own changes; update them on purpose, with a diff (`pins/executors.json` says how).

## Wallet checks

CI drives the sandbox path with a test executor and the injected test wallet. Two opt-in tests load the real, vendored executors and reach the wallets' servers:

```sh
PROBE_REAL_WALLETS=1 npx playwright test --project=chromium real-wallets
```

They show that Intear's connect chooser renders in the frame under the production policy, and that Meteor's dialog renders and its Meteor Web button opens the testnet login. Not tested here: signing in with a real account, `signMessage` and a transaction. Both were tried by hand on testnet; Meteor works there. The hosts each wallet needs are in `wallet-hosts.ts`; they came from the policy violations seen at load, so extend them from what the full flow reports. Meteor's interface also asks for two web fonts from third-party hosts. The frame's policy refuses them and the dialog falls back to a system font.

### Wallets that do not answer

A wallet can approve a transaction and never return (Intear did when its own archival RPC answered 400). While a request is out, the app reads the vault: funding looks for a new note of the same size whose leaf matches its own commitment (no `note_status` call), and a close looks for the note closed in `note_records`. If the vault shows the effect, the flow finishes without the wallet. A refusal from the wallet counts as "rejected" only if the vault shows nothing for 20 seconds after it. After 3 minutes with no answer and nothing on chain, the app stops waiting and points to the Notes tab and "Ask the vault whether the close landed". A note found this way has no receipt, so the app cannot check its payout account and the screen says so. Exit, finalize and claim are not watched this way.

After a reload the wallet connection is gone (the wallet frame keeps nothing on the device). The app says so and offers "Reconnect with ...". `e2e/silent-wallet.spec.ts` covers a wallet that never returns, a window closed after the send, a real cancel and a dead wallet, for funding and for closing, and the reload notice.

## What the app does not promise

- It cannot hide the user's IP address from the operator, an RPC provider or 1Click. It says so on every screen and points to Tor Browser or `nymirad`.
- A compromised host could serve a different app, and nothing in the browser checks the bundle. The signed manifest and `nymirad verify-web` narrow that; `verify-web` catches a different app only if the user runs it and the host does not show it a clean copy (cloaking). The footer's bundle hash comes from the same host, so it proves nothing by itself.
- Anonymity on testnet is small and made of test notes.
- The web app cannot use Tor and cannot bind the gateway's TLS certificate. The operator, NEAR AI's gateway and the PCCS see the user's IP address and the time. It cannot verify NVIDIA's GPU evidence (see Chat).
- The main thread never holds a plain note secret, but the worker's memory does, and a script that can run in the page can ask the worker to do what the page can do. The strict policy is what keeps such a script out; see `INTEGRITY.md`.
- Real Meteor and Intear wallets are not driven in the automated tests; the sandbox path is tested with a test executor. Both were tried by hand on testnet, and Meteor works there.

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
9. A backup is written to a chosen file when the browser has File System Access. Otherwise a download is prompted, after every retire while the balance is above 0.50 USDC, after every tenth lease below that, and when the balance reaches zero. The sealed payload carries the lease records (root, leaf count, snapshot block, bound class, times, charge, lease id, model, sampling, message count and size, token usage; never content), and the backup carries them.

### What the attestation check covers

`chat/attest/` follows the checks `nymirad` runs. It verifies the signed policy (Ed25519, pinned signer, strict `UpToDate` TCB, minimum version). Then, for the model and for the gateway, it checks the Intel TDX quote (`dcap-qvl` compiled to WASM, collateral from Phala's PCCS because Intel's own service sends no CORS headers), the debug flag, the nonce and signing key in the report data, the event log replayed to RTMR3, the compose file against `mr_config_id` and the deployment against the policy. For the model it also checks that the key chats go to is the attested signer. This is tested on the evidence NEAR AI served on 2026-11-02 (`test-fixtures/near-ai-recorded/`), at the time it was recorded, including a real `provider_tee` signature.

What a browser cannot do:

- **NVIDIA GPU evidence.** NRAS refuses cross-origin requests (checked with `curl`: "Invalid CORS request" on the POST), so the app checks only that the GPU payload carries the nonce. The result says `nonce_only`, and the chat screen says the GPU evidence itself is not verified here. `nymirad` verifies it.
- **The gateway's TLS certificate.** A browser cannot read or pin it, so a gateway signature shows who signed, not which server answered. The badge area says so.
- `mr_seam`, `xfam` and the TD attributes other than debug are covered by the quote's signature, but the policy does not name them.

The mock profile's reports carry a test quote (`chat/attest/testquote.ts`) so Playwright can drive the checks that come after the quote. The mock build alone imports it, and `check:bundle` refuses its marker string in a deployed bundle. The same goes for the dev-setup wallet core (`zkapi-browser-dev`) and the development proving key (`dist/setup/`).

### Build and test notes

`build-wasm.sh` builds five modules: the ceremony-setup wallet core (`zkapi-browser`), the dev-setup one (`zkapi-browser-dev`, mock profile), `near-codec`, `web-checks` (status signature and Intel quote) and `web-checks-test` (a test operator: its state key and signed responses; Node tests only). The unit tests run a real proof with the dev setup (about 10 s).

The `testnet` profile carries both ceremony keys, served from the bundle under `setup/` and checked by SHA-256. The `mainnet` profile has no vault and carries `provingKey: null` and `withdrawalKey: null`, so Chat and Close say they are off there.

## Privacy panel

The panel opens from the Chat and Notes screens (`ui/Privacy.tsx`). The left column shows the deposit from the record this browser keeps and asks no node for it; the explorer link opens when clicked. The right column lists the operator's view of each lease, with the operator's published retention policy labelled as something the client cannot check.

Each lease record carries a set size, counted in the browser when the lease opens: the note IDs below the leaf count under the proof's root that are `Active` with an expiry at or after the request time, read with `note_records` at the proof's block from each configured RPC endpoint on its own (the lower figure is kept and a difference is shown). The team list is the one pinned in the bundle (`crates/nymira-operator/team-note-ids.json`) plus any IDs the operator lists; the operator's list can only lower N. A record made before the figure existed shows none. The "now" figure applies the same rule at the current final block, with the last operator list a lease stored.

The sentences `nymirad status` also prints are in `shared/privacy-strings.json` at the repository root, which both programs include; `test/privacy.test.ts` pins its hash against the one in `crates/nymirad/src/strings.rs`. `shared/set-size-cases.json` is run through the Rust rule and this one. Time since the deposit starts at `depositedAt`, the moment the app saw the deposit accepted, so the gap shown can be shorter than the real one. A note without that field (made before it existed) falls back to its creation time, which is earlier than the deposit, so its gap can look longer than it was.

The middle column says what joins the chain view and the operator view. While the note is open it says the proofs are designed to leave no zero-knowledge link and lists what can still link the leases. Once a close, a clearance or an exit has started on the note (`leave` is set, or the sealed payload holds an exit lock), it says the leases are now tied to the note ID.

"Check reuse" reads `GET /v2/nullifiers/{n}` from the operator for the request nullifier of the last settled lease, which the sealed payload keeps in `last` together with that request. While the request is inside 300 seconds of its `request_time`, and no exit proof has left the device, the control sends it again to `POST /v2/openrouter/leases` and shows the refusal as "the operator refuses" with the operator's text in a labelled plain-text box, cut to 300 characters. The panel says the refusal is the operator's rule, backed by its replicated records, and not a vault check. It never asks an RPC node about the nullifier and never calls `is_nullifier_used`. If an operator did accept the request, the answer is a lease and the panel says so. The page also lists what leaving reveals and which observers see what; the observer sentences are the ones in the landing drawing's description. All of these sentences are in `shared/privacy-strings.json`.

## Close and exit

The Close screen (`ui/Close.tsx`, flows in `leave/flow.ts`) leaves a note by one of two ways. Both follow `nymirad` (`gateway/close.rs`, `expiry.rs`), and the numbers are the same.

**Close.** The worker makes a kind-2 ownership proof (valid 3,300 s) and the app posts it to `POST /v2/withdraw/clearance`. The operator signs a clearance only for a note with no lease open. The worker checks that signature against the pinned clearance key and makes the kind-1 close proof, valid for 3 hours. `mutual_close` goes out from the user's wallet (no close fee), or, for a user without NEAR, to the operator's `POST /v1/close`, which sends it and takes the close fee. The app then asks the vault (`note_records`) whether the note shows `Closed`; a failed payout receipt does not make a close look failed, because the vault, not the outcome, decides.

**Exit.** `request_exit` needs no clearance. The escape proof concedes the state's `bound_at_state`: 0 when no request proof was made at the state, the stored bound otherwise (the wallet core records the bound before a request leaves). A request that was proven and maybe sent is not guessed at: the exit concedes its bound. The exit window runs 7 days by the chain's clock. The screen shows the deadline and enables `finalize_exit` once the chain's latest final block is past it. "Claim payout" calls `claim_payout` for the payout account when a payout is pending.

**The lock** (`note/exit.ts`). The wallet core's exit lock lives in the sealed payload (`extra.exit`) with every proof that left or may have left. It is written before a proof is returned and the page stores it before the proof is sent. While it exists the worker refuses `prepareLease`, so no request is made on a note after an exit proof left the worker; the Chat screen hides the note and `ChatSession` refuses it too. A mutual lock can switch to escape; an escape lock does not go back to mutual. One ownership proof is live at a time and a retry resends it. The app has no cancel: once a close or exit starts, the note leaves.

**Times.** The ownership proof uses the operator's signed status time (so a wrong local clock cannot push it past the operator's 3,300 s window). The close and exit proofs and the exit deadline use the chain's final block time, which is what the vault judges. The local clock is used for the expiry guard only.

**Gas.** The wallet account must hold about 0.02 NEAR before any proof is made, so a half-started exit never waits on gas. `request_exit`, `finalize_exit` and `claim_payout` can be sent by any account; the screen says so.

**What the vault tripwire does here.** It does not block leaving: a vault that changed is exactly when a user may want out, and the proofs bind the chain, the vault address and the pinned keys.

**What it shows on chain and to whom** is in `THREAT_MODEL.md` ("What closing and exiting show, from the web app") and on the screen: the note ID, total spend and payout account next to the sending account; the RPC provider sees the IP address with each read. The web app adds no delay before a close (`nymirad` waits one to twenty minutes).

### Expiry guard (`leave/guard.ts`, `leave/store.ts`, `ui/Watch.tsx`)

A note that reaches its expiry goes to the treasury. The banner shows every open note within 14 days of expiry. With the guard on (the default; a switch on the Close screen) and the app open:

- 5 days or fewer: no new lease on the note (the Chat screen and `ChatSession` both refuse), and a mutual close starts;
- 3 days or fewer, when the close did not go through: `request_exit` starts. The guard runs the exit in the same pass as the failed close, as `nymirad` does;
- "starts" means the proofs are made and the wallet is asked. Nothing is sent that the wallet did not sign. A refused signature or a failure waits an hour before the next try;
- the guard needs the app unlocked and a wallet connected. Without a wallet it says what is waiting. It cannot run while the app is closed;
- closing at the end of a session is off by default. When switched on, ending a conversation starts a close that waits for the wallet. A browser cannot sign while a page unloads.

On the operator's emergency notice (`/v1/status`, read when the Chat or Close screen opens and every five minutes), the app opens no lease, shows the notice on every screen and suggests an exit for each open note. The operator signs no clearance in an emergency, so the Close button says to exit.

After a close or a finalized exit the app prompts for an encrypted backup and tests it: the file must be downloaded and chosen again, and its note must match the sealed one.

### Tests and what is real

`leave.spec.ts` (Playwright, Chromium; the close test also on WebKit) and `test/exit.test.ts` (Node, the real wallet core with the development setup).

Real: the app, every proof (development setup), the operator mock's verification of the ownership proof and the clearance it signs with a real clearance key, the test wallet's signed transactions to the fake RPC, and the check of every `mutual_close`, `request_exit` and `/v1/close` call under the development withdrawal verifying key from its 16 statement words, the way the vault checks them.

Mocked: the vault. The fake RPC runs no contract, so after each call the test scripts the views the vault would show (`note_records`, `exit_requests`, `pending_payout`). The chain's clock is the fake chain's, moved by the test: the sandbox cannot move seven days. The interop test runs the web-built `mutual_close` and `request_exit` calldata on a sandbox vault, and closes from the deployed app ran by hand on the testnet vault.

## Hosting and integrity

The full rules, byte by byte, are in [`INTEGRITY.md`](INTEGRITY.md). In short:

- **Where.** Cloudflare Pages serves `https://nymira.pages.dev` with the header sets of `headers.ts`: the app's strict policy, the wallet frame's relaxed one, and `Access-Control-Allow-Origin: *` on `/assets/` and `/executors/`. The build writes the Pages `_headers` file (`<outDir>-meta/_headers`, no overlapping rules, because Pages joins a header set by two rules with a comma). GitHub Pages is a mirror with the same bytes and none of the headers; the app refuses to run there. The bundle loads no font, script or analytics from another host. Cloudflare's analytics and response-rewriting features stay off.
- **Release.** The build writes `<outDir>-meta/headers.txt`; `scripts/integrity.mjs assemble` adds `integrity/files.txt`, `integrity/headers.txt`, `_headers` and an unsigned `manifest.json` `{version, bundle_hash, header_set_hash}`. A person signs `manifest.json` offline with minisign; the signed pair is committed under `releases/`. `integrity.mjs verify` (CI and the deploy workflow) fails an unsigned or mismatching site, or a policy with `unsafe-eval`.
- **Reproducible.** `scripts/repro-check.sh` builds twice and compares; CI runs it (`web-integrity`). `Dockerfile.repro` and `scripts/rebuild-in-container.sh` rebuild in a pinned container for an independent check.
- **Dependencies.** Lockfile installs with scripts off; `scripts/check-deps.mjs` and `npm audit signatures` run in a read-only CI job with no secrets and no cache. `check:bundle` refuses test-only content, the dev-setup wallet core and keys in the testnet and mainnet bundles. `scripts/check-release-binary.sh` (repository root) shows the release `nymirad` has no `mock` feature and no demo strings.
- **Bundle hash.** Printed in each release's notes (`releases/RELEASE_NOTES_TEMPLATE.md`) and shown in the app footer from `/manifest.json`.

The release public key is pinned in `pins/release-signing.pub` (minisign key ID `934C176AF34D794D`, SHA-256 `989781b59bd85e30fb99555ef7551b1a1c097d4eb4e2be0159d284a579dea1ff`) and compiled into `nymirad`. Releases 0.1.0, 0.1.1, 0.1.2 and 0.1.3 of the testnet profile are signed with it (`releases/`). The places the fingerprint is published are listed in `INTEGRITY.md`, with the public log entry made for 0.1.2 (the one for 0.1.3 follows the deploy). The mainnet release gets its own key and a new pin.

**What remains.** A compromised host could serve a different app. The signed manifest and `nymirad verify-web` let a user compare what the host serves with what the key signed, but only if the user runs it, and not if the host shows `verify-web` a clean copy. Reading the footer hash is no check.

### Notes for contributors

- No new host: the Close screen talks to the operator, the RPC endpoints and the wallet frame, all already in `headers.ts`. The withdrawal proving key is a second file next to the request key (`profile.withdrawalKey`); if its URL is not on this origin, its host goes into `connect-src` too.
- `headers.ts` is the only place that knows the policy. Keep new hosts there.
- The mainnet profile has no dedicated archival RPC endpoint (the archival read uses the profile's last RPC URL), and NRAS is not in `connect-src` because it refuses browser requests (see Chat).

Also true: `walletClient.send(receiver, FunctionCallAction)` is how every vault call goes out, and the sealed payload is `{state, extra}` as JSON text read with `note/rawjson.ts`, never `JSON.parse` for numbers.
