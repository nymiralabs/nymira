# Budget notes for agents

[README](README.md) · [Website](https://nymira.pages.dev) · [Documentation](https://nymira.pages.dev/#docs) · [Try it in 20 minutes](JUDGES.md) · [Threat model](THREAT_MODEL.md) · [Releases](https://github.com/nymiralabs/nymira/releases)

This is the full guide to budget notes: the policy, the agent API, the Agents screen, the IronClaw and Cursor recipes and the opt-in mode. The [README](README.md#budget-notes-for-agents) has the short version.

A budget note is a note you fund for one agent. Its payout account is your own account, fixed at deposit, so whatever the agent leaves comes back to you. The agent gets a token and can spend that one note on inference. It cannot withdraw anywhere else.

```bash
nymirad agent create --name cursor --transport tcp --key-file KEY --amount 1000000
nymirad agent create --name worker --transport unix --agent-user agent1 --token-file /srv/agent/worker.token --key-file KEY --amount 1000000
nymirad agent list
nymirad agent disable --name cursor
nymirad agent policy --name cursor --file cursor-policy.signed.json
```

`agent create` is the normal deposit plus a registry entry, written before the transfer is signed. On chain the note looks like any other note of its denomination. It prints a reminder that a fresh 1 USDC note each day is the hard way to cap a day's spend: it costs the note fee every day and leaves a daily deposit pattern on your account. The registry (`agents.json` in the state directory, mode 0600) keeps the agent name, a hash of the token, the budget note, your account and the policy version. Tokens are never stored. Agent tokens start with `nyma_`, the owner token with `nymo_`, so a tool can accept only the agent prefix.

- **Own note only.** An agent token draws on its budget note and nothing else. Your own token never selects a budget note, and `nymirad status` marks them.
- **One transport per token.** A `unix` token works on the agent socket only and a `tcp` token on loopback port 8788 only. A remote agent reached through an SSH forward gets a `tcp` token.
- **Routes.** An agent token reaches `POST /v1/chat/completions`, `GET /v1/models`, `GET /v1/nymira/balance` and `POST /v1/nymira/quote` on both listeners and nothing else, and works as an OpenAI API key on the first two (the last two are described under "Agent API" below). The owner token is not accepted on the agent socket.
- **Disabling.** `agent disable` takes effect on the next request, which gets 401 `token_disabled`.
- **No policy, no spending.** Until a signed policy is loaded for the agent, every agent request gets 402 `policy_missing` and nothing reaches the operator. A policy that is unsigned, changed after signing, signed by another account or by a function-call key, signed for another agent or note, older than one already seen, or that misses a limit key or has an unknown one counts as no policy.
- **Signed policy.** `policy.yaml` has `allowed_models`, `max_lease_usd`, `max_call_usd`, `max_daily_usd`, `confirm_above_usd`, `rate_limit_per_min`, `require_tee` (omitted means true) and `allowed_origins` (empty for now; a non-empty list is refused until paying sellers exists). You sign it with your NEAR account (NEP-413) and a full-access key. The signed message is JSON, `{"kind":"nymira-policy","version":N,"policy":"<the yaml text>"}`, and the recipient is `nymirad:<agent name>:<note commitment>`. `agent policy` checks the signature, reads your account's access keys over the configured RPC endpoints to confirm the key is full-access, prints a summary and asks you to type yes. The policy applies from the agent's next request. The YAML is a small subset (one `key: value` per line, plain-word lists, no anchors, tags, merge keys or quotes) and is parsed only after the signature over its exact bytes has verified. The signed file is kept in the state directory (`policy-<agent>.json`, mode 0600) and checked again on every request.
- **Model allowlist.** A request for a model outside `allowed_models` gets 402 `model_not_allowed` before anything leaves the machine, also on a lease that is already open. `GET /v1/models` lists only the allowed models.
- **Spending limits.** Every agent request, also one on an open lease, is checked in this order: token enabled, valid signed policy, model, rate, call cost, the day, and last the note's balance. `rate_limit_per_min` caps requests in a minute (429 `policy_rate_limit` with `Retry-After`). `max_call_usd` lowers the `max_tokens` that is sent so the call's worst case stays within it; a request whose own `max_tokens` asks for more, or whose input alone passes the limit, gets 402 `call_limit`. `max_daily_usd` counts, for the UTC day, what settled leases charged plus the bound of the open lease plus the worst case of calls in flight; a request that would pass it gets 402 `daily_limit`. Opening a lease reserves its bound, and settlement replaces the reservation with the charge. The day's total is kept in `counters-<agent>.json` in the state directory and after a restore is never lower than the charges in the lease records. Leases use the 0.10 USDC bound class; the 0.50 class only when the request asks for it with `X-Nymira-Max-Lease-Usd` and `max_lease_usd` is at least 0.5. The call limit lowers `max_tokens`, never the bound. A note that cannot cover the bound gets 402 `budget_exhausted`, whatever your other notes hold. A third request while the note's lease has two in flight gets 409 `busy`. A call whose worst case is above `confirm_above_usd` is held for your approval and gets 402 `approval_required`.
- **`require_tee`.** With `require_tee: true`, the default, an agent gets an answer only after `nymirad` has checked its signature against the exact bytes exchanged. The model's attestation report, and the binding of the encryption key to it, are checked before anything is encrypted, and the answer opens only with that key. The provider's signatures can only be read once a lease is closed, so each such call closes its lease, waits up to 120 s for the operator to store the signature, and only then releases the answer. A streamed answer is held and released in one piece. The reply carries `X-Nymira-Signature-Kind`, `model-signed` or `gateway-signed`. `require_tee` means the attestation verified and the key is bound to it; a gateway-signed answer passes, and it does not show that the model itself signed. Streams are expected to be gateway-signed. An answer that fails gets 502 `not_verified` and no content. The closed lease has to settle before the note can serve the next call. With `require_tee: false` the lease stays open and answers arrive at once.

### Agent API

An agent calls these with `Authorization: Bearer nyma_...` on the agent socket or on loopback port 8788. Every number is in micro-USD (1 USDC is 1,000,000) unless the name says USD.

- `GET /v1/nymira/balance` returns `balance_micro` (the deposit less the charges of settled leases; a lease still open counts when it settles), `spendable_micro` (the balance when it can pay for a lease of the agent's class and the note is open, else 0), `lease_bound_micro`, `spent_today_micro`, `note` (`status`: `open`, `expiring`, `revoking`, `exiting`, `closed` or `unfunded`; `closing`; `expires_at` in Unix seconds), `charge_warning` and `limits` (the policy's limits). It names no other note.
- `POST /v1/nymira/quote` takes the same body as a chat request and runs the same checks, in the same order, as `POST /v1/chat/completions`. It moves no counter, holds no approval, opens no lease and sends nothing. It answers `{"decision": "allow" | "needs_approval" | "deny", "reason": ..., "estimated_cost_micro", "max_tokens", "input_tokens", "model"}`; a `deny` also carries `http_status`, `retry_after_secs` for a rate limit and `detail` for `unavailable`. The balance and the lease are read from local state, so a refusal the operator would make at the lease (its own limits) shows only when the call is made.
- `X-Nymira-Max-Cost-Usd` on a chat or quote request caps the call's cost in USD with at most 6 decimals, for example `0.002`. It lowers the `max_tokens` that is sent. A cap above `max_call_usd` is refused, not lowered. A request whose own `max_tokens` would pass the cap, or a cap below the cost of the prompt and one answer token, also gets 402 `call_limit`.
- A non-streamed chat answer on the agent API carries `nymira.charge_usd`: what this call cost at the pinned prices, from the usage the answer reports, as a USD decimal string with six places, rounded up to a micro-USD. It is an estimate of the call alone. The lease's final charge adds the slot fee and the charge step, and shows in the balance after the lease settles. A replay under an `Idempotency-Key` returns the same figure. A streamed answer has none.
- `Idempotency-Key` on a chat request (1 to 128 printable characters, not with `stream: true`): a retry of the same request with the same key returns the first answer, with `X-Nymira-Idempotent-Replay: true`, and spends nothing. A retry that arrives while the first request runs waits for it. The answer is found by token, note, key and request, so another token, another note or another request finds nothing. It is kept in memory for 5 minutes, at most 64 at once, never written to disk or a backup. Only answers are kept; a refusal or failure runs again on retry.
- Message `content` is one string, or a list of text parts that are each exactly `{"type": "text", "text": "..."}`; `nymirad` joins the texts with nothing between them and encrypts the result as one string. A part of any other type (image, audio, file) or with an extra field, and an empty list, gets 400 `invalid_request`, because it would have to travel in the clear or be dropped.
- `X-Nymira-Approval-Id` on a chat request is the retry of a held call. The 402 `approval_required` that held it carries the ID as `error.approval_id` and in the `X-Nymira-Approval-Id` response header.
- `X-Nymira-Max-Lease-Usd` asks for the 0.50 lease class, as above.

Errors use the OpenAI shape, `{"error": {"message", "type", "param", "code"}}`, plus `approval_id` for `approval_required` and `detail` for `unavailable`. On the agent API the `code` is one of the reasons below or one of the guard codes after the table, and the `message` is a fixed text or one `nymirad` wrote from its own figures; nothing the operator or the model wrote is passed on.

| Status | `code` | Meaning |
|---|---|---|
| 401 | `token_disabled` | the owner disabled this token |
| 402 | `policy_missing` | no valid signed policy for this agent |
| 402 | `model_not_allowed` | the model is not in `allowed_models` |
| 402 | `call_limit` | the call would pass `max_call_usd` or the cost cap, or the cost cap is above `max_call_usd` |
| 402 | `daily_limit` | the call would pass `max_daily_usd` |
| 402 | `approval_required` | held for the owner; carries the approval ID |
| 402 | `budget_exhausted` | the note cannot cover the lease |
| 402 | `note_expiring` | the note is in its last 5 days, or a revocation, close or exit has begun |
| 402 | `note_moved` | the note was moved to the agent's own machine; this copy does not spend it |
| 402 | `no_note_can_cover_request` | the lease cannot pay for this prompt and an answer |
| 429 | `policy_rate_limit` | `rate_limit_per_min` reached; `Retry-After` given |
| 429 | `operator_capacity` | the operator's admission, in-flight limit or daily provider budget refused; `Retry-After` given, to the next UTC midnight when the daily budget is used up |
| 429 | `provider_rate_limit` | the model provider is rate limiting; `Retry-After` given |
| 409 | `busy` | the note's lease already has 2 requests in flight |
| 502 | `not_verified` | `require_tee` withheld an answer whose signature did not verify |
| 503 | `unavailable` | `detail` says why: `tor_down`, `tor_refused`, `vault_pins_changed`, `operator_emergency`, `charge_warning` (see below), `operator_unreachable`, or the code of any other operator error |

Approval requests also use `approval_invalid` (400, a malformed or incomplete signed approval), `approval_wrong_signer` (403, not the note's owner account or not a full-access key), `approval_unknown` (404), `approval_used` (409), `approval_expired` (410), `approval_mismatch` (409) and `approval_limit` (429; at most 5 held calls per agent, each valid for 10 minutes). A malformed chat body is 400 `invalid_request`.

The front door adds its own refusals before any of this: 401 `invalid_token` (a missing or unknown token), 403 `wrong_transport` (a `tcp` token on the socket, or a `unix` token from another user), 403 `agent_route_forbidden` (an agent token on a route outside its four), 403 `agent_route_only` (an owner token on `/v1/nymira/*`), 403 `peer_not_loopback`, `bad_host`, `origin_present` and `cross_site`, and 503 `registry_unreadable`.

A **charge warning** is raised when a lease settles with a charge more than one charge step above the one recomputed from the signed usage. From then on every request on that agent's note gets 503 `unavailable` with `detail: charge_warning`, `nymirad serve` prints a warning, and `agent show` marks it, until you run `nymirad agent ack-warning --name NAME`. The mark is a file in the state directory and part of the backup.

### What a budget note guarantees

The ceiling is cryptographic: an agent can spend at most the balance of its budget note, and a close can only pay your payout account. The policy is only as strong as the machine that runs `nymirad`. The model list, the lease, call and daily limits, the rate limit, the approvals and `require_tee` are checked there. That machine is yours by default. In opt-in mode (below) it is the agent's, and revoking is not guaranteed.

Both rest on the vault matching the client's pins. The team can still update the vault code, and open user deposits in the vault are capped at 100 USDC (test USDC on testnet).

There is no remote rate limit and no remote kill switch. A lease request does not name its note, and the design is meant to keep the operator from grouping leases by note. That is a claim of the underlying protocol that has not been checked independently. Only your own `nymirad` limits an agent's rate, and only you disable its token. The operator can still stop new leases for everyone, through its emergency stop and capacity limits.

Closing a budget note shows its note ID and total spend on chain, next to the payout account you named, so the agent's total becomes public at the end.

### Agents screen in the web app

The web app has an Agents screen for the owner. It builds `policy.yaml` from form fields and signs it with your NEAR wallet (NEP-413, so the wallet has to support `signMessage`; on testnet Meteor works, and Intear signs in but its approval window may not report back, see [INTEARnear/wallet#23](https://github.com/INTEARnear/wallet/issues/23); the screen is hidden when the connected wallet cannot sign messages). `require_tee: true` is the only value it offers. It signs an approval from the approval ID, request hash, amount and nonce that `nymirad agent approvals` shows, and it prints the `nymirad` commands and tool settings from the fields you type: every value is shell-quoted, control characters are refused, and nothing is read from the URL. The remote-agent forward it prints is `ssh -N -R 8788:localhost:8788 AGENT_HOST`, to run on your machine.

The page creates no budget note, never asks for or shows a token, and makes no request to `nymirad`. You download the signed files and run the commands yourself. The policy needs the note commitment: it is the `note` field of the agent in `agents.json` in the state directory, and it goes into the signed recipient, so a file signed for one agent or note is refused for another.

### Another OS user on your machine

Use `--transport unix --agent-user USER --token-file PATH`. `nymirad serve` opens a socket at `/tmp/nymira-agent-<your uid>/agent.sock` (or `agent_socket_dir` in `config.json`), mode 0660 in a directory of mode 0750, both in the agent group. The group is the agent user's primary group, or `--group`. Every connection's peer user ID is checked: your own OS user is refused, as is any user no socket agent is registered for, and each token works only for its own user. The token file is mode 0640, owned by you, in the agent group, so the agent user can read it and nobody else can. If you are not a member of that group, you cannot give files to it: the command then prints `sudo chgrp GROUP FILE` for the token file, and `serve` prints the `sudo chgrp` and `chmod 2750` commands for the socket directory. Restart `nymirad serve` after the first socket agent is created.

The peer check on the socket is the only OS-user check there is. Loopback TCP has none, so a `tcp` token is a bearer secret for anything on the machine that can read it. An agent that runs as your own OS user (Cursor, IronClaw, any tool started from your shell) can read your owner token. Run it against a dedicated `nymirad` instance with its own `--home`, port and owner token that holds only its budget note and none of your personal notes. `agent create` refuses `--agent-user` set to your own user.

### Another machine

Requests arrive on the loopback listener, so they pass the same checks as yours: loopback peer, a `Host` of `localhost:8788`, `127.0.0.1:8788` or `[::1]:8788`, no `Origin`, no cross-site fetch. Start the forward from your own machine, so the agent host holds no credential for it:

```bash
ssh -N -R 8788:localhost:8788 AGENT_HOST
```

The forwarded port binds to the loopback of the agent host as long as `GatewayPorts` stays off there. A direct connection to your machine over a tailnet or LAN is refused. If the forward has to start on the agent host, restrict that key in your `authorized_keys` with `restrict,port-forwarding,permitopen="localhost:8788",command="/bin/false"` and run `ssh -N -L 8788:localhost:8788 OWNER_HOST` there. `permitopen` limits `-L` forwards only. For a key that starts the `-R` forward from your machine on the agent host, use `restrict,port-forwarding,permitlisten="localhost:8788",command="/bin/false"` there instead. `scripts/agent-ssh-forward-check.sh` tests both lines against a throwaway `sshd` on loopback (no `sudo`): no shell, only the permitted port forwards, other ports are refused. It passed with OpenSSH 10.3. `scripts/agent-os-user-check.sh` checks the socket and token file with a throwaway second OS user (macOS, needs `sudo`).

### IronClaw

<!-- recipe:ironclaw begin: written from shared/recipes.json by scripts/recipes.mjs, do not edit here -->

[IronClaw](https://github.com/nearai/ironclaw) is NEAR's agent runtime. This recipe points it at `nymirad`, so every model call it makes is paid from a budget note under your signed policy. It was tested with IronClaw v1.4.1. A newer version is listed here only after it passes `scripts/agents-smoke.sh`, which runs that release against the mock stack and checks its SHA-256 before it unpacks it. The recipe works against `make demo` with no keys or money: `cargo run -p nymirad --features mock --example demo_sign -- DEMO_HOME NAME` signs the demo policy with the demo account's key. That tool is built only with the `mock` feature, so a release build has no key to sign with.

Start from a budget note, a signed policy that allows the model below, and an agent token (see the Budget notes guide or the Agents screen). IronClaw starts as the user who launches it, so it must not share an OS user with the `nymirad` that holds your own notes. Pick one:

- a different OS user on this machine (IronClaw reaches `nymirad` over loopback TCP, so issue a `tcp` token, not a `unix` one);
- a different machine, reached with the forward under "Another machine" in the Budget notes guide;
- a dedicated `nymirad` instance with its own `--home`, port and owner token that holds only the budget note.

Write an environment file that only IronClaw's user can read (mode 0600), from your token file. Run this as you, and replace `AGENT_USER` with IronClaw's OS user:

```bash
{
  echo 'LLM_BACKEND=openai_compatible'
  echo 'LLM_BASE_URL=http://localhost:8788/v1'
  printf 'LLM_API_KEY=%s\n' "$(cat /srv/agent/ironclaw.token)"
  echo 'LLM_MODEL=z-ai/glm-5.3-flash'
} | sudo install -m 600 -o AGENT_USER /dev/stdin ~AGENT_USER/nymira.env
```

Then, as IronClaw's user, from a work directory that is not the home directory:

```bash
set -a; . ~/nymira.env; set +a
ironclaw onboard --no-service </dev/null
ironclaw models status        # default.base_url must read http://localhost:8788/v1
```

`LLM_BACKEND=openai_compatible` is IronClaw's generic provider (`provider_id = "openai_compatible"` with a `base_url` in `config.toml` is the same thing). `onboard` with no terminal asks nothing and copies the key from the environment into IronClaw's own secret store under that user's home; disabling the token (`nymirad agent disable --name ironclaw`) is what stops the agent, not deleting the file. Use the pinned model's canonical ID. `nymirad` refuses any model outside `allowed_models` with 402 `model_not_allowed`, whatever IronClaw is configured to ask for, and `GET /v1/models` lists only the allowed ones.

One-line check, then the owner command that shows the charge:

```bash
ironclaw run --message "Reply with one short greeting."     # as IronClaw's user
nymirad agent show                                          # as you: balance, and spend today once the lease has settled
```

What to expect:

- Chat completions only. An agent token reaches `/v1/chat/completions` and `/v1/models` as an OpenAI key; every other OpenAI route is refused.
- IronClaw on Linux sends message content as a list of text parts (on macOS as a string). `nymirad` joins the text parts into one string before it encrypts them; any other part type is refused with 400.
- At most two requests in flight per lease; a third gets 409 `busy`.
- A pause of up to about 10 seconds when a new lease opens.
- A call whose worst case is above `confirm_above_usd` is held for your approval and gets 402 `approval_required`.
- A 429 `operator_capacity` with `Retry-After` when the operator's daily provider budget is used up; a 429 `policy_rate_limit` when your own rate limit is reached.
- Errors arrive in OpenAI's shape. IronClaw words a 402 as "The AI provider account is out of credits" and prints `nymirad`'s reason (the `code`, for example `model_not_allowed`) in its warning line.
- With `require_tee` on, an answer is held until its signature and the model's attestation are checked, and each answer comes back with its signature kind in `X-Nymira-Signature-Kind`. Streamed answers are expected to be gateway-signed, not model-signed. `run` in v1.4.1 sent no `stream` field in our run.

What IronClaw's calls still reveal: the operator sees the model, the request settings, the number and size of messages, token usage and timing, as for any request (THREAT_MODEL.md). IronClaw sends its own tool list with every request; v1.4.1 sent 26 definitions in our run, so that length is visible too, and their text is encrypted. The set of notes a lease could belong to is small while few notes are open, and on testnet it is made of test notes. IronClaw's other traffic does not go through Tor. A machine that runs IronClaw sees every prompt.

IronClaw has no MCP entry for the Nymira server. Its documentation says v1.4.1 connects to MCP servers over HTTP(S) and rejects the `stdio` transport, and the Nymira server speaks stdio only. IronClaw pays through the provider route above.

For IronClaw on another machine, run this on your machine, so the agent host holds no credential for yours:

```bash
ssh -N -R 8788:localhost:8788 AGENT_HOST
```

The forwarded port binds to loopback on the agent host while `GatewayPorts` stays off there, and a direct connection to your machine over a tailnet is refused. If the forward must start on the agent host, restrict the key on your machine as shown under "Another machine" in the Budget notes guide. Never expose `nymirad` through a public tunnel: that removes the loopback guard, and the tunnel's operator would carry the token.

<!-- recipe:ironclaw end -->

### Cursor

<!-- recipe:cursor begin: written from shared/recipes.json by scripts/recipes.mjs, do not edit here -->

Cursor's agent pays for answers from a budget note through the Nymira MCP server and its `nymira_ask` tool. It does not use Cursor's base URL override (see below). Cursor has not been run against this recipe, so no Cursor version is listed here; the entry follows the format in Cursor's MCP documentation.

Start from the same budget note, signed policy and agent token as for the IronClaw recipe. Cursor starts the MCP server as the user who runs Cursor, so run Cursor as a different OS user or on a different machine, or point the entry at a dedicated `nymirad` instance that holds only the budget note.

Install the server at a fixed path (the steps are in `mcp/README.md`; no release exists yet, so build it from a checkout and point the entry at its `dist/index.js`), keep the token in a file that only that user can read, and add this entry to `.cursor/mcp.json` in the project or to `~/.cursor/mcp.json`. It holds the path of the token file and no token. `node tools/host-config.ts --host cursor` in `mcp/` prints it for your paths.

```json
{
  "mcpServers": {
    "nymira": {
      "type": "stdio",
      "command": "/usr/local/bin/node",
      "args": [
        "/opt/nymira-mcp/node_modules/@nymiralabs/nymira-mcp/dist/index.js",
        "--token-file",
        "/srv/agent/cursor.token",
        "--transport",
        "tcp",
        "--url",
        "http://127.0.0.1:8788"
      ]
    }
  }
}
```

**Why not the base URL override.** Replies on Cursor's forum say that a request with an overridden base URL first goes to Cursor's servers ([one](https://forum.cursor.com/t/problem-reaching-openai-error-on-a-local-model-with-overriden-base-url/3975), [two](https://forum.cursor.com/t/connecting-local-ai-server-to-cursor-does-not-work/152940)), so they cannot reach `localhost`. Exposing `nymirad` through a tunnel would remove its loopback guard, and Cursor would still read every prompt. If Cursor adds a mode that sends custom-base-URL requests from the local client, this recipe is checked again before that path is added.

**What Cursor sees.** Cursor and its model vendor see what the agent sends to `nymira_ask` and what comes back, with the balance, charges and timing the tools return. MCP gives no content privacy from the host. What the path does give: NEAR AI sees only Nymira's organisation as the payer, the agent cannot raise its ceiling, and your policy applies to every call.

What to expect is the IronClaw list, with the tool's own limits: `max_usd` caps a call, a held call returns `approval_required` and only you can approve it, and an answer's text reaches Cursor as untrusted text.

One-line check, then the owner command that shows the charge: ask Cursor's agent to call `nymira_ask` with a short question and `max_usd` of `0.01`, then run `nymirad agent show`. To stop the agent, run `nymirad agent disable --name cursor`.

<!-- recipe:cursor end -->

### Opt-in: the note on the agent's machine

If the agent has to run without your machine, you can move its budget note to the agent's own `nymirad`. This is off unless you ask for it, and only the owner CLI does it.

```bash
nymirad agent move --name cursor --agent-recipient AGE_PUBLIC_KEY_OF_THE_AGENT_MACHINE \
  --state-recipient YOUR_AGE_PUBLIC_KEY --state-place /mnt/vault/states --out cursor-move.age
# on the agent's machine, into an empty state directory:
nymirad restore --from cursor-move.age
# later, on your machine:
nymirad agent import-state --name cursor --from /mnt/vault/states
```

`agent move` prints what you give up and asks you to type yes. In short: revocation is not guaranteed, the ceiling is. The note's balance is still the most the agent can spend, and a close can only pay your payout account. The policy, the counters and the approvals now run on the agent's machine and are advisory, because anyone who controls that machine can change them. Revoking becomes a race: only what is unspent when your close or exit lands comes back to you, a hostile agent can spend the rest on inference first, and an exit from a stale state can be defeated by a newer one.

After the move your copy refuses to spend (402 `note_moved`), so two copies never fork the note. It keeps the secret, so you can still close or exit. The agent machine's `nymirad` takes the token over on loopback TCP (a Unix user ID from your machine means nothing there) and carries the signed policy and this note's lease records, so today's charges still count against the daily limit. After each retire it writes the note's new state to every `--state-place`, as `moved-<note>-<sequence>.age`, encrypted to your age key.

`agent import-state` decrypts every file there with your private key and chooses by the note's nullifier chain: a state counts only if its anchor lies on the chain that your secret generates and the operator's signature on it verifies. The file's own sequence number only breaks a tie, so a validly encrypted old state with a high number loses to a longer chain. Stale files are listed. It then asks the operator whether the best state's nullifier is already used and warns if it is: the agent is ahead of the files you hold. Run it before every close or exit of a moved note.

A moved note can only be paid out to the account fixed at deposit, so an agent that holds the secret and sends `mutual_close` pays only you (a test covers it). Paying sellers (x402) is not built yet; when it is, your copy will refuse to pay sellers for a moved note, and an agent that holds the secret could pay any listed seller.
