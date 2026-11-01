# web-testkit

Test doubles for the web tests: a NEAR wallet that signs without a browser extension, and the browser build of the NEAR codec. Both belong to the shared test harness (issue #86).

## Parts

**Test wallet** (`src/wallet.js`, `src/playwright.js`). `NearTestWallet` implements the `NearWalletBase` interface that NEAR Connect calls: `signIn`, `signInAndSignMessage`, `signOut`, `getAccounts`, `signMessage` (NEP-413, base64 signature), `signAndSendTransaction` and `signAndSendTransactions`. It holds an ed25519 test key, reads the nonce (`view_access_key`) and block hash (`block`) from the RPC URL you give it, signs a real transaction (`FunctionCall` and `Transfer` actions) and sends it with `send_tx`, waiting for `FINAL` unless you set `waitUntil`. Point it at the sandbox or at the fake RPC. `wallet.sent` lists every transaction it sent, so a test can assert that nothing was sent yet.

`installTestWallet(context, wallet)` puts it in every page of a Playwright context, before the first navigation. The key stays in the test runner; the page gets `window.nymiraTestWallet`, whose methods call back into the runner. The page also announces it with the `near-wallet-injected` event when NEAR Connect fires `near-selector-ready`, so the app finds it as an injected wallet. Only `FunctionCall` and `Transfer` are supported; other actions throw.

```js
const wallet = new NearTestWallet({ accountId: 'alice.test.near', rpcUrl: sandbox.rpcAddr });
await installTestWallet(context, wallet);
await page.goto(appUrl);
```

**Browser NEAR codec** (`crates/nymira-near-codec-wasm`, `src/codec.js`). A wasm-bindgen wrapper around `nymira-near-codec`. `build-codec.sh` builds it for `wasm32-unknown-unknown` and runs `wasm-bindgen-cli` 0.2.117 (installed if missing) into `codec-pkg/`, an ES module with its `.wasm`. The directory is a build output and is not committed.

Exports: `chainId(network)` (`"testnet"` or `"mainnet"`, returns a `BigInt`), `contractAddress(account)`, `destination(account)`, `unixSeconds(ns)`, `noteFields(noteId, deposit, expiry)`, `proofFromWire(wire)`, `verifyingKeyFromCanonical(bytes)`, `publicInputFromWire(word)`. Byte results are `Uint8Array`. A proof is `a || b || c` (256 bytes) and a key is `alpha || beta || gamma || delta || ic...` in the host little-endian format. Bad input throws.

- Node: `const codec = await loadCodec();`
- Page: `await serveCodec(context)` serves `codec-pkg/` and an empty page at `https://nymira.test/blank`; `await loadCodecInPage(page)` sets `window.nymiraCodec`. A web test that loads the codec from the app's own bundle needs neither.

## Self-tests

```sh
web-testkit/build-codec.sh          # once, and after changing the codec
cd web-testkit
npm ci
npx playwright install chromium     # once
npm test                            # all of the below
```

- `npm run test:codec` (`test/codec.test.js`): every vector in `crates/nymira-near-codec/VECTORS.md` (chain ids, contract addresses including the reduced one, destinations, the time rule, `noteFields`), the wire proof, public input and verifying key conversions with their refusals, and the real proofs in `crates/nymira-verifier/tests/fixtures/near-fixture.json`. The generator key fixture is checked in the Rust crate too, so both sides stay in step.
- `npm run test:wallet` (`test/wallet.test.js`): the wallet from plain Node against an in-test JSON-RPC stub. Signatures, nonces, block hash, actions and the NEP-413 message are checked.
- `npm run test:browser` (`test/browser.spec.js`): Playwright with Chromium. A blank page gets the injected wallet and signs a transaction whose signature verifies; the codec WASM in the page returns the same vectors.

The Rust side runs with `cargo test -p nymira-near-codec-wasm`.

## Used by

Web issues #36 (funding), #37 (chat), #38 (close and exit) and #39 (hosting and integrity), and the web golden path in the browser, through the Playwright mock-mode stack: the wallet signs the deposit, `mutual_close`, `request_exit` and `finalize_exit`, and the codec builds withdrawal and request proofs and converts proof bytes.
