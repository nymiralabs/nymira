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
