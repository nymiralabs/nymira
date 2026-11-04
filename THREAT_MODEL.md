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

The operator writes no log lines that come from a request; its error output holds start-up errors and, after a panic, the source line only. Lease records hold the nullifier, the lease token's hash, the bound, the charge, the usage reading and times; at settlement the key id and the exact timers are deleted and times fall back to the hour. Copies on the second machine carry hour-rounded times from the start. Request proofs and signed states are deleted after 44 days, on both machines. Alarm messages pass through a scrubber that removes `sk-` and `rpt-` strings and `Authorization` values.

## Processes and keys

The operator runs as five processes under one dedicated macOS user: the public API, a key minter, a signer, a closer and a watcher. The public API has no NEAR key and no login; it holds the provider keys of the leases that are open, in memory, each capped at its lease bound. The NEAR AI login lives only in the key minter. The state and clearance keys exist only in the signer, from two passphrases typed at start-up. The closer holds one function-call key, `mutual_close`; the signer's is `heartbeat`; the watcher holds the upkeep key and a separate exit-answer key. Processes talk over Unix sockets (mode 0600, checked by user id), and the public API is the only TCP listener.

The processes share one macOS user, so code execution in any one of them as that user can read every file that user can read, including the sealed login file once its passphrase is known. The separation limits what a bug in the web-facing process can reach in memory and which keys it holds; it does not stop an attacker who gets a shell as the service user. The developer tools and browsers installed on the laptop stay readable by every user on macOS.

Usage figures come from NEAR AI's `usage/history` for a lease's key, read with the session login of the key minter, because the read-only reporting routes answer 404. Reading usage therefore uses the same login that can mint keys, and it stays inside the minter process.

## The second machine

The operator signs a state only after a second machine has the record on disk, so a lost laptop disk does not let a user spend a state twice. The second machine sits in the same home as the laptop: it covers a lost disk or a dead laptop, not a power or network failure of the house, and it does not protect against someone who takes both machines. It is planned to hold the log, a watcher and its own exit-answer key, and no signing key and no provider token; today it holds the log. The operator signs each log entry it sends, and the second machine can pin that key. The second machine is not set up yet.

## If the operator is offline

A user leaves with `request_exit` and `finalize_exit` after the 7-day window, and the vault does not need the operator for that. If the operator and every watcher are offline for the whole window, nobody can answer an exit from an old state, so the user leaves with an older, higher balance than the real one. The loss falls on the operator, and the cap on open deposits bounds it.

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

On the machine the wallet state is sealed under the passphrase. While a wallet is open for a lease, a close or an exit, its plain files sit in a memory-backed directory (tmpfs on Linux, a RAM disk on macOS), never on the disk, and `nymirad` refuses to open the wallet if it cannot make one. A crash leaves the files in memory until the next start seals them. The operating system may still write memory to swap: macOS encrypts swap by default, Linux does not unless the machine is set up for it.

Restoring with the key or passphrase continues on the same note from the state the backup held. If a lease was served after that backup, the operator is ahead of the restored state, and the way out is an exit, which the operator can defeat by showing a later state; the balance then goes to the treasury at expiry. Restoring from the note secret alone gives an exit from the deposit state with concession 0, with the same risk.

On the operator's emergency notice `nymirad` stops opening leases and suggests an exit for each open note. It does not exit by itself because of the notice. It reads the notice from `/v1/status` before each lease and every few minutes while it runs, so a notice can reach it late.

## What the other parties see

- **The web hosts (Cloudflare Pages and the GitHub Pages mirror).** They see each visitor's IP address and which files were asked for, and they decide which code a visitor gets. A compromised host could serve a different app. `nymirad verify-web` is built to compare both hosts with a signed manifest over Tor and directly. Its key and the two host names are compiled in, but it has nothing to compare until a release is signed. Even then it catches a change only if the user runs it, and not a host that serves different content to different clients.
- **RPC providers.** They see the IP address of whoever reads or sends, and with a deposit transaction the account, the transaction hash (so the note ID, the commitment and the payout account) and the time. The web app cannot use Tor. `nymirad` sends through Tor and asks first before any direct send.
- **Attestation and collateral services (NEAR AI's attestation endpoint, PCCS, NVIDIA's NRAS).** They see the address that asks and the time. `nymirad` asks through Tor; the web app asks NEAR AI and Phala's PCCS directly (Intel's own collateral service sends no CORS headers). The web app does not ask NRAS, which refuses cross-origin requests, so it cannot check NVIDIA GPU evidence: it checks the Intel TDX quote, the policy and the key binding, and says so.
- **1Click.** It sees the IP address of whoever asks for a quote, and the asset pair. The web app's testnet quote listing asks only after the user turns it on and sends no note data. `nymirad` sends testnet quote-only requests through Tor with placeholder accounts and no wallet or note data. Its mainnet funding path asks for explicit consent once per attempt, including after a restart, then calls 1Click directly outside Tor. On that path 1Click and the Intents explorer can pair the source wallet with the commitment and payout account. The quote signature identifies the issuer; 1Click and its solvers hold funds in transit, and the source wallet's refund address is the only way back. Spending can still be matched by timing and network address when few notes are open. No cross-chain payment is run on testnet.
- **Faucet.** Circle's faucet sees the account that asks for test USDC.

## What an encrypted request hides

`nymirad` and the web app encrypt every field the model reads to the model's attested key: message `content` and `name`, tool-call names and arguments, and tool names, descriptions and parameters. These stay in the clear because the operator and the gateway need them: the model name, `max_tokens` and `max_completion_tokens`, `stream` and `stream_options`, `n`, `temperature`, `top_p`, the two penalties, `seed`, `stop`, `tool_choice`, each message's `role`, `tool_call_id`, the `tools` key, the length of the `tools` array and each tool's `type`, and the sizes and timing of everything. The operator refuses a request with an unknown field instead of forwarding it. The browser cannot bind the gateway's TLS session, so the web app says "encrypted" on its own line and "model-signed" only when the signature checks.

## Trust in USDC

The vault holds Circle's USDC and trusts it fully. If Circle's blocklister blocks the vault account, closes, finalizes and claims stop; `request_exit` still works because it calls no token method, and payouts that cannot be sent wait in `pending_payouts`. Circle controls the token contract and can upgrade it or freeze funds. On testnet the token is Circle's testnet USDC and has no value.

## The trusted setup

The keys that check the zero-knowledge proofs come from a multi-party setup run by the team alone, with a public random value from a NEAR mainnet block applied at the end (the record and the scripts are in the nymira-ceremony repository). The setup is sound if at least one contribution kept its secret. We cannot show that from outside, and all contributions ran on the project's own machines. If it is not sound, someone who knows the combined secret can forge a proof and take what the vault holds. No loss of privacy is known from this. The proving keys are public files; the web app and `nymirad` check their SHA-256 against hashes shipped in the release. A vault made with zkapi's development keys or the rehearsal keys is for tests only and holds test funds.

## What the team's keys can do

The vault account keeps a full-access key. The team can use it to replace the vault code, so it could change the rules the contract enforces for deposits already inside it. The key is offline and the operator software never touches it. The operator account keeps its own full-access key offline too. Online work uses function-call keys whose methods are listed by name (`heartbeat`, `sync_root`, `mutual_close`, `defeat_exit`, `raise_concession`, `finalize_exit`, `retire_closed_leaf`, `claim_expired`, `claim_expired_batch`) and hold no owner method; the second machine's key has neither `heartbeat` nor `mutual_close`. Anyone can list a vault's keys with `view_access_key_list`, and `scripts/watch-vault-keys.mjs` raises an alert on a `DeployContract`, `AddKey` or `DeleteKey` on the vault account. The testnet vault caps open user deposits at 100 USDC, so a code swap by the team puts at most that much (test funds) at stake.
