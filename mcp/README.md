# nymira-mcp

An MCP server over stdio for one Nymira agent token. It adds no authority of its own: it forwards three calls to the owner's `nymirad` and returns the answers. Every policy check stays in `nymirad`.

| Tool | What it does |
|---|---|
| `nymira_balance` | Balance, spendable amount, today's spend, expiry, closing status and the owner's limits. |
| `nymira_quote` | Whether a call would be allowed, needs approval, or is denied (with a reason code), and its worst-case cost. Takes `model`, `messages` or `input_tokens`, and `max_usd`. |
| `nymira_ask` | Makes one chat call and pays for it from the budget note. Takes `model`, `messages` and `max_usd`, and optionally `request_id` and `approval_id`. Returns the answer in `untrusted_text`, with `model`, `signature_kind` (`model-signed`, `gateway-signed` or `none`), `usage` and `replayed`. A refusal comes back as the agent API's reason code. |

Amounts are USD decimal strings with six places. Results are in `structuredContent`; the text content is the same object as JSON. `nymira_balance` and `nymira_quote` open no lease and send nothing. `nymira_ask` spends money.

`max_usd` is sent as `X-Nymira-Max-Cost-Usd`, and `nymirad` turns it into the `max_tokens` it sends upstream. `request_id` is sent as `Idempotency-Key`: a retry with the same ID returns the first answer and spends once. `approval_id` is the ID that `nymirad` returned with `approval_required`; the owner approves with their own command, and the server has no way to approve. The answer text is untrusted: it is a third party's words and appears only in `untrusted_text`. An error is a code from the agent API, a fixed sentence and a few numbers. It never holds text from the operator or the model.

`nymira_ask` does not return the charge yet. The agent API's chat answer carries token counts but no price, so the charge is not available without a change in `nymirad`; the final charge shows in `nymira_balance` once the lease has retired.

## Run

```bash
node dist/index.js --token-file ~/.nymira/agent.token --transport tcp
node dist/index.js --token-file ~/.nymira/agent.token --transport unix --socket /tmp/nymira-agent-501/agent.sock
```

`--transport` must match the transport the token was made for. `--url` (tcp only) must be a loopback address and defaults to `http://127.0.0.1:8788`. The flags can also be set with `NYMIRA_MCP_TOKEN_FILE`, `NYMIRA_MCP_TRANSPORT`, `NYMIRA_MCP_URL` and `NYMIRA_MCP_SOCKET`.

The token is read only from the file. The server refuses to start if the file is a link, is readable by other users, has group write, holds an owner token (`nymo_`), or does not hold an agent token (`nyma_`). It ignores proxy settings and never talks to anything but loopback or the Unix socket.

## Test

```bash
cargo build -p nymira-mcp-fixture   # the mock stack the stack tests run against
npm ci --ignore-scripts
npm test
```

`test/stack.test.ts` starts the fixture (sandbox vault, operator, fake NEAR AI, and `nymirad`'s agent routes) and talks to the server with the official MCP client. Set `NYMIRA_MCP_FIXTURE` to use another fixture binary. The socket check needs `lsof`.
