# nymira-fake-near-ai

A fake NEAR AI Cloud for tests and `make demo`. It needs no network and no keys. It is a library (start it in-process on port 0) and a binary.

Used by: operator tickets 11, 12, 13 and 75 (the provider adapter), `nymirad` tickets 18, 19 and 23 (chat, signature and attestation checks, golden path), web tickets 24 and 25 (badge, golden path).

```rust
let fake = nymira_fake_near_ai::start("127.0.0.1:0".parse()?).await?;
let url = fake.url();                       // http://127.0.0.1:PORT
fake.handle.update(|s| s.fail_next = 1);    // in-process switches
let keys = fake.handle.keys();              // what was minted
```

Binary: `nymira-fake-near-ai [--listen ADDR]` or `NYMIRA_FAKE_NEAR_AI_LISTEN`, default `127.0.0.1:8790`. It prints `nymira-fake-near-ai listening on http://ADDR`. Dropping `Running` stops the server.

## Endpoints

Shapes follow `research/13b-near-ai-cloud-docs.md`. Where that file leaves a shape open, the choice is listed under "Choices".

| Endpoint | Auth | Notes |
|---|---|---|
| `GET /v1/model/list` | none | `modelId`, `inputCostPerToken`, `outputCostPerToken` (`{amount, scale: 9, currency}`), `metadata.{verifiable, providerType, attestationSupported}`. Four `vllm` models (the pinned ids), one `chutes`, one `external`. |
| `POST /v1/workspaces/{ws}/api-keys` | session | Body `{name, expires_at?, spendLimit?: {amount, currency}}`, amount in nano-USD. Returns the key once in `key`. Duplicate live name is 409. |
| `GET /v1/workspaces/{ws}/api-keys` | session | `?limit&offset`, default limit 20. `{data, total, limit, offset}`. No secrets. Deleted keys are left out. |
| `PATCH /v1/workspaces/{ws}/api-keys/{id}` | session | `is_active`, `expires_at`, `spendLimit` (`null` removes it). |
| `DELETE /v1/workspaces/{ws}/api-keys/{id}` | session | 204. Soft delete: the key stops working and leaves the list. |
| `GET /v1/organizations/{org}/usage/export` | session or `rpt-*` | `?api_key_id` filter. Rows `{inference_id, api_key_id, model, input_tokens, output_tokens, cache_read_tokens, cost_nano_usd}`. |
| `GET /v1/organizations/{org}/usage/summary` | session or `rpt-*` | Same filter. Totals and a request count. |
| `GET /v1/organizations/{org}/usage/balance` | session | `{balance: {amount, scale: 9, currency}}`. Cost of each answer is deducted. |
| `POST /v1/chat/completions` | key | OpenAI compatible, plain or SSE. |
| `GET /v1/signature/{chat_id}` | key | `?model&signing_algo`. |
| `GET /v1/attestation/report` | key | `?model&signing_algo&nonce` (hex, at most 128 characters). |

The session token is `fake-session-jwt` (`SESSION_TOKEN`). Any bearer starting `rpt-` is a reporting token. A key is accepted only while it exists, is active and is not deleted. `expires_at` is stored and returned but not enforced.

A key whose recorded spend has reached its `spendLimit`, or any request while the balance is 0 or below, gets 402 `insufficient_credits`. The check is at request start, like `check_api_key`. Cost is `prompt_tokens * input price + completion_tokens * output price` (defaults 80 and 500 nano-USD per token, the `glm-5.3-flash` prices).

### Chat

- Unknown model is 404. The reply is `fake reply to: <last user message>` unless `reply` is set. Tokens are `ceil(chars / 4)`, at least 1.
- `stream: true` sends one SSE event per word, a stop chunk, a `usage` chunk when `stream_options.include_usage` is true, then `data: [DONE]`. The real gateway strips per-chunk usage; the fake keeps it so `nymirad` can check charges.
- E2EE is on when `X-Client-Pub-Key` is present. `X-Model-Pub-Key` must be the key in the attestation report, and `X-Signing-Algo`, if sent, must be `ed25519`. `messages[].content` must then be encrypted to the model key; with `X-Encrypt-All-Fields: true`, `messages[].name` must be too. The answer's `content` (and each stream chunk's, independently) is encrypted to the client key. Hex on the wire. Other fields stay plain, as the guide says. The fake does not read `tools`.
- The suite is X25519, HKDF-SHA256, XChaCha20-Poly1305, layout `[eph_pk 32][nonce 24][ct+tag]`. The guide gives no HKDF salt or info, so the fake uses no salt and `nymira-fake-near-ai/e2ee/v1`. It does not interoperate with the real gateway or SDK, only with `nymira_fake_near_ai::crypto` and the same scheme. The model's E2EE secret is public (`crypto::MODEL_E2EE_SECRET`) so tests can check what was decrypted.
- Every chat is recorded: key id, model, stream, E2EE flags, `x-no-aliasing`, `Accept-Encoding`, the exact request and response bytes (`Handle::chats()`, `GET /control/chats` without the bytes).

### Signatures

`GET /v1/signature/{chat_id}` works only for the key that made the chat. It returns `{text, signature, signing_address, signing_algo, signature_kind}`; the signature is Ed25519 over `text`.

- `provider_tee`: `text = model:sha256(request):sha256(response)`, signed by the model key. Default for non-streamed answers.
- `gateway`: `text = sha256(request):sha256(response)`, signed by the gateway key. Default for streamed answers.

The hashes cover the exact request body and the exact response bytes (for a stream, every SSE byte up to and including `[DONE]`). Only `ed25519` is supported; `ecdsa` is 400.

### Attestation report

```json
{"nonce": "...",
 "gateway_attestation": {"signing_address", "signing_algo", "measurement", "tcb_status", "nonce", "report_signature"},
 "model_attestations": [{"model_name", "model_public_key", "signing_address", "signing_algo", "measurement", "tcb_status", "nonce", "report_signature"}]}
```

`report_signature` is an Ed25519 signature by a fixed test root key over `nymira-fake-near-ai/attestation/v1|measurement|tcb_status|signing_address|model_public_key|nonce` (`model_public_key` is empty for the gateway). It replaces the Intel quote and NVIDIA payload. All keys are fixed, so a web test can pin them as constants.

## Test policy

`TestPolicy::fake()` (also `GET /control/policy`) is the value to pin where production code pins `attestation-policy.json`:

```json
{"attestation_root_pk": "<test root public key, hex>",
 "accepted_measurements": ["<sha256 of nymira-fake-near-ai/measurement/v1, hex>"],
 "accepted_tcb_statuses": ["UpToDate"]}
```

`verify_attestation(policy, report, model, nonce)` and `verify_signature(attested, sig, model, request_bytes, response_bytes)` are a reference verifier for this fake. A pinning client must check the same things: root signature, measurement, TCB status, nonce, that the signer is the key the kind names, that the signed text matches the exchanged bytes, and the Ed25519 signature.

## Switches

In process: `handle.update(|s| ...)` or `handle.patch(&json)`. Over HTTP: `GET /control/switches`, `POST /control/switches` with a merge patch (`null` clears an option, unknown names are 400), `POST /control/reset` (defaults, forget keys, chats and usage), `GET /control/keys` (minted keys with secrets). `/control` is never affected by failure or latency switches.

| Switch | Default | Effect |
|---|---|---|
| `fail_next`, `fail_status`, `fail_path_prefix` | 0, 500, `/v1/` | The next N requests whose path starts with the prefix fail with that status. 429 carries `rate_limit_exceeded`, others `injected_failure`. |
| `latency_ms` | 0 | Added to every `/v1/` request. |
| `usage` | none | Replaces computed token counts in the answer, the usage rows and the cost. This is how to inflate usage. The answer and the export always agree. |
| `chat_signature_kind`, `stream_signature_kind` | `provider_tee`, `gateway` | Kind per answer type. |
| `bad_signature` | false | Flips a byte of every signature. |
| `unbound_key` | false | Signs with a key no attestation lists. |
| `measurement`, `tcb_status` | policy value, `UpToDate` | Reported in both attestation entries. A changed measurement or `OutOfDate` fails the test policy. |
| `balance_nano` | 10 USD | Credit returned by `usage/balance`. |
| `input_price_nano_per_token`, `output_price_nano_per_token` | 80, 500 | Shown in the model list and used for cost. |
| `reply` | none | Fixed answer text. |

## Choices

- Chat, signature and attestation take the lease key; key management, balance and usage take the session token, as in 13b. The real JWT, refresh-token and `rpt-` token handling is not modelled.
- Key list and balance bodies, the usage row names and the model list price fields are the simplest shapes that fit 13b. The real API may differ.
- The signature kind is fixed when the chat is answered. `bad_signature` and `unbound_key` apply when the signature is fetched.
- No billing/costs, no tools, no `max_tokens` enforcement, no rate limits and no concurrency limit. Add them when a ticket needs them.

## Tests

`cargo test -p nymira-fake-near-ai` starts the server on port 0 and drives every endpoint and switch over HTTP with `reqwest`.
