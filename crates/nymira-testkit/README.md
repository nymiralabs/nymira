# nymira-testkit

The shared test doubles of issue #86, in one place. A ticket that needs a double names #86 as a blocker instead of building its own. Numbers below are GitHub issue numbers.

| Double | Where | Used by |
|---|---|---|
| Mock USDC token (NEP-141, NEP-145) and an `ft_transfer_call` receiver | `contracts/mock-usdc`, `contracts/mock-ft-receiver`, helper `mock_usdc` | #15 to #20 (vault), #44, #45 (Intents funding), #35 (golden path) |
| Proof generator with NEAR-mapped inputs | `proofs` | #15 to #21 (vault), #23, #26, #27 (operator), #30 to #35 (`nymirad`, golden path) |
| Sandbox time control and exact-time helper | `clock` | #16 to #19 (close, exits, claims), #27 (watcher), #34 (expiry guard) |
| Sandboxes at the mainnet and testnet nearcore release | `chain` | #15 to #22 (vault), #35 |
| Open-source Verifier v0.4.2 fixture | `intents` | #20 (Intents branch), #22 (testnet measurements), #44, #45 |
| Mainnet USDC parity fixture | `mainnet_usdc` | #15, #16, #20, #22 |
| Fake NEAR AI service | `crates/nymira-fake-near-ai` | #23, #24, #25, #87, #30, #31, #35, #36, #37 |
| Fake NEAR RPC | `crates/nymira-fake-near-rpc` | #27, #28, #87, #88, #30 to #34, #36 to #39 |
| Web test wallet and browser NEAR codec | `web-testkit`, `crates/nymira-near-codec-wasm` | #36 to #38, #47, #48, #52, #53, #76 |

Every double has a self-test, and CI runs them.

## Needs

cargo-near 0.22.0 on `PATH` (the doubles that are contracts are built with it) and network access the first time near-sandbox downloads its binary. Run the Rust self-tests with `cargo test -p nymira-testkit`.

## Mock USDC

Behaves like the real token where the vault depends on it. `storage_deposit` registers an account for exactly 0.00125 NEAR (`MIN_STORAGE_DEPOSIT`) and refunds any excess, whether or not `registration_only` is set; a deposit for an account that is already registered is refunded in full; a deposit below the minimum fails. `ft_transfer` needs 1 yocto, a registered receiver and enough balance. `ft_transfer_call` calls `ft_on_transfer` and refunds the unused part. Switches: `set_paused`, `set_blocked` (one account, as sender and receiver), `set_panic_in_transfer`, `set_junk_return` (a successful `ft_transfer` returns junk bytes). `mint` credits tokens with no checks.

`mainnet_usdc` runs the real mainnet contract next to it. `tests/mainnet_usdc.rs` runs the same registration steps on both and checks that they agree.

## Proof generator

`ProofGen::new(Network, vault_account)` makes request proofs and withdrawal proofs of kinds 0 (escape), 1 (mutual close) and 2 (ownership), with `chain_id`, `contract_address` and `destination` mapped through `nymira-near-codec`, from the development setup. The state key and the clearance key are two separate seeds (`with_keys`). It takes the note (id and expiry as the vault assigned them) and the tree, so the proof carries the tree's actual root; `MerkleTree::from_leaves` rebuilds the tree from the vault's leaf words. The vault under test must be built with `dev-setup`.

Negative variants: `withdrawal_with_clearance_seed` (a valid proof with another clearance key), a valid proof of another note with the same id (a right note ID with a wrong leaf), `Bundle::with_input` (a forged `noteFields`, or the right leaf with a wrong deposit; the proof no longer verifies), an earlier `DevState` (an old state) and an earlier tree (an old root).

Layout: the pinned circuit is leaf-bound. The root-based layout of the earlier zkapi pin is not in it, so it is not generated.

## Time

See the module docs of `clock`. The sandbox moves about 0.3 s of chain time per block and costs about 11 ms of wall time per block, so only short rules are reachable there: `advance_to` lands exactly one second before a limit and exactly at it, which covers the 30-second rule. The 48-hour, 7-day and 30-day rules are tested at their boundaries under `testing_env!` through `testing_env_at`, where `block_timestamp` is set exactly and the real `alt_bn128` host functions still run. A note's expiry is the end of a UTC day (`end_of_utc_day`); a test that needs an expired note sets the time past that expiry.

## Open-source Verifier v0.4.2

`intents::deploy_verifier` deploys the release wasm of `near/intents` (`defuse/v0.4.2`) at a sandbox subaccount and initializes it. `tests/intents.rs` walks deposit, `ft_withdraw(..., msg)` and the Verifier's `ft_transfer_call` into a receiver, and checks the `sender_id` and `msg` the receiver sees. Its code hash differs from the deployed `intents.near`, so text says "open-source Verifier v0.4.2", never "the mainnet contract". The vault's test-only Intents account setting is the cargo feature `test-intents-account`.
