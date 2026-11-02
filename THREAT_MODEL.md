# Threat model: the operator

This file covers the operator side. It says what the operator can see and do, what limits that, and what it cannot promise. The testnet deployment holds test funds only, the contract has had no independent review, and the anonymity set is small and made of test notes.

## What the operator sees

- **Requests.** Each lease request carries a proof, a nullifier and a bound class. The operator sees those, the times it handled them and the charge it signs.
- **Prompts.** Chat bodies are encrypted end to end to the model's key from the attestation report. The operator sees the model name, sizes and timing, not the text. It forwards the encryption headers and refuses a request that has none.
- **Addresses.** Operator code reads no client address and keeps none, and nothing it writes to its database or its alarm messages holds an IP address or a prompt (a test checks this). The entry point is a different matter: Tailscale Funnel's relay sees the client address and the time of every connection and passes the address to the laptop in a header. The operator does not read that header. A client that connects through Tor shows a Tor exit instead. If Funnel cannot carry a feature, a Cloudflare Tunnel takes its place and then terminates TLS, so Cloudflare sees request metadata; bodies stay encrypted.
- **Linking.** Tor exits repeat, and timing and usage can still link a user's requests. The set of notes a lease could belong to is the set of open notes, which on testnet is small.

## What the provider sees

NEAR AI receives each request under a per-lease key that the operator mints and caps at the lease bound. The provider sees the model, token counts, timing and the key's usage, and can tie requests of one lease together. It does not see which note the lease belongs to. Prompts reach it only as ciphertext addressed to the model's attested key.

## What the logs keep

The operator writes no log lines. Lease records hold the nullifier, the lease token's hash, the bound, the charge and times; at settlement the key id and the exact timers are deleted and times fall back to the hour. Copies on the second machine carry hour-rounded times from the start. Request proofs and signed states are deleted after 44 days. Alarm messages and errors pass through a scrubber that removes `sk-` and `rpt-` strings and `Authorization` values.

## Processes and keys

The operator runs as five processes under one dedicated macOS user: the public API, a key minter, a signer, a closer and a watcher. The public API has no secret and no NEAR key. The NEAR AI login lives only in the key minter. The state and clearance keys exist only in the signer, from two passphrases typed at start-up. The closer holds one function-call key, `mutual_close`; the signer's is `heartbeat`; the watcher holds the upkeep key and a separate exit-answer key. Processes talk over Unix sockets (mode 0600, checked by user id), and the public API is the only TCP listener.

The processes share one macOS user, so code execution in any one of them as that user can read every file that user can read, including the sealed login file once its passphrase is known. The separation limits what a bug in the web-facing process can reach in memory and which keys it holds; it does not stop an attacker who gets a shell as the service user. The developer tools and browsers installed on the laptop stay readable by every user on macOS.

## The second machine

The operator signs a state only after a second machine has the record on disk, so a lost laptop disk does not let a user spend a state twice. The second machine sits in the same home as the laptop: it covers a lost disk or a dead laptop, not a power or network failure of the house, and it does not protect against someone who takes both machines. It holds the log, a watcher and its own exit-answer key, and no signing key and no provider token. The operator signs each log entry it sends, and the second machine can pin that key. Its volume snapshots keep the shortest retention the host allows.

## If a key leaks

New leases and clearances stop at once (`emergency`) and `/v1/status` shows why; users are told to exit and a new vault is deployed. A leaked state key lets an attacker sign states, a leaked clearance key lets an attacker forge clearances, and a leaked provider credential lets an attacker spend the prepaid credit. None of these gives access to user funds held by the vault, whose rules are in the contract.
