# nymira-vault

The vault contract. Deposits, the Merkle tree, mutual close, payouts and claims.

## Tests

```bash
cargo test -p nymira-vault --lib      # unit tests, no sandbox
cargo test -p nymira-vault --test sandbox -- --nocapture   # sandbox, prints gas
```

The sandbox tests build the vault with `cargo near` (0.22.0) and run it next to the mock USDC token. The unit tests and the `dev-setup` sandbox build accept zkapi's development verifying keys. A build without `dev-setup` accepts none: its pins are zero until the ceremony publishes its key hashes.

## Gas, measured on the sandbox (2026-11-01)

| Call | Gas burnt |
|---|---|
| `new` (derives both verifying keys) | 94 Tgas |
| `ft_on_transfer` (one deposit) | 4.1 Tgas |
| `sync_root` with one leaf | 60 Tgas |
| `sync_root` with 80 leaves | 202 to 211 Tgas |
| `mutual_close` (proof check, two payout receipts, callback) | 46 Tgas in all receipts, 42 Tgas in the vault |
| `claim_payout`, `claim_treasury` | 9 Tgas in all receipts |

A Poseidon hash costs about 1.8 Tgas, and every `sync_root` computes the root again (up to 32 hashes). A batch of 120 leaves burnt 287 Tgas, over the 250 Tgas that leaves the 1.2 margin under the 300 Tgas limit, so one call inserts at most 80 leaves (`MAX_SYNC_LEAVES`).

The payout legs use fixed gas: 10 Tgas for `storage_deposit`, 20 Tgas for `ft_transfer`, 10 Tgas for the callback. They are placeholders with margin over the mock token; the real USDC is measured in ticket #22. `mutual_close` needs 110 Tgas prepaid and the claims 50 Tgas, or they panic before writing anything.

A payout callback reads only the promise status, never the returned bytes (a token may return anything), so it uses `env::promise_result_checked(0, 0)` and not `#[callback_result]`, which would panic on bytes that are not JSON.
