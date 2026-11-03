# web

The Nymira web app. This ticket (#36) builds funding: landing, wallet connection, vault checks, the Fund screen, note storage, backup and restore. Chat (#37), close and exit (#38) and hosting (#39) build on it.

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

| Command                                              | What it does                                                                                |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `npm run build` / `build:mainnet` / `build:mock`     | One bundle per profile (below).                                                             |
| `npm run check:bundle -- dist testnet`               | Fails when a deployable bundle holds test-only content (loopback, mock vault, test wallet). |
| `npm run check:executors`                            | Fails when a vendored wallet executor and its pin disagree.                                 |
| `npm run pin-vault -- --rpc <url> --vault <account>` | Prints the `pins` object for a profile from a deployed vault.                               |
| `npm run test:unit`                                  | Vitest, including the real zkapi wallet core in Node.                                       |
| `npm run test:e2e`                                   | Playwright on Chromium; WebKit runs the tests tagged `@webkit`.                             |

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
                                   token.ts (payout checks), receipt.ts (deposit event)
    wallet/                        host.ts (runs NEAR Connect in a sandboxed frame), client.ts
                                   (the app's side), manifest.ts, pinned-fetch.ts, nep413.ts
    crypto/                        kdf.ts (Argon2id), aead.ts (AES-GCM), wrap.ts (wrapping key,
                                   passphrase or WebAuthn PRF)
    store/                         db.ts (IndexedDB), types.ts (NoteRecord and its phases)
    note/core.ts                   everything that touches a plain note secret
    worker/                        the note worker that runs note/core.ts, its protocol and client
    backup/                        format.ts (the file), ics.ts (calendar reminder)
    funding/                       flow.ts (the funding steps), oneclick.ts (dry quotes)
    ui/                            React screens
    wasm/                          build output of build-wasm.sh (not committed)
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
- The main thread never holds a plain note secret, but the worker's memory does, and a script that can run in the page can ask the worker to do what the page can do. The strict policy is what keeps such a script out; see the hosting ticket.
- Real Meteor and Intear wallets were not driven in the tests (see the pull request). The sandbox path is tested with a test executor.

## For the next tickets

The pieces #37 and #38 build on:

- `PROFILE` (`src/profile.ts`): network, accounts, RPC URLs, pins, operator URL. Extend `headers.ts` `appCsp(profile, extraConnect)` with the NEAR AI gateway, PCCS and NRAS hosts when chat needs them.
- `rpc` (`ui/services.ts`) and `RpcClient` (`chain/rpc.ts`): views at the final block with endpoint fallback. `readVault`/`checkVault` (`chain/vault.ts`) give the pinned values for the operator check in #37.
- `walletClient` (`wallet/client.ts`): `connect(id)`, `send(receiver, action)` for any function call (`FunctionCallAction`), `connected.accountId`. #38 sends `mutual_close`, `request_exit`, `finalize_exit` and `claim_payout` through `send`; `GAS` and the outcome handling are the caller's.
- `NoteRecord` (`store/types.ts`): one record per note. After acceptance `kind` is `state` and the sealed payload holds zkapi's `BrowserNoteState` as JSON text (kept as text: parsing it rounds the u64 chain ID). Read it only through the worker: add an operation to `worker/protocol.ts` and `worker/note.worker.ts`, backed by a function in `note/core.ts`. Do not unseal on the main thread.
- `callWorker` (`worker/client.ts`): typed requests to the note worker. #37 puts the proving key and the request prover in the same worker (or a second one) and builds requests from the state there.
- `makeBackupFile`, `loadNotes`, `backupScope` (`funding/flow.ts`): the backup after every retire and close is `makeBackupFile` plus `download`. Backup records carry whatever `NoteRecord` and the sealed payload carry; add the lease record to the payload (not to the public fields) so it travels with the backup.
- Tests: `e2e/support.ts` has `newFundingWallet` (the test wallet plus the chain side of a deposit), `scriptChain` (the fake RPC's vault and token views), `prepare`, `backUpAndTest`, `pageSurfaces` and `expectNoSecret`. Chat tests extend the scripted chain and reuse the leak scan.
- Header set: `headers.ts` is the only place that knows the policy. Keep new hosts there.
