use crate::errors::VerificationError;
use crate::types::{TcbStatus, TdxQuoteVerificationResult, TdxQuoteVerifier};
use crate::util::decode_hex;
use async_trait::async_trait;
use dcap_qvl::collateral::get_collateral_with_client;
use dcap_qvl::quote::Quote;
use dcap_qvl::verify::verify;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_INTEL_PCCS_URL: &str = "https://api.trustedservices.intel.com";

/// Built-in Intel TDX quote verifier, using Intel's official PCS endpoint by default.
#[derive(Clone, Debug)]
pub struct DefaultTdxQuoteVerifier {
    pccs_url: String,
    /// Nymira patch: the client the collateral is fetched with. `None` builds a direct one.
    client: Option<reqwest::Client>,
}

impl Default for DefaultTdxQuoteVerifier {
    fn default() -> Self {
        Self::new(DEFAULT_INTEL_PCCS_URL)
    }
}

impl DefaultTdxQuoteVerifier {
    /// Retrieve collateral from a PCCS-compatible base URL. This changes the
    /// collateral source while retaining Intel's cryptographic trust roots.
    pub fn new(pccs_url: impl Into<String>) -> Self {
        Self {
            pccs_url: pccs_url.into(),
            client: None,
        }
    }

    /// Nymira patch: fetch collateral over this client (a SOCKS5 proxy, for instance).
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }
}

#[async_trait]
impl TdxQuoteVerifier for DefaultTdxQuoteVerifier {
    async fn verify(
        &self,
        intel_quote: &str,
    ) -> Result<TdxQuoteVerificationResult, VerificationError> {
        verify_tdx_quote_with_client(self.client.as_ref(), &self.pccs_url, intel_quote).await
    }
}

/// Verify an Intel TDX quote using DCAP and expose the measurements required
/// by the shared model/Gateway verification flow.
pub async fn verify_tdx_quote(
    pccs_url: &str,
    intel_quote: &str,
) -> Result<TdxQuoteVerificationResult, VerificationError> {
    verify_tdx_quote_with_client(None, pccs_url, intel_quote).await
}

/// Nymira patch: [`verify_tdx_quote`] with the collateral fetched over `client`.
pub async fn verify_tdx_quote_with_client(
    client: Option<&reqwest::Client>,
    pccs_url: &str,
    intel_quote: &str,
) -> Result<TdxQuoteVerificationResult, VerificationError> {
    let quote_bytes =
        decode_hex(intel_quote).map_err(|_| VerificationError::QuoteVerificationFailed {
            reason: "invalid_encoding",
        })?;

    // Validate the quote before requesting collateral. Invalid quote bytes are
    // permanent local failures, not a retryable PCCS failure.
    Quote::parse(&quote_bytes).map_err(|_| VerificationError::QuoteVerificationFailed {
        reason: "invalid_quote",
    })?;

    let collateral = match client {
        Some(client) => get_collateral_with_client(client, pccs_url, &quote_bytes).await,
        None => dcap_qvl::collateral::get_collateral(pccs_url, &quote_bytes).await,
    }
    .map_err(|_| VerificationError::QuoteCollateralUnavailable)?;
    let current_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| VerificationError::QuoteVerificationFailed {
            reason: "verifier_error",
        })?
        .as_secs();
    let verified = verify(&quote_bytes, &collateral, current_time).map_err(|_| {
        VerificationError::QuoteVerificationFailed {
            reason: "verifier_error",
        }
    })?;
    let td10 = verified
        .report
        .as_td10()
        .ok_or(VerificationError::QuoteUnsupportedReportType)?;

    Ok(TdxQuoteVerificationResult {
        tcb_status: parse_tcb_status(&verified.status)?,
        advisory_ids: verified.advisory_ids,
        debug_enabled: (td10.td_attributes[0] & 0x01) != 0,
        report_data: td10.report_data.to_vec(),
        mr_config_id: td10.mr_config_id.to_vec(),
        rt_mr3: td10.rt_mr3.to_vec(),
    })
}

fn parse_tcb_status(status: &str) -> Result<TcbStatus, VerificationError> {
    match status {
        "UpToDate" => Ok(TcbStatus::UpToDate),
        "SWHardeningNeeded" => Ok(TcbStatus::SwHardeningNeeded),
        "ConfigurationNeeded" => Ok(TcbStatus::ConfigurationNeeded),
        "ConfigurationAndSWHardeningNeeded" => Ok(TcbStatus::ConfigurationAndSwHardeningNeeded),
        "OutOfDate" => Ok(TcbStatus::OutOfDate),
        "OutOfDateConfigurationNeeded" => Ok(TcbStatus::OutOfDateConfigurationNeeded),
        "Revoked" => Ok(TcbStatus::Revoked),
        "Unknown" => Ok(TcbStatus::Unknown),
        _ => Err(VerificationError::QuoteInvalidResult {
            reason: "unsupported_tcb_status".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[test]
    fn uses_the_official_intel_endpoint_by_default() {
        assert_eq!(
            DefaultTdxQuoteVerifier::default().pccs_url,
            "https://api.trustedservices.intel.com"
        );
    }

    #[tokio::test]
    async fn requests_collateral_from_the_custom_pccs_base_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxy/sgx/certification/v4/pckcert"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;

        // Structurally valid SGX quote with synthetic signatures and encrypted
        // PPID data: enough to request a PCK certificate, never to pass verification.
        let mut auth = vec![0; 64 + 64 + 384 + 64];
        auth.extend(0_u16.to_le_bytes()); // Empty QE authentication data.
        auth.extend(2_u16.to_le_bytes()); // Encrypted PPID-2048 certification.
        auth.extend(276_u32.to_le_bytes());
        auth.extend([0; 276]);
        let mut quote = vec![0; 48 + 384];
        quote[..2].copy_from_slice(&3_u16.to_le_bytes());
        quote.extend((auth.len() as u32).to_le_bytes());
        quote.extend(auth);
        let quote = hex::encode(quote);

        for suffix in ["/proxy", "/proxy/tdx/certification/v4/"] {
            let verifier = DefaultTdxQuoteVerifier::new(format!("{}{suffix}", server.uri()));
            let error = verifier.verify(&quote).await.unwrap_err();
            assert!(matches!(
                error,
                VerificationError::QuoteCollateralUnavailable
            ));
        }
    }

    #[tokio::test]
    async fn rejects_invalid_quotes_before_contacting_the_custom_pccs() {
        let server = MockServer::start().await;
        let verifier = DefaultTdxQuoteVerifier::new(server.uri());
        for (quote, expected) in [("zz", "invalid_encoding"), ("00", "invalid_quote")] {
            let error = verifier.verify(quote).await.unwrap_err();
            assert!(matches!(
                error,
                VerificationError::QuoteVerificationFailed { reason } if reason == expected
            ));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn rejects_an_unrecognized_tcb_status() {
        let error = parse_tcb_status("FutureStatus").unwrap_err();
        assert!(matches!(
            error,
            VerificationError::QuoteInvalidResult { ref reason }
                if reason == "unsupported_tcb_status"
        ));
    }
}
