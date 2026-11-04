# Operator scripts

Two machines run the operator. The laptop runs five processes (`scripts/operator/start.sh`). A second machine, on the same tailnet, keeps a copy of the signed state log and watches the vault on its own. The operator signs nothing until the second machine holds the entry (see `THREAT_MODEL.md`).

| File | For |
| --- | --- |
| `operator.example.json` | Laptop config: has a `replica` section (the second machine's address and key). |
| `replica.example.json` | Second machine config: has `replica_node` (where it listens, the laptop's entry key) and `deadman_status_url`. |
| `start.sh` | Laptop: minter, signer, closer, watcher, api. Asks for the passphrases. |
| `start-replica.sh` | Second machine: `replica`, `second-watcher`, `deadman`. Asks for nothing. |

## Starting the second machine

1. On the laptop: `nymira-operatord keygen entry`. On the second machine: `nymira-operatord keygen replica`, `keygen answer` and `keygen upkeep` (the last two are access keys for the answer and operator accounts, separate from the laptop's).
2. Exchange the public keys that `keygen` prints. The laptop's `replica.public_key` is the replica's; the second machine's `replica_node.entry_public_key` is the laptop's entry key.
3. Put the replica's tailnet address in both configs and allow only the laptop to reach its port. The replica serves plain HTTP and relies on the tailnet to decide who may connect.
4. On the laptop, before the first start: `nymira-operatord recovery hold`. Start the second machine with `scripts/operator/start-replica.sh replica.json`, start the laptop, and run `recovery clear` once the two heads match.

`second-watcher` runs the laptop watcher's duties on the replica's own database, which holds the entries the laptop pushed, under the id `replica`. If the laptop is gone, the second machine can still answer an old-state exit from what was replicated.

The second watcher's polls are written to the second machine's database, not the laptop's, so the laptop's `/v1/status` does not list it. Its alarms go out through the second machine's own alarm channels.

`config.rs` has a test that loads both example files, so a renamed field breaks the build instead of a launch.
