//! Pin TLS before HTTP writes while retaining certificate/hostname validation.
use crate::{
    errors::protocol,
    util::{require_hex_length, sha256},
    VerificationError,
};
use rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use std::sync::Arc;
use x509_cert::{
    der::{Decode, Encode},
    Certificate,
};

pub fn create_pinned_tls_client(
    fingerprints: &[String],
) -> Result<reqwest::Client, VerificationError> {
    create_pinned_tls_client_via(fingerprints, None)
}

/// Nymira patch: the pinned client through a SOCKS5 proxy. TLS still ends at the Gateway, so the
/// pin is checked against the Gateway's own certificate.
pub fn create_pinned_tls_client_via(
    fingerprints: &[String],
    socks_proxy: Option<&str>,
) -> Result<reqwest::Client, VerificationError> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    pinned_client_via(fingerprints, roots, socks_proxy)
}

#[cfg(test)]
pub(crate) fn pinned_client(
    fingerprints: &[String],
    roots: rustls::RootCertStore,
) -> Result<reqwest::Client, VerificationError> {
    pinned_client_via(fingerprints, roots, None)
}

pub(crate) fn pinned_client_via(
    fingerprints: &[String],
    roots: rustls::RootCertStore,
    socks_proxy: Option<&str>,
) -> Result<reqwest::Client, VerificationError> {
    if fingerprints.is_empty() {
        return Err(VerificationError::SpkiFingerprintRequired);
    }
    let pins = fingerprints
        .iter()
        .map(|s| {
            require_hex_length(s, 32)
                .map_err(|_| protocol("input.invalid", "expected a SHA256 SPKI fingerprint"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let normal = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|_| protocol("input.invalid", "invalid TLS roots"))?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| protocol("input.invalid", "invalid TLS configuration"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier { normal, pins }))
        .with_no_client_auth();
    let mut builder = reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if let Some(url) = socks_proxy {
        let proxy = reqwest::Proxy::all(url)
            .map_err(|_| protocol("input.invalid", "invalid SOCKS proxy"))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|_| protocol("input.invalid", "could not create pinned TLS client"))
}
#[derive(Debug)]
struct PinnedVerifier {
    normal: Arc<WebPkiServerVerifier>,
    pins: Vec<Vec<u8>>,
}
impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verified = self
            .normal
            .verify_server_cert(cert, intermediates, name, ocsp, now)?;
        let der = Certificate::from_der(cert.as_ref())
            .and_then(|c| c.tbs_certificate.subject_public_key_info.to_der())
            .map_err(|_| {
                rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
            })?;
        if !self.pins.iter().any(|pin| pin.as_slice() == sha256(&der)) {
            return Err(rustls::Error::General(
                "binding.spki_fingerprint_mismatch".into(),
            ));
        }
        Ok(verified)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.normal.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.normal.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.normal.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn pin_hostname_and_chain_failures_precede_http_writes() {
        for case in ["valid", "wrong_pin", "wrong_hostname", "untrusted"] {
            let certified = rcgen::generate_simple_self_signed(vec![if case == "wrong_hostname" {
                "elsewhere.test"
            } else {
                "localhost"
            }
            .to_owned()])
            .unwrap();
            let cert = certified.cert.der().clone();
            let private =
                rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let server = rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert.clone()], private.into())
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
                if let Ok(mut socket) = acceptor.accept(socket).await {
                    let mut buf = [0; 4096];
                    let count = socket.read(&mut buf).await.unwrap_or_default();
                    if count > 0 {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    }
                    count > 0
                } else {
                    false
                }
            });
            let parsed = Certificate::from_der(cert.as_ref()).unwrap();
            let spki = parsed
                .tbs_certificate
                .subject_public_key_info
                .to_der()
                .unwrap();
            let pin = if case == "wrong_pin" {
                "00".repeat(32)
            } else {
                hex::encode(sha256(spki))
            };
            let mut roots = rustls::RootCertStore::empty();
            if case == "untrusted" {
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            } else {
                roots.add(cert).unwrap();
            }
            let client = pinned_client(&[pin], roots).unwrap();
            let response = client
                .post(format!("https://localhost:{}/", address.port()))
                .body("confidential prompt")
                .send()
                .await;
            assert_eq!(response.is_ok(), case == "valid", "{case}");
            assert_eq!(
                task.await.unwrap(),
                case == "valid",
                "HTTP was written for {case}"
            );
        }
    }
    #[test]
    fn rejects_empty_and_invalid_pins() {
        assert_eq!(
            create_pinned_tls_client(&[]).unwrap_err().code(),
            "binding.spki_fingerprint_required"
        );
        assert!(create_pinned_tls_client(&["not-hex".into()]).is_err());
    }
}
