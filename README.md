# Nymira

Private, prepaid API credits on NEAR.

**This is a testnet project with test funds.** The vault will hold Circle's testnet USDC, which has no value. The circuit has had no independent review, and zkapi does not use it on its own mainnet yet. The anonymity set on testnet is small and made of test notes, so a lease can be tied to a note more easily than it could be on a busy network. Read [THREAT_MODEL.md](THREAT_MODEL.md) before you trust any sentence here. Use of the deployment is under the [terms of use](TERMS.md).

## Live deployment

The public testnet vault is not deployed yet. This table is filled in when it is, and every value below can be checked against the chain:

| What | Value |
|---|---|
| Vault account | not deployed yet |
| Vault code hash (reproducible build) | not deployed yet |
| `vk_hashes()` next to the ceremony transcript hashes | not deployed yet |
| `view_access_key_list` of the vault | not deployed yet |
| Web bundle hash | not published yet |
| Keys the vault uses | not deployed yet |

The proving and verifying keys come from a setup run by the team alone (the log, scripts and the accepted contributions are in [nymira-ceremony](https://github.com/nymiralabs/nymira-ceremony)). They are sound if at least one contributor really was honest, and the team cannot prove that: if every contribution was dishonest or its machine was compromised, proofs can be forged and the vault emptied. zkapi's development keys, whose secrets are public, never run on the final public vault or on mainnet. Until the final vault is up, development and rehearsal keys are used and no privacy claim rests on them. A deposit fee is read from the vault's `config()`; on testnet it is a placeholder in test USDC and is not revenue.

The proving keys are release assets of the ceremony repository (`request.pk`, `withdrawal.pk`, tag `final`). The web app serves them from its own signed bundle and checks their SHA-256 before use. For `nymirad`, download both files into one directory and pass it once: `nymirad init ... --setup-dir DIR`. `init` refuses files whose hash differs from the one compiled into the binary, and without the directory chat and close stay off.

## Limits

These are the rules of the vault contract and of the testnet deployment. Once the vault is deployed, its `config()` shows the values. The operator cannot send your money to another account or block your exit: with `request_exit` you leave to your payout account after 7 days, and the operator can deduct at most the bound of the one lease in flight. If you lose your latest wallet state, an exit from an older state can be defeated and you can lose the remaining balance, so keep your encrypted backup. A note expires 30 days after the deposit; seven days after that, the treasury can take it unless an exit is pending. The team can still update the vault code, and open user deposits in the vault are capped at 100 USDC (test USDC on testnet; the cap bounds open deposits, not what the provider is paid).

The first sentence holds while you keep your latest state; see the first point below. The cap is a vault parameter that the owner can change after a 48-hour delay announced by an event.

- An exit from an old state can be defeated: if you lose your latest state, the operator can answer the exit with a later request and keep what it charged, up to the whole remaining balance. Keep a backup.
- A note expires at the end of the UTC day 30 days after the deposit. The treasury can take it only 7 days after the later of that expiry and the last exit activity on the note, and never while an exit is pending; until then an expired note can still exit and finalize.
- If the operator is offline for a whole 7-day exit window, a user can leave with an older, higher balance than the real one. The loss falls on the operator, and the 100 USDC cap on open deposits bounds it.
- Closing shows your note ID, total spend and payout account on chain, next to the account that sends the close.
- The web app cannot use Tor, so the operator and the RPC provider see your IP address. Use Tor Browser or `nymirad` for IP protection.
- A compromised host could serve a different web app. `nymirad verify-web` catches that only if you run it and the host does not cloak, so it does not replace trusting the host you load the app from.
- Everything the other parties see is in [THREAT_MODEL.md](THREAT_MODEL.md).

Nymira reuses the zero-knowledge circuits of [zkapi](https://github.com/ethereum/zkapi) (`ethereum/zkapi`), vendored at a pinned commit in `vendor/zkapi/`. `NOTICE` lists the exact commit and every patch we apply. `PINS.md` records the pins and the toolchain.

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

1Click sees the source wallet, note commitment, payout account and IP address on this path. Its quote signature identifies the issuer; it does not guarantee delivery. The client accepts a note only from a matching final vault receipt. Testnet offers signed quotes labelled "quote only" over Tor and makes no Intents payment. Use Circle's faucet for testnet USDC.

The CLI integration tests deliver through the open-source Verifier, release v0.4.2, and through the demo stand-in. They cover a pending-backup restore, renewed consent, receipt acceptance and a lease on the new note. No live cross-chain swap is claimed. The live destination-signer lookup and batching shape still need the mainnet probe; no mainnet vault is pinned in this build.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option. Code taken from other projects is listed in [NOTICE](NOTICE).
