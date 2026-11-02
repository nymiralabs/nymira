# NEAR AI Inference SDK for Rust

`InferenceClient` provides verified Chat Completions with optional field
end-to-end encryption (E2EE), OHTTP, streaming, and explicit response verification.
It verifies the Gateway and supported model deployments before sending Chat and
retains the exact wire bytes used for completion signatures.

## Install

Use Rust stable and Cargo. The crate is imported as `nearai_inference_sdk`:

```toml
[dependencies]
nearai-inference-sdk = "0.1"
```

## Send and verify a Chat completion

Add these dependencies to your application's `Cargo.toml`:

```toml
[dependencies]
nearai-inference-sdk = "0.1"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Set your NEAR AI Cloud API key in your server environment:

```sh
export NEARAI_API_KEY='your-api-key'
```

Save this example as `src/main.rs` and run it with `cargo run`:

```rust,no_run
use nearai_inference_sdk::{InferenceClient, InferenceClientOptions};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = InferenceClient::with_options(InferenceClientOptions {
        api_key: Some(std::env::var("NEARAI_API_KEY")?),
        e2ee: true,
        ..Default::default()
    })?;
    let completion = client.chat_completions(json!({
        "model": "z-ai/glm-5.3-flash",
        "messages": [{"role": "user", "content": "Hello!"}]
    })).await?;
    // Verify the completion signature before using the answer.
    let verified = client.verify_response(completion["id"].as_str().unwrap()).await?;
    println!("Verified {:?} response", verified.signature_kind());
    println!("{}", completion["choices"][0]["message"]["content"]);
    Ok(())
}
```

The client connects to the NEAR AI Cloud Gateway by default. This example
enables E2EE, which encrypts supported Chat fields to an attested model and
decrypts its response. Choose a model that supports NEAR model attestation;
E2EE rejects unsupported models before sending Chat.

Gateway TLS identity verification is enabled by default, and subsequent requests
are pinned to that identity. Clone a client to share connections, caches, and
in-flight verification.

## When verification happens

1. **Before sending Chat:** the client verifies Gateway evidence and checks the
   model's attestation support. Supported models require every returned model
   report to pass. Failed required checks stop the request.
2. **After receiving the response:** call `verify_response(id)` to verify its
   completion signature against the exact request and response bytes and the
   evidence retained for that request. The client captures those bytes
   automatically, including encrypted wire bytes when E2EE is enabled.
3. **For streaming:** consume the entire response body through HTTP EOF before
   calling `verify_response()`. Output displayed before that call succeeds is
   not yet signature-verified. See the [streaming example](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#streaming).

Optionally call `verify(model)` before sending Chat. It sends no Chat request,
shares Chat's verification cache and in-flight work, and returns verified Gateway
and model evidence with its original verification timestamp. This preflight
verifies deployments; it does not verify a particular reply.

Successful deployment verification is cached for 60 minutes by default. Set
`attestation_cache_ttl: Duration::ZERO` (from `std::time`) to verify before every
request. Response records have a separate 60-minute lifetime starting after full
body consumption. Verify responses before their records expire or are evicted.
See [cache lifetime and limits](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#cache-lifetime-and-limits)
for entry and byte budgets.

## Verification and encryption

These defaults apply to the Gateway `InferenceClient`:

| Feature | Purpose | Default |
| --- | --- | --- |
| Attestation | Verify Gateway and supported model deployments | Enabled |
| Gateway TLS binding | Pin requests to the attested Gateway identity | Enabled |
| Response verification | Verify exact request and response bytes | Explicit `verify_response()` call |
| E2EE | Encrypt supported Chat fields to the attested model | Disabled; set `e2ee: true` |
| OHTTP | Encrypt the Chat HTTP exchange to the attested Gateway | Disabled; set `ohttp: true` |

Ed25519 is the default signing algorithm; ECDSA is also supported. OHTTP requires
Ed25519, is independent of field-level E2EE, and also works with Gateway-only
models. See [encryption and OHTTP](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#encryption-and-ohttp)
for protected fields and transport scope.

Models without supported NEAR model attestation use **Incognito** mode:
Gateway-only verification. E2EE and configured model deployment policies reject
this mode before Chat. Failed metadata or attestation checks never downgrade to it.

Attestation does not approve particular deployments by default. Supply deployment
policies or image provenance policies to enforce your application's approval
criteria. See [verification policies and TLS](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#verification-policies-and-tls)
and [image build provenance](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#verify-an-images-build-provenance).

## Rust HTTP transport

Use `chat_completions()` for JSON or `chat_request()` and `send(reqwest::Request)`
for streaming. Rust exposes its own reqwest transport API; it does not provide an
adapter for a third-party OpenAI Rust crate. See the
[runnable Rust examples](https://github.com/nearai/inference-sdk/blob/main/examples/README.md#rust) for JSON and SSE workflows.
Only Chat Completions are supported; the Responses API is not supported.

## Advanced APIs

- **Manual verification:** `AttestationClient` retrieves evidence and signatures.
  Use the standalone functions below when your application controls each stage.
- **Standalone E2EE:** `prepare_e2ee_chat_request()` encrypts a raw reqwest Chat
  request and supplies JSON/SSE decryptors. Key verification and response signature
  verification remain the application's responsibility. See
  [encryption and OHTTP](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#encryption-and-ohttp).
- **Direct endpoints (experimental):** `DirectInferenceClient` and
  `DirectAttestationClient` connect without Gateway verification. Direct TLS
  fingerprint fetching is disabled pending complete fleet coverage; normal HTTPS
  verification remains enabled. Use Gateway clients for production. See
  [direct-endpoint limitations](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#experimental-direct-endpoints).

## Standalone verification

The standalone APIs verify three distinct kinds of NEAR AI Cloud evidence:

- a Gateway deployment attestation, including its TLS endpoint binding when
  available;
- a model-serving deployment attestation; and
- a completion signature over exact request and response bytes.

Optional image-provenance helpers verify GitHub build attestations against a
caller-selected repository, workflow and source pin. See the
[guide](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#verify-an-images-build-provenance).

The recommended lifecycle has three stages:

1. Before the request, verify the Gateway deployment and every canonical-model
   deployment candidate returned by the Gateway.
2. Send the chat request and retain its exact request and response bytes.
3. Fetch the completion signature and verify that response receipt against the
   corresponding preflight result.

The Gateway may return zero or multiple model candidates. Reject an empty
preflight, verify every returned candidate, and retain the verified results for
response-receipt selection.

The Gateway and model checks are both useful preflight controls. A completion
signature's `CompletionSignatureKind` only selects the final response-receipt
verifier:

| Kind | Verify with | A successful receipt proves |
| --- | --- | --- |
| `ProviderTee` | `verify_model_response` | The verified model signer signed the exact request and response bytes. |
| `Gateway` | `verify_gateway_response` | The verified Gateway signer signed the exact client-visible request and response bytes. |

The current Gateway interface does not yet provide a cryptographic chain from
a particular model response through a Gateway transformation to the final
response. In particular, preflight Gateway and model attestation candidates do not prove
that they served a particular chat completion. [cloud-api#986](https://github.com/nearai/cloud-api/issues/986)
tracks a provider-signature plus Gateway-receipt design for that complete
chain. Verify both deployments before the request, but do not claim more than
the evidence currently proves.

## Documentation

- [Verification guide](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md) walks through the
  preflight, chat, and response-receipt stages.
- [API reference](https://github.com/nearai/inference-sdk/blob/main/rs/docs/api-reference.md) lists the client, verification
  functions, return values, policies, and callback traits.
- [Runnable Rust examples](https://github.com/nearai/inference-sdk/blob/main/examples/README.md#rust) cover Gateway and direct
  clients, JSON/SSE, standalone verification, and image provenance.

When using standalone verification, retain the exact bytes sent to and received
from the completion endpoint. The integrated client captures these automatically.

Gateway SPKI fingerprint evidence is requested by default. The Rust client
captures the peer certificate for that same HTTPS attestation request. A
runtime without peer-certificate access must use
`GatewayAttestationFetchOptions { include_spki_fingerprint: false, ..Default::default() }`.
That still verifies Gateway deployment evidence, but makes no TLS identity
claim. Model attestation fetches always omit TLS fingerprint evidence because
the Gateway, rather than the client, connects to the model.

## Error handling

Cloud client methods and model-evidence selection return `ApiError`. Local
verification functions return `VerificationError`. Handle each at its own call
site: a client or selection operation never needs to dispatch between the two
error types. Match variants when practical, or use `code()` and `retryable()`
for a stable machine-readable classification; never parse display text. See the
[error-handling section](https://github.com/nearai/inference-sdk/blob/main/rs/docs/verification-guide.md#handle-errors) for
completion-signature failures.

Integrated client methods return `InferenceError`, which wraps `ApiError` or
`VerificationError`. Both `code()` and `retryable()` delegate to the underlying
failure. Transient signature retrieval failures can be retried without replaying
Chat. Verification failures remain failures; the client never automatically
replays inference.

## Development checks

From `rs/`, run `make check` for formatting, lint, and deterministic tests.
`cargo publish --locked --dry-run` verifies the publishable package without
uploading it. Live service checks are separate; see the
[E2E guide](https://github.com/nearai/inference-sdk/blob/main/e2e/README.md#local-use) for model discovery, credentials, and the
explicit command to run the ignored Rust live tests.
