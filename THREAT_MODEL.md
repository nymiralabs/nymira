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

## What closing and exiting show, from the web app

A close, an exit request, a finalize and a payout name the note ID in the vault call. A close also shows the note's total spend and the payout account. The web app sends them from the wallet account the user connects, so that account is on chain next to the note ID; connecting an account other than the funding account keeps the two apart, and the app does not create one. `request_exit` and `finalize_exit` can come from any account with a little NEAR, and the payout still goes to the account fixed at deposit.

The RPC provider sees the user's IP address with every read the app makes to leave: the sending account's balance, `exit_requests` (a page of the whole list, naming no note), `note_records` (names the note ID) and `pending_payout` (names the payout account). The browser cannot use Tor.

A close asks the operator for a clearance, so the operator sees the ownership proof and the user's IP address and the time. `nymirad` waits a random one to twenty minutes first; the web app does not, so a close soon after a conversation is easier to tie to it. A close handed to the operator's intake shows the operator's account as the sender, and the operator keeps its close fee.

An exit from an old state can be defeated: if the user loses the latest state (the browser's storage and the backup), the operator can answer the exit with a later request and keep what it charged, up to the whole remaining balance. The close screen says so next to the guarantee.

## What closing and exiting show on chain

A close, an exit request and a payout name the note's ID in the vault call. `nymirad` sends them from a submitter account, a separate NEAR account whose access key can call only `request_exit`, `finalize_exit`, `mutual_close` and `claim_payout` on the vault, so the account that funded the note does not sign them. The submitter's own funding trail is visible on chain: whoever funds it can be seen to have paid for gas next to your note ID.

One submitter shared by every note puts all of those notes next to one account. `nymirad` keeps one submitter per family (your personal notes, and each budget note on its own) so you can keep that link apart. A note belongs to the family `personal` until you assign another.

Sends go over Tor, each on its own circuit. If Tor is down, `nymirad` sends nothing by itself. It asks you first, in fixed words: a direct send shows your IP address to the RPC endpoint together with the note's ID and its total spend. The clearance request and the operator's close intake never go direct, because they would show your address to the operator. A close can go to the operator's intake when the submitter has no NEAR; the operator then keeps its close fee.

## Expiry and backups

A note that reaches its expiry goes to the treasury. By default `nymirad` opens no new lease on a note within 5 days of its expiry and tries a mutual close at 5 days. If the close has failed, it files `request_exit` at 3 days. It shows notices at 14, 7 and 2 days. The guard can be switched off. A close or exit it sends shows the note ID like any other, and it never goes direct while Tor is down. Closing every note when a session ends is a separate switch, off by default, because each close shows the note ID and the total spend.

After every retire, and after each lease open, close and deposit, `nymirad` writes the whole state, encrypted with age to a public key you chose at setup, to a copy on the machine and to a second place you name. The private key is shown once and is never stored by `nymirad`, in the keychain or beside the file. With a passphrase instead, Argon2id (64 MiB, 3 passes, 1 lane, random salt) derives the key, and the passphrase needs at least 12 characters. A passphrase can still be guessed offline by anyone who holds a backup; Argon2id slows that down and does not stop it.

Inside the encryption the notes and wallet states are in the clear, so whoever holds a backup and its key or passphrase can spend your notes. Keep the key away from the machine and from the backup. The place that stores the file sees an encrypted blob, its size and its write times.

Restoring with the key or passphrase continues on the same note from the state the backup held. If a lease was served after that backup, the operator is ahead of the restored state, and the way out is an exit, which the operator can defeat by showing a later state; the balance then goes to the treasury at expiry. Restoring from the note secret alone gives an exit from the deposit state with concession 0, with the same risk.

On the operator's emergency notice `nymirad` stops opening leases and suggests an exit for each open note. It does not exit by itself because of the notice. It reads the notice from `/v1/status` before each lease and every few minutes while it runs, so a notice can reach it late.
