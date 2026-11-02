//! Chunked OHTTP via the maintained `ohttp` implementation; RFC 9292 framing.
use crate::inference::{ByteStream, InferenceResponse};
use crate::{
    errors::protocol, util::decode_hex, ApiError, ApiResource, ApiTransportReason, InferenceError,
    OhttpAttestation, SigningAlgo, SigningIdentity, VerificationError,
};
use futures_util::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    TryStreamExt,
};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Url,
};
use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
const LIMIT: usize = 1 << 30;
const FIELDS_LIMIT: usize = 1 << 20;
// Each informational field section is independently bounded by FIELDS_LIMIT.
// Bound their count as well, before the final response/body budget starts.
const MAX_INFORMATIONAL_RESPONSES: usize = 32;
const CHUNK: usize = 16384;
const HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
];
fn error(reason: impl Into<String>) -> VerificationError {
    protocol("ohttp.decryption_failed", reason)
}
/// Authenticate the raw key configuration against previously verified Gateway evidence.
pub fn verify_ohttp_key_config(
    attestation: &OhttpAttestation,
    signer: &SigningIdentity,
) -> Result<Vec<u8>, VerificationError> {
    let mismatch = || {
        protocol(
            "ohttp.signer_mismatch",
            "configuration signer differs from verified signer",
        )
    };
    if signer.signing_algo != SigningAlgo::Ed25519
        || attestation.signing_algo != SigningAlgo::Ed25519
    {
        return Err(mismatch());
    }
    let key = decode_hex(&attestation.signing_key).map_err(|_| mismatch())?;
    if key != decode_hex(&signer.signing_address).map_err(|_| mismatch())? {
        return Err(mismatch());
    }
    let key: [u8; 32] = key.try_into().map_err(|_| mismatch())?;
    let invalid = || {
        protocol(
            "ohttp.signature_invalid",
            "invalid key configuration signature",
        )
    };
    let config = decode_hex(&attestation.key_config).map_err(|_| invalid())?;
    let signature = decode_hex(&attestation.signature).map_err(|_| invalid())?;
    let signature = ed25519_dalek::Signature::from_slice(&signature).map_err(|_| invalid())?;
    ed25519_dalek::VerifyingKey::from_bytes(&key)
        .map_err(|_| invalid())?
        .verify_strict(&config, &signature)
        .map_err(|_| invalid())?;
    Ok(config)
}
#[derive(Clone)]
pub struct OhttpClient {
    config: Vec<u8>,
    relay: Url,
    client: reqwest::Client,
    forwarded: Vec<String>,
}
/// The supplied configuration must have been authenticated by the caller.
pub fn create_ohttp_client(
    config: &[u8],
    base_url: &str,
    client: reqwest::Client,
    forwarded_headers: Vec<String>,
) -> Result<OhttpClient, InferenceError> {
    // Restrict negotiation to the same suite as the JavaScript and Python SDKs.
    let invalid = || {
        protocol(
            "ohttp.key_config_invalid",
            "expected X25519/HKDF-SHA256/AES128-GCM",
        )
    };
    if config.len() < 41 || config[1..3] != [0, 32] {
        return Err(invalid().into());
    }
    let size = u16::from_be_bytes([config[35], config[36]]) as usize;
    if size == 0
        || !size.is_multiple_of(4)
        || config.len() != 37 + size
        || !config[37..].as_chunks::<4>().0.contains(&[0, 1, 0, 1])
    {
        return Err(invalid().into());
    }
    let mut selected = config[..35].to_vec();
    selected.extend_from_slice(&[0, 4, 0, 1, 0, 1]);
    ohttp::ClientRequest::from_encoded_config(&selected).map_err(|_| invalid())?;
    let mut relay = crate::cloud_api::parse_base_url(base_url)?;
    relay.set_path("/ohttp");
    Ok(OhttpClient {
        config: selected,
        relay,
        client,
        forwarded: forwarded_headers
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect(),
    })
}
#[derive(Clone, Default)]
struct Writer(Arc<Mutex<Vec<u8>>>);
impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.lock().expect("writer lock").extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
fn varint(value: usize, out: &mut Vec<u8>) {
    let (size, bits) = if value < 64 {
        (1, 6)
    } else if value < 16384 {
        (2, 14)
    } else if value < (1 << 30) {
        (4, 30)
    } else {
        (8, 62)
    };
    let prefix = match size {
        1 => 0,
        2 => 1,
        4 => 2,
        _ => 3,
    };
    let bytes = ((value as u64) | ((prefix as u64) << bits)).to_be_bytes();
    out.extend_from_slice(&bytes[8 - size..]);
}
fn vector(bytes: &[u8], out: &mut Vec<u8>) {
    varint(bytes.len(), out);
    out.extend_from_slice(bytes);
}
fn encode(request: &reqwest::Request) -> Result<Vec<u8>, InferenceError> {
    let body = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .ok_or_else(|| error("buffered body required"))?;
    let mut out = vec![0];
    let url = request.url();
    let authority = if request.headers().contains_key("host") {
        ""
    } else {
        &url[url::Position::BeforeHost..url::Position::AfterPort]
    };
    for field in [
        request.method().as_str().as_bytes(),
        url.scheme().as_bytes(),
        authority.as_bytes(),
        url[url::Position::BeforePath..url::Position::AfterQuery].as_bytes(),
    ] {
        vector(field, &mut out);
    }
    let connection = request
        .headers()
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut fields = Vec::new();
    for (name, value) in request.headers() {
        if HOP.contains(&name.as_str())
            || connection
                .split(',')
                .any(|v| v.trim().eq_ignore_ascii_case(name.as_str()))
        {
            continue;
        }
        vector(name.as_str().as_bytes(), &mut fields);
        vector(value.as_bytes(), &mut fields);
    }
    if fields.len() > FIELDS_LIMIT || body.len() > LIMIT {
        return Err(error("message too large").into());
    }
    vector(&fields, &mut out);
    vector(body, &mut out);
    out.push(0);
    out.resize(out.len().div_ceil(CHUNK) * CHUNK, 0);
    Ok(out)
}
impl OhttpClient {
    pub async fn send(
        &self,
        request: reqwest::Request,
    ) -> Result<InferenceResponse, InferenceError> {
        if request.url().origin() != self.relay.origin() {
            return Err(error("request origin differs from configured origin").into());
        }
        let writer = Writer::default();
        let encryption = || protocol("ohttp.encryption_failed", "request encapsulation failed");
        let mut encapsulated = ohttp::ClientRequest::from_encoded_config(&self.config)
            .map_err(|_| encryption())?
            .encapsulate_stream(writer.clone())
            .map_err(|_| encryption())?;
        encapsulated
            .write_all(&encode(&request)?)
            .await
            .map_err(|_| encryption())?;
        encapsulated.close().await.map_err(|_| encryption())?;
        let body = std::mem::take(&mut *writer.0.lock().expect("writer lock"));
        let mut headers = HeaderMap::new();
        for (name, value) in request.headers() {
            if (name == "authorization" || self.forwarded.iter().any(|n| n == name.as_str()))
                && !name.as_str().starts_with("content-")
                && !HOP.contains(&name.as_str())
                && ![
                    "host",
                    "x-signing-algo",
                    "x-client-pub-key",
                    "x-model-pub-key",
                    "x-encryption-version",
                    "x-encrypt-all-fields",
                ]
                .contains(&name.as_str())
            {
                headers.append(name.clone(), value.clone());
            }
        }
        headers.insert(
            "content-type",
            HeaderValue::from_static("message/ohttp-chunked-req"),
        );
        headers.insert("incremental", HeaderValue::from_static("?1"));
        let response = self
            .client
            .post(self.relay.clone())
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|_| ApiError::Transport {
                resource: ApiResource::Ohttp,
                reason: ApiTransportReason::Request,
            })?;
        if !response.status().is_success() {
            return Err(ApiError::HttpStatus {
                resource: ApiResource::Ohttp,
                status: response.status().as_u16(),
            }
            .into());
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("message/ohttp-chunked-res")
        {
            return Err(error("invalid response content type").into());
        }
        let input = response
            .bytes_stream()
            .map_err(io::Error::other)
            .into_async_read();
        let decrypted = encapsulated
            .response(input)
            .map_err(|_| error("response context failed"))?;
        let mut reader = Reader {
            input: Box::pin(decrypted),
        };
        let framing = reader.integer(false).await?;
        if framing != 1 && framing != 3 {
            return Err(error("expected BHTTP response framing").into());
        }
        let known = framing == 1;
        let mut informational = 0;
        let (status, headers) = loop {
            let code = reader.integer(false).await?;
            if !(100..=599).contains(&code) {
                return Err(error("invalid BHTTP status").into());
            }
            if code < 200 {
                if informational == MAX_INFORMATIONAL_RESPONSES {
                    return Err(error("too many informational responses").into());
                }
                informational += 1;
            }
            let headers = reader.fields(known).await?;
            if code >= 200 {
                break (
                    reqwest::StatusCode::from_u16(code as u16)
                        .map_err(|_| error("invalid status"))?,
                    headers,
                );
            }
        };
        let expected = content_length(&headers)?;
        let no_body =
            request.method() == reqwest::Method::HEAD || [204, 205, 304].contains(&status.as_u16());
        let stream: ByteStream = Box::pin(async_stream::try_stream! {
            let mut total = 0usize;
            let mut first = true;
            loop {
                let mut remaining = reader.integer(first).await?;
                first = false;
                if remaining == 0 {
                    break;
                }
                if remaining > LIMIT as u64 {
                    Err(error("content exceeds message limit"))?;
                }
                while remaining > 0 {
                    let size = std::cmp::min(remaining as usize, CHUNK);
                    let piece = reader.exact(size).await?;
                    total += piece.len();
                    remaining -= piece.len() as u64;
                    if total > LIMIT || (!no_body && expected.is_some_and(|n| total > n)) {
                        Err(error("content exceeds declared length or limit"))?;
                    }
                    if !no_body {
                        yield bytes::Bytes::from(piece);
                    }
                }
                if known {
                    break;
                }
            }
            if !no_body && expected.is_some_and(|n| n != total) {
                Err(error("content length mismatch"))?;
            }
            reader.fields(known).await?;
            // Drain and authenticate the final chunk, including after SSE [DONE].
            reader.drain_padding(total).await?;
        });
        Ok(InferenceResponse {
            status,
            headers,
            body: stream,
        })
    }
}
fn content_length(headers: &HeaderMap) -> Result<Option<usize>, VerificationError> {
    let mut result = None;
    for value in headers.get_all("content-length") {
        let text = value
            .to_str()
            .map_err(|_| error("invalid Content-Length"))?;
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(error("invalid Content-Length"));
        }
        let n = text
            .parse::<usize>()
            .map_err(|_| error("invalid Content-Length"))?;
        if result.is_some_and(|r| r != n) {
            return Err(error("conflicting Content-Length"));
        }
        result = Some(n);
    }
    Ok(result)
}
struct Reader {
    input: Pin<Box<dyn AsyncRead + Send>>,
}
impl Reader {
    async fn drain_padding(&mut self, mut total: usize) -> Result<(), VerificationError> {
        let mut buf = [0; CHUNK];
        loop {
            let size = self
                .input
                .read(&mut buf)
                .await
                .map_err(|_| error("invalid final OHTTP chunk"))?;
            if size == 0 {
                return Ok(());
            }
            if size > LIMIT.saturating_sub(total) {
                return Err(error("padding exceeds message limit"));
            }
            total += size;
            if buf[..size].iter().any(|v| *v != 0) {
                return Err(error("invalid BHTTP padding"));
            }
        }
    }
    async fn exact(&mut self, size: usize) -> Result<Vec<u8>, VerificationError> {
        if size > LIMIT {
            return Err(error("message too large"));
        }
        let mut bytes = vec![0; size];
        self.input
            .read_exact(&mut bytes)
            .await
            .map_err(|_| error("truncated or unauthenticated message"))?;
        Ok(bytes)
    }
    async fn integer(&mut self, eof_zero: bool) -> Result<u64, VerificationError> {
        let mut first = [0];
        let n = self
            .input
            .read(&mut first)
            .await
            .map_err(|_| error("invalid OHTTP chunk"))?;
        if n == 0 {
            return if eof_zero {
                Ok(0)
            } else {
                Err(error("missing BHTTP integer"))
            };
        }
        let size = 1 << (first[0] >> 6);
        let mut value = (first[0] & 63) as u64;
        for b in self.exact(size - 1).await? {
            value = (value << 8) | u64::from(b);
        }
        Ok(value)
    }
    async fn fields(&mut self, known: bool) -> Result<HeaderMap, VerificationError> {
        let mut headers = HeaderMap::new();
        if known {
            let size = self.integer(true).await?;
            if size > FIELDS_LIMIT as u64 {
                return Err(error("field section too large"));
            }
            let data = self.exact(size as usize).await?;
            let mut cursor = 0;
            while cursor < data.len() {
                let name = slice_vector(&data, &mut cursor)?;
                let value = slice_vector(&data, &mut cursor)?;
                insert(&mut headers, name, value)?;
            }
        } else {
            let mut total = 0;
            loop {
                let size = self.integer(total == 0).await?;
                if size == 0 {
                    break;
                }
                if size > FIELDS_LIMIT as u64 {
                    return Err(error("field too large"));
                }
                let name = self.exact(size as usize).await?;
                let size = self.integer(false).await?;
                if size > FIELDS_LIMIT as u64 {
                    return Err(error("field too large"));
                }
                let value = self.exact(size as usize).await?;
                total += name.len() + value.len();
                if total > FIELDS_LIMIT {
                    return Err(error("field section too large"));
                }
                insert(&mut headers, &name, &value)?;
            }
        }
        Ok(headers)
    }
}
fn insert(headers: &mut HeaderMap, name: &[u8], value: &[u8]) -> Result<(), VerificationError> {
    let name = HeaderName::from_bytes(name).map_err(|_| error("invalid field name"))?;
    if HOP.contains(&name.as_str()) {
        return Err(error("hop-by-hop BHTTP field"));
    }
    headers
        .try_append(
            name,
            HeaderValue::from_bytes(value).map_err(|_| error("invalid field value"))?,
        )
        .map_err(|_| error("too many BHTTP fields"))?;
    Ok(())
}
fn slice_vector<'a>(data: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], VerificationError> {
    let first = *data.get(*cursor).ok_or_else(|| error("truncated field"))?;
    *cursor += 1;
    let mut size = (first & 63) as u64;
    for _ in 1..(1 << (first >> 6)) {
        size =
            (size << 8) | u64::from(*data.get(*cursor).ok_or_else(|| error("truncated length"))?);
        *cursor += 1;
    }
    let size = usize::try_from(size).map_err(|_| error("field too large"))?;
    let end = cursor
        .checked_add(size)
        .ok_or_else(|| error("field too large"))?;
    let slice = data
        .get(*cursor..end)
        .ok_or_else(|| error("truncated field"))?;
    *cursor = end;
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::ohttp::{
        hpke::{Aead, Kdf, Kem},
        KeyConfig, SymmetricSuite,
    };
    use ed25519_dalek::Signer;
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
    struct Relay {
        config: KeyConfig,
        case: &'static str,
    }
    impl Respond for Relay {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            assert_eq!(request.url.path(), "/ohttp");
            assert_eq!(request.headers["content-type"], "message/ohttp-chunked-req");
            assert_eq!(request.headers["authorization"], "Bearer fixture");
            assert!(request.headers.get("x-model-pub-key").is_none());
            futures::executor::block_on(async {
                let server = ::ohttp::Server::new(self.config.clone()).unwrap();
                let mut incoming = server.decapsulate_stream(request.body.as_slice());
                let mut decoded = Vec::new();
                incoming.read_to_end(&mut decoded).await.unwrap();
                let message =
                    bhttp::Message::read_bhttp(&mut std::io::Cursor::new(decoded)).unwrap();
                assert_eq!(
                    message.control().path().unwrap(),
                    b"/v1/chat/completions?test=1"
                );
                assert_eq!(message.content(), b"{\"model\":\"test\"}");
                assert_eq!(
                    message.header().get(b"x-model-pub-key"),
                    Some(b"inner-key".as_slice())
                );
                let mut body = b"data: [DONE]\n\n".to_vec();
                body.extend_from_slice(&vec![b' '; CHUNK + 10]);
                let mut message = bhttp::Message::response(bhttp::StatusCode::OK);
                message.put_header(b"content-type".to_vec(), b"text/event-stream".to_vec());
                if self.case == "bad_length" {
                    message.put_header(b"content-length".to_vec(), b"1".to_vec());
                }
                message.write_content(&body);
                let mut plaintext = Vec::new();
                message
                    .write_bhttp(
                        if self.case.ends_with("indeterminate") {
                            bhttp::Mode::IndeterminateLength
                        } else {
                            bhttp::Mode::KnownLength
                        },
                        &mut plaintext,
                    )
                    .unwrap();
                if self.case.starts_with("informational") {
                    let count =
                        MAX_INFORMATIONAL_RESPONSES + usize::from(self.case.contains("excess"));
                    let mut prefix = vec![plaintext[0]];
                    for _ in 0..count {
                        varint(103, &mut prefix);
                        // Empty field section in both BHTTP framing modes.
                        prefix.push(0);
                    }
                    prefix.extend_from_slice(&plaintext[1..]);
                    plaintext = prefix;
                }
                if self.case == "bad_padding" {
                    plaintext.extend_from_slice(b"not-padding");
                } else {
                    plaintext.extend_from_slice(&[0; 100]);
                }
                let writer = Writer::default();
                let mut response = incoming.response(writer.clone()).unwrap();
                response.write_all(&plaintext).await.unwrap();
                response.close().await.unwrap();
                let mut encrypted = std::mem::take(&mut *writer.0.lock().unwrap());
                if self.case == "truncated" {
                    encrypted.truncate(encrypted.len() - 1);
                }
                if self.case == "tampered" {
                    *encrypted.last_mut().unwrap() ^= 1;
                }
                ResponseTemplate::new(200)
                    .insert_header("content-type", "message/ohttp-chunked-res")
                    .set_body_bytes(encrypted)
            })
        }
    }
    fn config() -> KeyConfig {
        KeyConfig::new(
            1,
            Kem::X25519Sha256,
            vec![SymmetricSuite::new(Kdf::HkdfSha256, Aead::Aes128Gcm)],
        )
        .unwrap()
    }
    #[tokio::test]
    async fn interoperates_with_chunked_ohttp_and_authenticates_final_framing() {
        for case in [
            "valid",
            "indeterminate",
            "truncated",
            "tampered",
            "bad_padding",
            "bad_length",
            "informational_boundary",
            "informational_boundary_indeterminate",
            "informational_excess",
            "informational_excess_indeterminate",
        ] {
            let server = MockServer::start().await;
            let config = config();
            let raw = config.encode().unwrap();
            Mock::given(wiremock::matchers::any())
                .respond_with(Relay { config, case })
                .mount(&server)
                .await;
            let client = create_ohttp_client(
                &raw,
                &format!("{}/v1", server.uri()),
                crate::inference::ordinary_client().unwrap(),
                vec![],
            )
            .unwrap();
            let mut request = reqwest::Request::new(
                reqwest::Method::POST,
                format!("{}/v1/chat/completions?test=1", server.uri())
                    .parse()
                    .unwrap(),
            );
            *request.body_mut() = Some("{\"model\":\"test\"}".into());
            request
                .headers_mut()
                .insert("authorization", "Bearer fixture".parse().unwrap());
            request
                .headers_mut()
                .insert("x-model-pub-key", "inner-key".parse().unwrap());
            let result = match client.send(request).await {
                Ok(r) => r.bytes().await,
                Err(e) => Err(e),
            };
            if case == "valid"
                || case == "indeterminate"
                || case.starts_with("informational_boundary")
            {
                assert!(result.unwrap().starts_with(b"data: [DONE]\n\n"));
            } else {
                assert_eq!(
                    result.unwrap_err().code(),
                    "ohttp.decryption_failed",
                    "{case}"
                );
            }
        }
    }
    #[tokio::test]
    async fn padding_counts_toward_message_limit_across_reads() {
        // Start near the real limit to exercise boundary accounting without
        // allocating or decrypting a gigabyte of fixture data.
        let allowance = CHUNK + 10;
        for padding in [0, allowance, allowance + 1] {
            let mut reader = Reader {
                input: Box::pin(futures_util::io::Cursor::new(vec![0; padding])),
            };
            let result = reader.drain_padding(LIMIT - allowance).await;
            if padding <= allowance {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().code(), "ohttp.decryption_failed");
            }
        }
        let mut reader = Reader {
            input: Box::pin(futures_util::io::Cursor::new(vec![0])),
        };
        assert!(reader.drain_padding(LIMIT).await.is_err());
    }
    #[test]
    fn excess_header_count_returns_error_without_panicking() {
        let mut headers = HeaderMap::new();
        for index in 0..40000 {
            if insert(&mut headers, format!("x-{index}").as_bytes(), b"v").is_err() {
                return;
            }
        }
        panic!("expected header capacity error");
    }
    #[test]
    fn signed_config_requires_verified_signer_and_rejects_tampering() {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let raw = config().encode().unwrap();
        let signer = SigningIdentity {
            signing_algo: SigningAlgo::Ed25519,
            signing_address: hex::encode(signing.verifying_key().as_bytes()),
        };
        let mut attestation = OhttpAttestation {
            signing_algo: SigningAlgo::Ed25519,
            signing_key: signer.signing_address.clone(),
            key_config: hex::encode(&raw),
            signature: hex::encode(signing.sign(&raw).to_bytes()),
        };
        assert_eq!(verify_ohttp_key_config(&attestation, &signer).unwrap(), raw);
        attestation.key_config.push_str("00");
        assert_eq!(
            verify_ohttp_key_config(&attestation, &signer)
                .unwrap_err()
                .code(),
            "ohttp.signature_invalid"
        );
        attestation.signing_key = "00".repeat(32);
        assert_eq!(
            verify_ohttp_key_config(&attestation, &signer)
                .unwrap_err()
                .code(),
            "ohttp.signer_mismatch"
        );
    }
}
