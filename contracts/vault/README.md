# nymira-vault

The vault contract. Deposits, the Merkle tree, mutual close, payouts and claims.

## Tests

```bash
cargo test -p nymira-vault --lib      # unit tests, no sandbox
cargo test -p nymira-vault --test sandbox -- --nocapture   # sandbox, prints gas
```

The sandbox tests build the vault with `cargo near` (0.22.0) and run it next to the mock USDC token. The unit tests and the `dev-setup` sandbox build accept zkapi's development verifying keys. A build without `dev-setup` accepts only the ceremony keys: today the hashes of the rehearsal ceremony, replaced by the final ceremony's hashes after its beacon (`scripts/check-pins.sh` checks that they equal the vendored ones). Its code hash depends on the commit it is built from (the contract embeds its source revision), so a published hash names the commit to rebuild.

## Gas, measured on the sandbox (2026-11-01)

| Call | Gas burnt |
|---|---|
| `new` (derives both verifying keys) | 94 Tgas |
| `ft_on_transfer` (one deposit) | 4.1 Tgas |
| `sync_root` with one leaf | 60 Tgas |
| `sync_root` with 80 leaves | 202 to 211 Tgas |
| `mutual_close` (proof check, two payout receipts, callback) | 47.3 Tgas in all receipts, 40.0 Tgas in the first |
| `claim_treasury` | 10.1 Tgas in all receipts |
| `finalize_exit` (real USDC wasm, after a one-day window) | 10.5 Tgas in all receipts, about 4 in the first |

A Poseidon hash costs about 1.8 Tgas, and every `sync_root` computes the root again (up to 32 hashes). A batch of 120 leaves burnt 287 Tgas, over the 250 Tgas that leaves the 1.2 margin under the 300 Tgas limit, so one call inserts at most 80 leaves (`MAX_SYNC_LEAVES`).

The payout legs use fixed gas: 6 Tgas for `storage_deposit` (`GAS_STORAGE_DEPOSIT`), 7 Tgas for `ft_transfer` (`GAS_FT_TRANSFER`), 6 Tgas for the callback (`GAS_CALLBACK`). Against the real mainnet USDC wasm they burn 1.82, 2.05 and 1.70 Tgas, so each constant keeps a margin of about 3.3 times. `mutual_close` needs 80 Tgas prepaid (`MIN_GAS_MUTUAL_CLOSE`; it burns 40.0 Tgas before the legs, and 19 Tgas go to the legs) and `claim_payout`, `claim_treasury` and `finalize_exit` need 30 Tgas (`MIN_GAS_CLAIM`; they burn about 4 before the legs), or they panic before writing anything. `finalize_exit` was measured at 50 Tgas attached and the minimum was lowered to 30 from those numbers; the claim tests pass at 29 (refused) and 30 (paid).

A payout callback reads only the promise status, never the returned bytes (a token may return anything), so it uses `env::promise_result_checked(0, 0)` and not `#[callback_result]`, which would panic on bytes that are not JSON.
