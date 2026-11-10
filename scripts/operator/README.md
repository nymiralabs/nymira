# Operator scripts

[README](../../README.md) · [Website](https://nymira.pages.dev) · [Documentation](https://nymira.pages.dev/#docs) · [Try it in 20 minutes](../../JUDGES.md) · [Threat model](../../THREAT_MODEL.md) · [Releases](https://github.com/nymiralabs/nymira/releases)

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

## Starting the laptop

Needs the vault deployed, a `nymira-operatord` built without `dev-setup`, and the verifying keys in `setup_dir`.

1. `cargo build --profile operator -p nymira-operatord`, then install `target/operator/nymira-operatord` at `/usr/local/libexec/nymira/nymira-operatord`.
2. `sudo scripts/operator/create-service-user.sh`, then `sudo scripts/operator/check-service-user.sh`. The first makes the `nymira-op` user and `/var/nymira`; the second changes nothing.
3. Copy `operator.example.json` to `/var/nymira/operator.json` and fill in the account names (`answer_account` is the separate account that holds the exit-answer key), `host`, `web_origin`, `usage_public_key`, `near_ai` and `replica`. Point `run_dir`, `data_dir`, `secrets_dir` and `setup_dir` into `/var/nymira`. The config refuses unknown fields, and a test (`the_example_config_loads`) keeps both example files loadable.
4. As the service user, run `nymira-operatord keygen heartbeat`, `mutual_close`, `upkeep`, `answer` and `entry`. Each writes a 0400 seed under `secrets_dir` and prints the public key. On the second machine, `keygen replica` makes the replica key (see above). Add the public keys to the operator and answer accounts as function-call keys, with the methods in `THREAT_MODEL.md`.
5. `nymira-operatord pubkeys --config operator.json` asks for the state and clearance passphrases and prints the two keys the vault's `new()` takes. These passphrases fix the keys for the life of the vault.
6. Put the NEAR AI refresh token in a 0400 file and set `near_ai.first_refresh_token_file`. The key minter reads it once, on the first start, and seals the rotated token under `data_dir`. Delete the plain file afterwards.
7. `sudo scripts/operator/start.sh /var/nymira/operator.json` and type the seal, state and clearance passphrases. Expose port 8080 with `tailscale funnel --bg 8080`, then run `scripts/operator/funnel-check.sh OPERATOR_URL WEB_ORIGIN`. `GET /v1/status` should show `recovery_mode: false`.

Optional fields, left out of the examples: `team_note_ids_file` (a JSON array of note IDs published by the team, so the privacy panel can count outside notes), `archival_rpc_url` (mainnet only, required there), and `alarms.email` next to `alarms.telegram` (`host`, `port`, `security` of `tls` or `starttls`, `from`, `to`, and optionally `username` and `password_file`). The mainnet profile also needs three `rpc_urls`.

`second-watcher` runs the laptop watcher's duties on the replica's own database, which holds the entries the laptop pushed, under the id `replica`. If the laptop is gone, the second machine can still answer an old-state exit from what was replicated.

The second watcher's polls are written to the second machine's database, not the laptop's, so the laptop's `/v1/status` does not list it. Its alarms go out through the second machine's own alarm channels.

`config.rs` has a test that loads both example files, so a renamed field breaks the build instead of a launch.
