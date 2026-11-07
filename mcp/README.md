# nymira-mcp

An MCP server over stdio for one Nymira agent token. It adds no authority of its own: it forwards three calls to the owner's `nymirad` and returns the answers. Every policy check stays in `nymirad`.

| Tool | What it does |
|---|---|
| `nymira_balance` | Balance, spendable amount, today's spend, expiry, closing status and the owner's limits. |
| `nymira_quote` | Whether a call would be allowed, needs approval, or is denied (with a reason code), and its worst-case cost. Takes `model`, `messages` or `input_tokens`, and `max_usd`. |
| `nymira_ask` | Makes one chat call and pays for it from the budget note. Takes `model`, `messages` and `max_usd`, and optionally `request_id` and `approval_id`. Returns the answer in `untrusted_text`, with `model`, `charged_usd`, `signature_kind` (`model-signed`, `gateway-signed` or `none`), `usage` and `replayed`. A refusal comes back as the agent API's reason code. |

Amounts are USD decimal strings with six places. Results are in `structuredContent`; the text content is the same object as JSON. `nymira_balance` and `nymira_quote` open no lease and send nothing. `nymira_ask` spends money.

`max_usd` is sent as `X-Nymira-Max-Cost-Usd`, and `nymirad` turns it into the `max_tokens` it sends upstream. `request_id` is sent as `Idempotency-Key`: a retry with the same ID returns the first answer and spends once. `approval_id` is the ID that `nymirad` returned with `approval_required`; the owner approves with their own command, and the server has no way to approve. The answer text is untrusted: it is a third party's words and appears only in `untrusted_text`. An error is a code from the agent API, a fixed sentence and a few numbers. It never holds text from the operator or the model.

`charged_usd` is what this call cost at the pinned prices, recomputed by `nymirad` from the usage in the answer. It covers the call alone. The lease's final charge adds the slot fee and the charge step, and shows in `nymira_balance` once the lease has retired.

## Install at a fixed path

The release tarball carries the locked dependencies, so the install reads nothing from a registry and the one hash covers everything that runs. Each release has four files: `nymira-mcp-X.Y.Z.tgz`, `RELEASE_NOTES.md` (the package hash, the source commit), `SHA256SUMS` and `SHA256SUMS.minisig`. The notes are signed through `SHA256SUMS` with the project's minisign key, the one in `web/pins/release-signing.pub`. Take that key from this repository, not from the release page.

```
cp web/pins/release-signing.pub .          # from a checkout you already trust
minisign -V -p release-signing.pub -m SHA256SUMS
shasum -a 256 -c SHA256SUMS
npm install --prefix /opt/nymira-mcp --ignore-scripts --no-audit --no-fund ./nymira-mcp-X.Y.Z.tgz
```

Both checks must pass before the install. The server is then `/opt/nymira-mcp/node_modules/@nymiralabs/nymira-mcp/dist/index.js`, run by Node 24 or later. A host entry points at that file and at nothing that changes under it. To update, check a new release the same way and install it into a fresh directory.

The team publishes the same file to npm by hand under the `@nymiralabs` scope. An npm install is not covered by the signature unless you compare its hash with `SHA256SUMS`; use the tarball above.

## Host entries

An entry holds the command, the install path, the path of the token file and the `nymirad` address. It never holds the token: the server reads the token from the file and refuses a file that other users can read. `tools/host-config.ts` in a checkout writes the entry for you:

```
node tools/host-config.ts --host cursor --install-dir /opt/nymira-mcp \
  --token-file /srv/agent/worker.token --transport unix --socket /tmp/nymira-agent-501/agent.sock
```

`--host` is `cursor`, `claude-desktop` or `vscode`. For a `tcp` token use `--transport tcp --url http://127.0.0.1:8788`. Paste the printed JSON into the host's file (`~/.cursor/mcp.json`, `claude_desktop_config.json`, or `.vscode/mcp.json`). The command is the absolute path of your `node`, because a host started from a menu has a short `PATH`. The shape of each entry is tested, and each generated command is run with the official MCP client against an installed tarball. The hosts themselves were not run; see the compatibility notes when they are written.

## What the host sees

The agent host and its model vendor see every question and answer that passes through `nymira_ask`, and the balance, charges and timing that the tools return. The privacy of the rest of Nymira does not reach the program that drives the tools. Do not give an agent a note for questions you would not show its vendor.

## What the limits are

- Rate limits are local to the owner's `nymirad`. There is no remote kill switch: you stop an agent by disabling its token or closing its note on your own machine.
- The confirmation amount (`confirm_above_usd`) is per call. An agent can stay under it with many smaller calls. A hard daily cap needs `max_daily_usd` or a fresh note per day.
- In opt-in mode (the note moved to the agent's machine) the policy is advisory. Only the note's balance is a ceiling.

## Compatibility

| Client | Version | Protocol | Result |
|---|---|---|---|
| Official MCP SDK client (`@modelcontextprotocol/sdk`) | 1.32.1 | 2025-11-25 | Lists the three tools; tested on every run. The server answers `initialize` with the client's version when it knows it (tested for 2025-11-25 and 2025-06-18) and with its latest otherwise. |
| Cursor | not tested | | The entry shape is generated and checked; the host was not run. |
| IronClaw | not tested | | No entry is generated: its configuration format was not checked. |

The server stays on `@modelcontextprotocol/sdk` 1.32.1. It moves to the v2 line (`@modelcontextprotocol/server`) only after Cursor and IronClaw both work with a v2 server over stdio; the two lines are never mixed.
