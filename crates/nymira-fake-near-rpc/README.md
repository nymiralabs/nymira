# nymira-fake-near-rpc

A fake NEAR JSON-RPC node for tests. It serves the reads the operator watcher, `nymirad` and the web app make (vault views, `config()`, access keys, transaction status with receipts and events, final block data) from state a test scripts. Switches make an instance lag, fork or fail, so that code which compares several providers can be tested with three instances. It needs no network and no keys.

It is a test double, not a node. It does not run contracts, check signatures or keep real state roots.

## Use in a test

```rust
use nymira_fake_near_rpc::*;
use serde_json::json;

let a = FakeRpc::start().await;          // 127.0.0.1, free port
let b = FakeRpc::start().await;
for rpc in [&a, &b] {
    rpc.set_view("vault.test", "config", ArgsMatch::Any, ViewResult::json(json!({"fee": "1"})));
    rpc.advance_blocks(10, 1.0);         // 10 blocks, 1 s apart
}
b.set_lag(3);                            // b serves blocks and state 3 blocks behind
let urls = [a.url(), b.url()];           // hand these to the code under test
// ... later: what did the code send?
let sent = a.submissions();
```

Dropping a `FakeRpc` stops it. All setters take `&self`.

## Binary

```
nymira-fake-near-rpc [--addr HOST:PORT] [--chain-id ID]
```

`NYMIRA_FAKE_RPC_ADDR` sets the address when `--addr` is absent. The default is `127.0.0.1:0`. The first stdout line is `listening on http://HOST:PORT`. Script it over HTTP (below). Run three processes for three providers.

## JSON-RPC (POST `/`)

| Method | Notes |
|---|---|
| `query` `call_function` | Scripted views. `args_base64` is matched, the result comes back as a byte array. A scripted error or an unscripted method returns `{"error": "..."}` in `result`, as nearcore does for a failed call. |
| `query` `view_account` | Defaults if only the view or the account was scripted: large balance, no code. |
| `query` `view_access_key_list`, `view_access_key` | From `set_access_keys`. Unknown key: `UNKNOWN_ACCESS_KEY`. |
| `block` | `finality` (`final`, `optimistic`) or `block_id` (height or hash). Header has `height`, `hash`, `prev_hash`, `timestamp` and `timestamp_nanosec` in nanoseconds. |
| `status` | `sync_info` with latest height, hash and RFC 3339 time. `chain_id` defaults to `testnet`. |
| `gas_price` | Fixed. |
| `tx`, `EXPERIMENTAL_tx_status` | Array `[hash, sender]` or object `{tx_hash, sender_account_id, wait_until}`. Returns status, `transaction`, `transaction_outcome`, `receipts_outcome` (with `logs`) and `receipts`. |
| `send_tx`, `broadcast_tx_commit`, `broadcast_tx_async` | The signed bytes are recorded. The hash is the sha256 of the borsh transaction (the bytes before the trailing signature), so a client's own hash matches. Default result is success with no receipt logs, unless a status for that hash was scripted. Afterwards `tx` finds it. |

Every `query` result carries `block_height` and `block_hash`. Every answer reads state at the served block, so a `finality: final` read and a read by `block_id` of the same block agree.

Errors use NEAR's shape: `HANDLER_ERROR` with cause `UNKNOWN_ACCOUNT`, `UNKNOWN_BLOCK`, `UNKNOWN_ACCESS_KEY` or `UNKNOWN_TRANSACTION`; `REQUEST_VALIDATION_ERROR` with `METHOD_NOT_FOUND` (-32601) or `PARSE_ERROR` (-32602); -32700 for a body that is not JSON. A view on an account that was never scripted is `UNKNOWN_ACCOUNT`.

Not implemented: batch requests, `view_code`, `view_state`, `changes`, `chunk`, legacy array-style `query` params, `EXPERIMENTAL_*` methods other than `tx_status`, WebSocket.

## Scripting model

Time and height. Every instance starts at height 1000, timestamp 2026-11-01T00:00:00Z. `advance_blocks(n, seconds_per_block)` adds blocks. `set_block(height, timestamp_ns)` sets one block and moves the head up if needed. `set_block_hash(height, hash)` pins a hash. Hashes are derived from chain id, height and fork tag, so two instances that were advanced the same way agree on every hash.

State is versioned by block. A view, account or key set is stamped with the head at the time. A node only sees a script whose stamp is at or below the block it serves, and a newer script replaces an older one for the same key. This is what makes a lagging node return stale state.

View matching: `ArgsMatch::Any`, `ArgsMatch::json(v)` (the request args parse as JSON equal to `v`) or `ArgsMatch::bytes(b)` (exact bytes). An exact match beats `Any`. Results: `ViewResult::json(v)`, `ViewResult::bytes(b)` for junk or non-JSON bytes, `ViewResult::error(msg)` for a panicking contract.

Transactions: `TxStatusSpec::success(hash, signer, receiver)` with `.receipt(ReceiptSpec::new(executor).event(standard, version, event, data))`, `.status(OutcomeSpec::Failure(msg))` and `.at_height(h)`. `event_log` builds an NEP-297 `EVENT_JSON:` line. Without receipts the status has one implicit receipt at the receiver.

## Switches (per instance)

| Switch | Effect |
|---|---|
| `set_lag(n)` | Serves blocks, views, accounts and transactions as of `head - n`. Blocks above that are `UNKNOWN_BLOCK`. `set_lag(0)` catches up. |
| `set_fork(Some(ForkSpec { from_height, tag }))` | Blocks from `from_height` get a different hash (same height, same timestamp). Earlier blocks stay common. `None` heals. For different state, script different views on that instance. |
| `set_failure(f)` | Applies to every JSON-RPC request: `Failure::Http(503)`, `RpcError { code, message }` (HTTP 200 with a JSON-RPC error), `Timeout` (never answers; the client must set a timeout), `Garbage` (HTTP 200, not JSON), `None`. |
| `set_latency(d)` | Delays every JSON-RPC answer. |
| views differ between instances | Each instance has its own scripts: give one a different `set_view` result. |
| `reset()` | Clears scripts, logs and all switches. Blocks stay. |

A failing instance still logs the request (see `requests()`), but it does not record a submission or serve it. The control endpoint is never affected by `set_failure`.

## Logs

`submissions()` returns what was sent: method, base64 bytes, hash, signer, receiver, nonce, `wait_until`, head at the time. `requests()` returns every JSON-RPC method and params the instance saw, so a test can assert what a client did not ask (for example that `nymirad` never queried `note_status` on a public provider).

## HTTP control (for the binary)

`POST /control` with `{"cmd": ..., ...}`. 200 `{"ok":true}`, or 400 for a bad command. The shapes are the same as the in-process setters.

```
{"cmd":"set_view","account":"vault.test","method":"config","args":"any","result":{"json":{"fee":"1"}}}
{"cmd":"set_view","account":"vault.test","method":"note_status","args":{"json":{"note_id":"1"}},"result":{"base64":"cmF3"}}
{"cmd":"set_view","account":"vault.test","method":"m","args":{"base64":"AQID"},"result":{"error":"panic"}}
{"cmd":"set_account","account":"op.test","spec":{"amount":"42"}}
{"cmd":"set_access_keys","account":"op.test","keys":[{"public_key":"ed25519:K","nonce":3,"permission":"FullAccess"}]}
{"cmd":"set_block","height":1100,"timestamp_ns":1793491300000000000}
{"cmd":"set_block_hash","height":1100,"hash":"..."}
{"cmd":"advance_blocks","n":10,"seconds_per_block":1.0}
{"cmd":"set_lag","blocks":3}
{"cmd":"set_fork","fork":{"from_height":1005,"tag":"evil"}}      // "fork": null heals
{"cmd":"set_failure","failure":{"http":503}}                      // "none", "timeout", "garbage", {"rpc_error":{"code":-32000,"message":"x"}}
{"cmd":"set_latency_ms","ms":500}
{"cmd":"set_tx_status","status":{"tx_hash":"H","signer_id":"a.test","receiver_id":"vault.test","receipts":[{"executor_id":"vault.test","logs":["EVENT_JSON:{}"]}]}}
{"cmd":"reset"}
```

`GET /control/state` (head, served head, lag, fork, failure, latency), `GET /control/submissions`, `GET /control/requests`.

## Decisions where the specs are open

- The tx hash is computed from the borsh bytes, not from anything the test supplies, so operator code that broadcasts, then queries its own hash, behaves as against a node. Only ed25519 and secp256k1 trailing signatures are told apart by length, and ed25519 is tried first.
- "Final" and "optimistic" both mean the served head. There is no two-block finality gap.
- A lagging node hides state by block stamp, and hides transactions by their inclusion height.
- A failing provider fails every method, not one.
- Receipts and events exist only for scripted and sent transactions. There is no retention window. To test a node that has forgotten a transaction, leave the hash unscripted (`UNKNOWN_TRANSACTION`).

## Used by

| Tickets | Use |
|---|---|
| Operator watcher: #27, #28, #87, #88 | `exit_requests`, `note_records`, `pending_payout` views on two or three providers, second-provider confirmation, disagreement alarms, final-block reads, gas-key balance and key list, sent exit answers and `wait_until: FINAL` receipts. |
| `nymirad`: #30 to #34 | `get_leaves` and parameter views read at one final block, endpoint fallback and Tor-path assertions through `requests()`, receipt events for deposit acceptance. |
| Web app: #36 to #39 | Public RPC fallback, deposit event from the receipt, `view_access_key`, `config()`, final block timestamps for expiry. |

Tests in `tests/self_test.rs` exercise every method and switch through a real HTTP client, including a test that tells a lagging, a forked and a failing provider apart. Run with `cargo test -p nymira-fake-near-rpc`.
