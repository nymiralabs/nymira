# Nymira

Private, prepaid API credits on NEAR.

**This is a testnet project with test funds.** The vault holds Circle's testnet USDC, which has no value. The circuit has had no independent review, and zkapi does not use it on its own mainnet yet. The anonymity set on testnet is small and made of test notes, so a lease can be tied to a note more easily than it could be on a busy network. Read [THREAT_MODEL.md](THREAT_MODEL.md) before you trust any sentence here. Use of the deployment is under the [terms of use](TERMS.md).

## Live deployment

The vault runs on NEAR testnet with Circle's test USDC (token account `3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af`). Every value below can be checked against the chain.

| What | Value |
|---|---|
| Vault account | `vault.nymira.testnet`, deployed in transaction `G1LGwd1oxcrza7VSwNuGYf9xjaMeNJzYdEZ2EFamjQVY` (`new()` ran in the same transaction) |
| Vault code hash (reproducible build) | `71QXP9FT4eiRcaqjZtsHi7mXMCAexXjCFgQ338p8tur4` in base58; as SHA-256 of the wasm, `5940b853e51476c69ab2cd0ffba2fb76ffd3fedb621a2471c9ccd2bdb62ad4f5`. Built from the tag `vault-testnet-1` (commit `18107d876dd9d3912796f4b002e9a006c74b1152`) with `scripts/reproducible-smoke.sh` (cargo-near 0.22.0, rustc 1.99.0, image `sourcescan/cargo-near@sha256:51ab40efd798eae26c0dc71180909303f10bb4988b15ce3a18522c5be06e277e`). The on-chain hash was checked against the build with `view_account`. Two builds on one Mac (arm64) were equal. A build on another CPU architecture (amd64) has not been compared yet. |
| `vk_hashes()` next to the ceremony transcript hashes | On chain: request `0xf3092e0d550f696a6507f279862afb43d0c22663de56f1b47160b5db6fc6fa1b`, withdrawal `0x3469ebb3245101549343dbe5939427d3dc7706c314f281f9b5935416fc720c8d`. These equal the verifying key keccak256 values in the `final` release of [nymira-ceremony](https://github.com/nymiralabs/nymira-ceremony), whose transcript SHA-256 is `e75cae40c203b08f8b9e559a2fa4b18f464e709d68abe01318c2d8a7aa4b7e07` (beacon: NEAR mainnet block 222942364). Proving key SHA-256: `request.pk` `b5cdb12000cf51cf566375aadbc2a27cf875438f646f751fa857371cd6cb7022`, `withdrawal.pk` `89e03ac8f9d6e90d189e9a3eb6b0c9c0126e1b121e78955a11f0f70a44d0161d`. |
| `view_access_key_list` of the vault | One key, full access: `ed25519:DJc2C6WWveyUY1VvLEdUPjBggzKrMuEwx7GGp1BLHMze`. It is the team's, and it can replace the vault code (see [THREAT_MODEL.md](THREAT_MODEL.md)). |
| Vault config (`config()`) | Operator `operator.nymira.testnet`, owner `owner.nymira.testnet`, treasury `treasury.nymira.testnet`. Denominations 1 and 5 USDC, bound classes 0.10 and 0.50 USDC, charge step 0.001, minimum charge 0.002, deposit fee 0.05 USDC, close fee 0.03 USDC, exit delay 604800 s (7 days), note TTL 2592000 s (30 days), cap on open deposits 100 USDC. |
| Web bundle | Release testnet 0.1.0 (tag `web-v0.1.0`, commit `f972b0d8f0ade26c259836f4c64d0691dc17a5c2`), served at <https://nymira.pages.dev> and mirrored at <https://nymiralabs.github.io/nymira/>. `bundle_hash` `1aac311d00b42d6613783692b9c7a63cf148df9b2e00bd203d74b2e744068098`, `header_set_hash` `3cf2b65df6a4f9df14bf0b87714fed5d6445b78605ac095954e02d4017c32b25`. The signed manifest is in `web/releases/testnet-0.1.0/`; the signing key is `web/pins/release-signing.pub` (minisign key id `934C176AF34D794D`). Cloudflare adds `access-control-allow-origin: *` to the HTML pages while the signed header set has none; a new release will fix that. No outside party has rebuilt the bundle and attested to it yet. |
| Keys the vault uses | The vault account holds only the full-access key above. `operator.nymira.testnet` holds function-call keys limited to `vault.nymira.testnet`: `heartbeat` (`ed25519:ByNe51phL1HLruA2m5mpUNjYU8N8fdAiv2vzjTUPg8Cp`), `mutual_close` (`ed25519:6Sz6SKZd4VRYSiXFANiFS32yNhSfAMwDarHmbmjMiacZ`), and two upkeep keys for `sync_root`, `retire_closed_leaf`, `claim_expired`, `claim_expired_batch` and `finalize_exit` (`ed25519:EcnUgf2F3nVVGh3VJDxydjxfo8asTGJrv2oPaTEns3Di` and, from a second machine, `ed25519:FQZbqfECB8XpMVP42osm5HadqiZpqUyuVzF2cEBmFM7s`). The operator account also still carries two old gas keys for a test vault that was deleted; they are harmless. `answer.nymira.testnet` holds the `defeat_exit` and `raise_concession` keys: `ed25519:3aQcKFzVdwHVBM6vjES6LYimcwDZWRwr7V2grAVg1qRC` (laptop) and `ed25519:HBqKTwBSqY62moLVqCK7MM7DZ36hAcQ2jTo2P95J5KbC` (second machine); its full-access key is `ed25519:396S8bP3UUU4zgRLjjYGGP7rEZMi47SzS7T6MKMUnEcf`. |
| Operator status | <https://nymira-operator.tail0484c4.ts.net/v1/status>, served from a home machine that can be offline. |

The proving and verifying keys come from a setup run by the team alone (the log, scripts and the accepted contributions are in [nymira-ceremony](https://github.com/nymiralabs/nymira-ceremony)). They are sound if at least one contributor really was honest, and the team cannot prove that: if every contribution was dishonest or its machine was compromised, proofs can be forged and the vault emptied. zkapi's development keys, whose secrets are public, never run on the final public vault or on mainnet. The deployed vault uses the final ceremony keys (see the table). A deposit fee is read from the vault's `config()`; on testnet it is a placeholder in test USDC and is not revenue.

The proving keys are release assets of the ceremony repository (`request.pk`, `withdrawal.pk`, tag `final`). The web app serves them from its own signed bundle and checks their SHA-256 before use. For `nymirad`, download both files into one directory and pass it once: `nymirad init ... --setup-dir DIR`. `init` refuses files whose hash differs from the one compiled into the binary, and without the directory chat and close stay off. `nymirad terms` prints the terms of use.

## Limits

These are the rules of the vault contract and of the testnet deployment. The vault's `config()` shows the values. The operator cannot send your money to another account or block your exit: with `request_exit` you leave to your payout account after 7 days, and the operator can deduct at most the bound of the one lease in flight. If you lose your latest wallet state, an exit from an older state can be defeated and you can lose the remaining balance, so keep your encrypted backup. A note expires 30 days after the deposit; seven days after that, the treasury can take it unless an exit is pending. The team can still update the vault code, and open user deposits in the vault are capped at 100 USDC (test USDC on testnet; the cap bounds open deposits, not what the provider is paid).

The first sentence holds while you keep your latest state; see the first point below. The cap is a vault parameter that the owner can change after a 48-hour delay announced by an event.

- An exit from an old state can be defeated: if you lose your latest state, the operator can answer the exit with a later request and keep what it charged, up to the whole remaining balance. Keep a backup.
- A note expires at the end of the UTC day 30 days after the deposit. The treasury can take it only 7 days after the later of that expiry and the last exit activity on the note, and never while an exit is pending; until then an expired note can still exit and finalize.
- If the operator is offline for a whole 7-day exit window, a user can leave with an older, higher balance than the real one. The loss falls on the operator, and the 100 USDC cap on open deposits bounds it.
- Closing shows your note ID, total spend and payout account on chain, next to the account that sends the close.
- The web app cannot use Tor, so the operator and the RPC provider see your IP address. Use Tor Browser or `nymirad` for IP protection.
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
