use crate::errors::{ApiError, ApiResource, ApiTransportReason, VerificationError};
use crate::types::{
    ImageProvenanceFailureReason as Reason, ImageProvenancePolicy, VerifiedImageProvenance,
};
use reqwest::{header::HeaderMap, Client, Url};
use serde::Deserialize;
use sigstore_verify::{
    trust_root::{TrustedRoot, SIGSTORE_PRODUCTION_TRUSTED_ROOT},
    types::{bundle::VerificationMaterialContent, Bundle, Sha256Hash, SignatureContent},
    VerificationPolicy, Verifier,
};
use std::collections::HashSet;
use x509_cert::{
    der::{asn1::ObjectIdentifier, asn1::Utf8StringRef, Decode},
    ext::Extension,
    Certificate,
};

const RESOURCE: ApiResource = ApiResource::ImageProvenance;
const SLSA_V1: &str = "https://slsa.dev/provenance/v1";
const SLSA_V02: &str = "https://slsa.dev/provenance/v0.2";
const SOURCE_REPOSITORY_URI: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.12");
const SOURCE_REPOSITORY_DIGEST: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.13");
const SOURCE_REPOSITORY_REF: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.14");
const GITHUB_WORKFLOW_SHA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.3");
const GITHUB_WORKFLOW_REPOSITORY: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.5");
const GITHUB_WORKFLOW_REF: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.6");

/// Fetch inline GitHub attestation bundles for a `sha256:…` image digest.
///
/// This only retrieves evidence. Call [`verify_image_provenance`] before using
/// its contents. The optional token is a GitHub token, not a Gateway API key.
pub async fn fetch_image_provenance(
    repository: &str,
    digest: &str,
    github_token: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    fetch_image_provenance_with_client(&Client::new(), repository, digest, github_token).await
}

/// Nymira patch: [`fetch_image_provenance`] over a client the caller built, so the request can
/// go through a SOCKS5 proxy. The upstream function connects directly.
pub async fn fetch_image_provenance_with_client(
    client: &Client,
    repository: &str,
    digest: &str,
    github_token: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    if !valid_repository(repository) {
        return Err(api_input("repository", "expected owner/repo"));
    }
    let hash = image_hash(digest).ok_or_else(|| {
        api_input(
            "digest",
            "expected sha256: followed by 64 hexadecimal digits",
        )
    })?;
    let url = Url::parse(&format!(
        "https://api.github.com/repos/{repository}/attestations/sha256:{hash}?per_page=100"
    ))
    .expect("validated repository and digest form a valid GitHub URL");
    fetch_image_provenance_pages(client, url, github_token).await
}

async fn fetch_image_provenance_pages(
    client: &Client,
    original_url: Url,
    github_token: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    let mut bundles = Vec::new();
    let mut url = original_url.clone();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(url.clone()) {
            return Err(pagination_error("repeated pagination cursor"));
        }
        let mut request = client
            .get(url.clone())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "nearai-inference-sdk");
        if let Some(token) = github_token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|_| ApiError::Transport {
            resource: RESOURCE,
            reason: ApiTransportReason::Request,
        })?;
        if !response.status().is_success() {
            return Err(ApiError::HttpStatus {
                resource: RESOURCE,
                status: response.status().as_u16(),
            });
        }
        let next = next_attestation_page(&original_url, response.headers())?;
        let text = response.text().await.map_err(|_| ApiError::Transport {
            resource: RESOURCE,
            reason: ApiTransportReason::ResponseBody,
        })?;
        let response: GitHubAttestations = serde_json::from_str(&text).map_err(|error| {
            if error.is_syntax() || error.is_eof() {
                ApiError::InvalidJson { resource: RESOURCE }
            } else {
                ApiError::InvalidResponse {
                    path: "attestations".to_owned(),
                    expected: "an array of inline bundle objects".to_owned(),
                    actual: "invalid attestation response".to_owned(),
                }
            }
        })?;
        bundles.extend(
            response
                .attestations
                .into_iter()
                .map(|entry| serde_json::Value::Object(entry.bundle).to_string()),
        );
        let Some(next) = next else {
            return Ok(bundles);
        };
        url = next;
    }
}

fn next_attestation_page(original_url: &Url, headers: &HeaderMap) -> Result<Option<Url>, ApiError> {
    for header in headers.get_all(reqwest::header::LINK) {
        let header = header
            .to_str()
            .map_err(|_| pagination_error("invalid Link header"))?;
        for link in header.split(',') {
            let mut parts = link.trim().split(';');
            let target = parts.next().unwrap_or_default().trim();
            let is_next = parts.any(|part| {
                part.trim().split_once('=').is_some_and(|(name, value)| {
                    name.trim().eq_ignore_ascii_case("rel")
                        && value
                            .trim()
                            .trim_matches('"')
                            .split_whitespace()
                            .any(|relation| relation == "next")
                })
            });
            if !is_next {
                continue;
            }
            let target = target
                .strip_prefix('<')
                .and_then(|target| target.strip_suffix('>'))
                .and_then(|target| Url::parse(target).ok())
                .filter(|target| matches!(target.scheme(), "http" | "https"))
                .ok_or_else(|| pagination_error("invalid next link URL"))?;
            let mut cursors = target
                .query_pairs()
                .filter(|(name, _)| name == "before" || name == "after");
            let (name, value) = cursors
                .next()
                .filter(|(_, value)| !value.is_empty())
                .ok_or_else(|| pagination_error("missing next link cursor"))?;
            if cursors.next().is_some() {
                return Err(pagination_error("multiple next link cursors"));
            }
            // Retain the caller's validated GitHub origin and attestation path;
            // only the cursor is taken from the server-supplied URL.
            let mut url = original_url.clone();
            url.set_query(None);
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair(&name, &value);
            return Ok(Some(url));
        }
    }
    Ok(None)
}

fn pagination_error(actual: &str) -> ApiError {
    ApiError::InvalidResponse {
        path: "Link".to_owned(),
        expected: "a next link with a new before or after cursor".to_owned(),
        actual: actual.to_owned(),
    }
}

/// Verify one matching GitHub build provenance from a collection of bundles.
///
/// The Sigstore library verifies the DSSE signature, Fulcio certificate, SCT,
/// Rekor inclusion proof/checkpoint and artifact digest using its embedded
/// public-good trust root. This function additionally checks the caller's
/// source repository, workflow and optional ref, and binds the SLSA source
/// commit to the certificate's source digest before applying an optional pin.
/// It does not discover images, approve a deployment or rebuild the artifact.
pub async fn verify_image_provenance(
    bundles: &[String],
    digest: &str,
    policy: &ImageProvenancePolicy,
) -> Result<VerifiedImageProvenance, VerificationError> {
    verify_image_provenance_inner(bundles, digest, policy, None).await
}

/// Verify image provenance when a reusable workflow is the exact signer.
///
/// `policy` continues to identify the caller/source repository and workflow.
/// `signer_identity` is the certificate SAN URI of the signing workflow,
/// including its exact ref, tag, or commit SHA.
pub async fn verify_image_provenance_with_signer_identity(
    bundles: &[String],
    digest: &str,
    policy: &ImageProvenancePolicy,
    signer_identity: &str,
) -> Result<VerifiedImageProvenance, VerificationError> {
    verify_image_provenance_inner(bundles, digest, policy, Some(signer_identity)).await
}

async fn verify_image_provenance_inner(
    bundles: &[String],
    digest: &str,
    policy: &ImageProvenancePolicy,
    signer_identity: Option<&str>,
) -> Result<VerifiedImageProvenance, VerificationError> {
    let hash = image_hash(digest).ok_or_else(|| VerificationError::InvalidInput {
        field: "digest".to_owned(),
        reason: "expected sha256: followed by 64 hexadecimal digits".to_owned(),
    })?;
    let normalized_digest = format!("sha256:{hash}");
    let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)
        .map_err(|_| provenance_error(&normalized_digest, vec![Reason::TrustRootUnavailable]))?;
    let verifier = Verifier::new(&root);
    let mut reasons = Vec::new();
    for bundle in bundles {
        match verify_bundle(bundle, &hash, policy, signer_identity, &verifier) {
            Ok(result) => return Ok(result),
            Err(reason) if !reasons.contains(&reason) => reasons.push(reason),
            Err(_) => {}
        }
    }
    if reasons.is_empty() {
        reasons.push(Reason::NoAttestations);
    }
    Err(provenance_error(&normalized_digest, reasons))
}

fn verify_bundle(
    json: &str,
    hash: &str,
    policy: &ImageProvenancePolicy,
    signer_identity: Option<&str>,
    verifier: &Verifier,
) -> Result<VerifiedImageProvenance, Reason> {
    let bundle = Bundle::from_json(json).map_err(|_| Reason::InvalidBundle)?;
    let SignatureContent::DsseEnvelope(envelope) = &bundle.content else {
        return Err(Reason::InvalidStatement);
    };
    // Sigstore bundles use one signature. In particular, the signature verified
    // by DSSE must be the same one authenticated by the transparency log.
    if envelope.signatures.len() != 1 {
        return Err(Reason::InvalidBundle);
    }
    if envelope.payload_type != "application/vnd.in-toto+json" {
        return Err(Reason::InvalidStatement);
    }
    let statement: Statement = serde_json::from_slice(envelope.payload.as_bytes())
        .map_err(|_| Reason::InvalidStatement)?;
    if !matches!(
        statement.statement_type.as_str(),
        "https://in-toto.io/Statement/v1" | "https://in-toto.io/Statement/v0.1"
    ) {
        return Err(Reason::InvalidStatement);
    }
    // This preliminary comparison only rejects a candidate. No statement is
    // trusted or returned until the cryptographic verifier below succeeds.
    if !statement.subject.iter().any(|subject| {
        subject
            .digest
            .sha256
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(hash))
    }) {
        return Err(Reason::DigestMismatch);
    }
    let artifact = Sha256Hash::from_hex(hash).map_err(|_| Reason::DigestMismatch)?;
    let verified = verifier
        .verify(artifact, &bundle, &VerificationPolicy::default())
        .map_err(|_| Reason::InvalidBundle)?;
    let identity = verified.identity.ok_or(Reason::UntrustedIdentity)?;
    let issuer = verified.issuer.ok_or(Reason::UntrustedIdentity)?;
    if issuer != policy.issuer {
        return Err(Reason::UntrustedIdentity);
    }
    let default_signer_ref = if let Some(expected) = signer_identity {
        if identity != expected {
            return Err(Reason::UntrustedIdentity);
        }
        None
    } else {
        let identity_prefix = format!(
            "https://github.com/{}/{}@",
            policy.repository, policy.workflow
        );
        let signer_ref = identity
            .strip_prefix(&identity_prefix)
            .filter(|value| value.starts_with("refs/"))
            .ok_or(Reason::UntrustedIdentity)?;
        if policy
            .git_ref
            .as_deref()
            .is_some_and(|expected| expected != signer_ref)
        {
            return Err(Reason::UntrustedIdentity);
        }
        Some(signer_ref)
    };
    let source = certificate_source(&bundle.verification_material.content, policy)?;
    if default_signer_ref.is_some_and(|signer_ref| signer_ref != source.git_ref) {
        return Err(Reason::SourceMismatch);
    }
    let (commit, predicate_type) = source_commit(
        statement.provenance,
        policy,
        &source.git_ref,
        &source.commit,
    )?;
    Ok(VerifiedImageProvenance {
        digest: format!("sha256:{hash}"),
        repository: policy.repository.clone(),
        workflow: policy.workflow.clone(),
        git_ref: source.git_ref,
        commit,
        certificate_identity: identity,
        issuer,
        predicate_type: predicate_type.to_owned(),
    })
}

struct CertificateSource {
    git_ref: String,
    commit: String,
}

fn certificate_source(
    material: &VerificationMaterialContent,
    policy: &ImageProvenancePolicy,
) -> Result<CertificateSource, Reason> {
    let der = match material {
        VerificationMaterialContent::Certificate(certificate) => &certificate.raw_bytes,
        VerificationMaterialContent::X509CertificateChain { certificates } => {
            &certificates
                .first()
                .ok_or(Reason::SourceMismatch)?
                .raw_bytes
        }
        VerificationMaterialContent::PublicKey { .. } => return Err(Reason::SourceMismatch),
    };
    let certificate = Certificate::from_der(der.as_bytes()).map_err(|_| Reason::SourceMismatch)?;
    source_extensions(
        certificate
            .tbs_certificate
            .extensions
            .as_deref()
            .unwrap_or_default(),
        policy,
    )
}

fn source_extensions(
    extensions: &[Extension],
    policy: &ImageProvenancePolicy,
) -> Result<CertificateSource, Reason> {
    let repository = source_extension(
        extensions,
        SOURCE_REPOSITORY_URI,
        GITHUB_WORKFLOW_REPOSITORY,
        "https://github.com/",
    )?;
    let git_ref = source_extension(extensions, SOURCE_REPOSITORY_REF, GITHUB_WORKFLOW_REF, "")?;
    if repository != format!("https://github.com/{}", policy.repository)
        || !git_ref.starts_with("refs/")
        || policy
            .git_ref
            .as_deref()
            .is_some_and(|expected| expected != git_ref)
    {
        return Err(Reason::SourceMismatch);
    }
    Ok(CertificateSource {
        git_ref,
        commit: source_commit_extension(extensions)?,
    })
}

fn source_extension(
    extensions: &[Extension],
    modern: ObjectIdentifier,
    legacy: ObjectIdentifier,
    legacy_prefix: &str,
) -> Result<String, Reason> {
    // A present modern claim is authoritative; malformed data must not fall back.
    if let Some(extension) = extensions
        .iter()
        .find(|extension| extension.extn_id == modern)
    {
        Ok(Utf8StringRef::from_der(extension.extn_value.as_bytes())
            .map_err(|_| Reason::SourceMismatch)?
            .as_str()
            .to_owned())
    } else {
        let extension = extensions
            .iter()
            .find(|extension| extension.extn_id == legacy)
            .ok_or(Reason::SourceMismatch)?;
        let value = std::str::from_utf8(extension.extn_value.as_bytes())
            .map_err(|_| Reason::SourceMismatch)?;
        Ok(format!("{legacy_prefix}{value}"))
    }
}

fn source_commit_extension(extensions: &[Extension]) -> Result<String, Reason> {
    // The source digest is independent of a reusable workflow's signer digest.
    let commit = source_extension(
        extensions,
        SOURCE_REPOSITORY_DIGEST,
        GITHUB_WORKFLOW_SHA,
        "",
    )?;
    if !is_commit(&commit) {
        return Err(Reason::SourceMismatch);
    }
    Ok(commit.to_ascii_lowercase())
}

fn source_commit(
    provenance: Provenance,
    policy: &ImageProvenancePolicy,
    git_ref: &str,
    certificate_commit: &str,
) -> Result<(String, &'static str), Reason> {
    let (commit, predicate_type) = match provenance {
        Provenance::V1(predicate) => {
            let definition = predicate.build_definition;
            let workflow = definition.external_parameters.workflow;
            let expected_repository = format!("https://github.com/{}", policy.repository);
            if workflow
                .repository
                .strip_suffix(".git")
                .unwrap_or(&workflow.repository)
                != expected_repository
                || workflow.path != policy.workflow
                || workflow.git_ref != git_ref
            {
                return Err(Reason::SourceMismatch);
            }
            let commit = definition
                .resolved_dependencies
                .iter()
                .find(|material| require_source(&material.uri, policy, git_ref).is_ok())
                .and_then(|material| material.digest.git_commit.as_deref())
                .filter(|commit| is_commit(commit))
                .ok_or(Reason::SourceMismatch)?
                .to_ascii_lowercase();
            (commit, SLSA_V1)
        }
        Provenance::V02(predicate) => {
            let source = predicate.invocation.config_source;
            if source.entry_point != policy.workflow {
                return Err(Reason::SourceMismatch);
            }
            require_source(&source.uri, policy, git_ref)?;
            let commit = source
                .digest
                .sha1
                .filter(|commit| is_commit(commit))
                .ok_or(Reason::SourceMismatch)?
                .to_ascii_lowercase();
            (commit, SLSA_V02)
        }
    };
    if !commit.eq_ignore_ascii_case(certificate_commit) {
        return Err(Reason::SourceMismatch);
    }
    if policy
        .commit
        .as_deref()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&commit))
    {
        return Err(Reason::CommitMismatch);
    }
    Ok((commit, predicate_type))
}

fn require_source(uri: &str, policy: &ImageProvenancePolicy, git_ref: &str) -> Result<(), Reason> {
    let (repository, reference) = github_source(uri).ok_or(Reason::SourceMismatch)?;
    if repository != policy.repository || reference != Some(git_ref) {
        return Err(Reason::SourceMismatch);
    }
    Ok(())
}

fn github_source(uri: &str) -> Option<(&str, Option<&str>)> {
    let uri = uri.strip_prefix("git+").unwrap_or(uri);
    let path = uri.strip_prefix("https://github.com/")?;
    let (repository, reference) = path
        .split_once('@')
        .map_or((path, None), |(repo, reference)| (repo, Some(reference)));
    Some((
        repository.strip_suffix(".git").unwrap_or(repository),
        reference,
    ))
}

fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn image_hash(digest: &str) -> Option<String> {
    let value = digest.strip_prefix("sha256:")?;
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn valid_repository(repository: &str) -> bool {
    let Some((owner, repo)) = repository.split_once('/') else {
        return false;
    };
    [owner, repo].into_iter().all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || b"-_.".contains(&value))
    })
}

fn api_input(field: &str, reason: &str) -> ApiError {
    ApiError::InvalidInput {
        field: field.to_owned(),
        reason: reason.to_owned(),
        expected: None,
        actual: None,
    }
}

fn provenance_error(digest: &str, reasons: Vec<Reason>) -> VerificationError {
    VerificationError::ImageProvenanceVerificationFailed {
        digest: digest.to_owned(),
        reasons,
    }
}

#[derive(Deserialize)]
struct GitHubAttestations {
    attestations: Vec<GitHubAttestation>,
}

#[derive(Deserialize)]
struct GitHubAttestation {
    bundle: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct Statement {
    #[serde(rename = "_type")]
    statement_type: String,
    subject: Vec<Subject>,
    #[serde(flatten)]
    provenance: Provenance,
}

#[derive(Deserialize)]
struct Subject {
    digest: SubjectDigest,
}

#[derive(Deserialize)]
struct SubjectDigest {
    sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "predicateType", content = "predicate")]
enum Provenance {
    #[serde(rename = "https://slsa.dev/provenance/v1")]
    V1(PredicateV1),
    #[serde(rename = "https://slsa.dev/provenance/v0.2")]
    V02(PredicateV02),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PredicateV1 {
    build_definition: BuildDefinition,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildDefinition {
    external_parameters: ExternalParameters,
    resolved_dependencies: Vec<Material>,
}

#[derive(Deserialize)]
struct ExternalParameters {
    workflow: Workflow,
}

#[derive(Deserialize)]
struct Workflow {
    repository: String,
    path: String,
    #[serde(rename = "ref")]
    git_ref: String,
}

#[derive(Deserialize)]
struct PredicateV02 {
    invocation: Invocation,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Invocation {
    config_source: ConfigSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigSource {
    uri: String,
    #[serde(default)]
    digest: SourceDigest,
    entry_point: String,
}

#[derive(Deserialize)]
struct Material {
    uri: String,
    #[serde(default)]
    digest: SourceDigest,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceDigest {
    git_commit: Option<String>,
    sha1: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        matchers::{header, method, path, query_param, query_param_is_missing},
        Mock, MockServer, ResponseTemplate,
    };
    use x509_cert::der::{asn1::OctetString, Encode};

    const COMMIT: &str = "8e07c3583909c9ab9da94d883e87add1ae90832d";

    fn policy() -> ImageProvenancePolicy {
        ImageProvenancePolicy::new(
            "nearai/compose-manager".to_owned(),
            ".github/workflows/build.yml".to_owned(),
        )
    }

    fn certificate_extension(oid: ObjectIdentifier, value: &[u8]) -> Extension {
        Extension {
            extn_id: oid,
            critical: false,
            extn_value: OctetString::new(value).unwrap(),
        }
    }

    fn text_extension(oid: ObjectIdentifier, value: &str) -> Extension {
        certificate_extension(oid, &Utf8StringRef::new(value).unwrap().to_der().unwrap())
    }

    #[test]
    fn extracts_source_repository_and_ref_with_independent_legacy_fallback() {
        let legacy_repository =
            certificate_extension(GITHUB_WORKFLOW_REPOSITORY, b"nearai/compose-manager");
        let legacy_ref = certificate_extension(GITHUB_WORKFLOW_REF, b"refs/heads/master");
        let legacy_commit = certificate_extension(GITHUB_WORKFLOW_SHA, COMMIT.as_bytes());
        for extensions in [
            vec![
                text_extension(
                    SOURCE_REPOSITORY_URI,
                    "https://github.com/nearai/compose-manager",
                ),
                text_extension(SOURCE_REPOSITORY_REF, "refs/heads/master"),
                text_extension(SOURCE_REPOSITORY_DIGEST, COMMIT),
                certificate_extension(GITHUB_WORKFLOW_REPOSITORY, b"ignored/legacy"),
                certificate_extension(GITHUB_WORKFLOW_REF, b"refs/heads/ignored"),
            ],
            vec![
                legacy_repository.clone(),
                legacy_ref.clone(),
                legacy_commit.clone(),
            ],
            vec![
                text_extension(
                    SOURCE_REPOSITORY_URI,
                    "https://github.com/nearai/compose-manager",
                ),
                legacy_ref,
                legacy_commit,
            ],
        ] {
            let source = source_extensions(&extensions, &policy()).unwrap();
            assert_eq!(source.git_ref, "refs/heads/master");
            assert_eq!(source.commit, COMMIT);
        }
    }

    #[test]
    fn rejects_missing_or_malformed_source_identity_without_legacy_downgrade() {
        let legacy = vec![
            certificate_extension(GITHUB_WORKFLOW_REPOSITORY, b"nearai/compose-manager"),
            certificate_extension(GITHUB_WORKFLOW_REF, b"refs/heads/master"),
            certificate_extension(GITHUB_WORKFLOW_SHA, COMMIT.as_bytes()),
        ];
        for modern in [
            certificate_extension(
                SOURCE_REPOSITORY_URI,
                b"https://github.com/nearai/compose-manager",
            ),
            text_extension(
                SOURCE_REPOSITORY_URI,
                "https://github.com/another/repository",
            ),
            text_extension(SOURCE_REPOSITORY_URI, ""),
            certificate_extension(SOURCE_REPOSITORY_REF, b"refs/heads/master"),
            text_extension(SOURCE_REPOSITORY_REF, "master"),
            text_extension(SOURCE_REPOSITORY_REF, ""),
        ] {
            let mut extensions = legacy.clone();
            extensions.push(modern);
            assert!(matches!(
                source_extensions(&extensions, &policy()),
                Err(Reason::SourceMismatch)
            ));
        }
        for missing in [GITHUB_WORKFLOW_REPOSITORY, GITHUB_WORKFLOW_REF] {
            let extensions = legacy
                .iter()
                .filter(|extension| extension.extn_id != missing)
                .cloned()
                .collect::<Vec<_>>();
            assert!(matches!(
                source_extensions(&extensions, &policy()),
                Err(Reason::SourceMismatch)
            ));
        }
    }

    #[test]
    fn extracts_the_certificate_source_digest_with_legacy_fallback() {
        let modern = |value: &str| {
            certificate_extension(
                SOURCE_REPOSITORY_DIGEST,
                &Utf8StringRef::new(value).unwrap().to_der().unwrap(),
            )
        };
        let legacy = certificate_extension(GITHUB_WORKFLOW_SHA, COMMIT.as_bytes());
        let signer = certificate_extension(
            ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.10"),
            &Utf8StringRef::new(&"b".repeat(40))
                .unwrap()
                .to_der()
                .unwrap(),
        );
        let config = Extension {
            extn_id: ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.19"),
            ..signer.clone()
        };
        for (extensions, expected) in [
            (
                vec![
                    modern(&COMMIT.to_ascii_uppercase()),
                    certificate_extension(GITHUB_WORKFLOW_SHA, b"not-the-source"),
                    signer.clone(),
                    config.clone(),
                ],
                Ok(COMMIT.to_owned()),
            ),
            (vec![legacy.clone()], Ok(COMMIT.to_owned())),
            (
                vec![
                    certificate_extension(SOURCE_REPOSITORY_DIGEST, COMMIT.as_bytes()),
                    legacy.clone(),
                ],
                Err(Reason::SourceMismatch),
            ),
            (
                vec![modern("not-a-sha"), legacy],
                Err(Reason::SourceMismatch),
            ),
            (vec![signer, config], Err(Reason::SourceMismatch)),
            (vec![], Err(Reason::SourceMismatch)),
        ] {
            assert_eq!(source_commit_extension(&extensions), expected);
        }
    }

    #[test]
    fn requires_certificate_source_commit_even_when_statement_matches_pin() {
        let certificate_commit = source_commit_extension(&[certificate_extension(
            SOURCE_REPOSITORY_DIGEST,
            &Utf8StringRef::new(&"b".repeat(40))
                .unwrap()
                .to_der()
                .unwrap(),
        )])
        .unwrap();
        for pin in [None, Some(COMMIT.to_owned())] {
            let mut policy = policy();
            policy.commit = pin;
            let provenance = Provenance::V02(PredicateV02 {
                invocation: Invocation {
                    config_source: ConfigSource {
                        uri: "git+https://github.com/nearai/compose-manager@refs/heads/master"
                            .to_owned(),
                        digest: SourceDigest {
                            sha1: Some(COMMIT.to_owned()),
                            ..Default::default()
                        },
                        entry_point: policy.workflow.clone(),
                    },
                },
            });
            assert_eq!(
                source_commit(
                    provenance,
                    &policy,
                    "refs/heads/master",
                    &certificate_commit
                ),
                Err(Reason::SourceMismatch)
            );
        }
    }

    #[tokio::test]
    async fn fetches_cursor_pages_at_the_original_origin_and_path() {
        let client = Client::builder().no_proxy().build().unwrap();
        for cursor_name in ["before", "after"] {
            let server = MockServer::start().await;
            let endpoint_path = "/repos/nearai/compose-manager/attestations/sha256:abc";
            let initial =
                Url::parse(&format!("{}{endpoint_path}?per_page=100", server.uri())).unwrap();
            let next_link = format!(
                "<https://example.com/untrusted?per_page=1&page=2&{cursor_name}=a%2B%2F%3D%26>; rel=\"next\""
            );
            Mock::given(method("GET"))
                .and(path(endpoint_path))
                .and(query_param("per_page", "100"))
                .and(query_param_is_missing(cursor_name))
                .and(header("Authorization", "Bearer test-token"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Link", next_link)
                        .set_body_json(json!({"attestations": [{"bundle": {"page": 1}}]})),
                )
                .expect(1)
                .mount(&server)
                .await;
            let final_bundles = (0..100)
                .map(|index| json!({"index": index}))
                .collect::<Vec<_>>();
            Mock::given(method("GET"))
                .and(path(endpoint_path))
                .and(query_param("per_page", "100"))
                .and(query_param(cursor_name, "a+/=&"))
                .and(header("Authorization", "Bearer test-token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "attestations": final_bundles.iter().map(|bundle| json!({"bundle": bundle})).collect::<Vec<_>>()
                })))
                .expect(1)
                .mount(&server)
                .await;

            let bundles =
                fetch_image_provenance_pages(&client, initial.clone(), Some("test-token"))
                    .await
                    .unwrap();

            let mut expected = vec![json!({"page": 1}).to_string()];
            expected.extend(final_bundles.iter().map(ToString::to_string));
            assert_eq!(bundles, expected);
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests
                .iter()
                .all(|request| request.url.path() == endpoint_path));
            assert_eq!(requests[0].url.query(), Some("per_page=100"));
            assert_eq!(
                requests[1].url.query().unwrap(),
                format!("per_page=100&{cursor_name}=a%2B%2F%3D%26")
            );
        }
    }

    #[tokio::test]
    async fn rejects_repeated_pagination_before_repeating_a_request() {
        let client = Client::builder().no_proxy().build().unwrap();
        let server = MockServer::start().await;
        let initial = Url::parse(&format!("{}/attestations?per_page=100", server.uri())).unwrap();
        let next_link = format!("<{}&after=repeated>; rel=\"next\"", initial);
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Link", next_link)
                    .set_body_json(json!({"attestations": [{"bundle": {}}]})),
            )
            .expect(2)
            .up_to_n_times(2)
            .mount(&server)
            .await;

        let result = fetch_image_provenance_pages(&client, initial, None).await;

        assert!(matches!(result, Err(ApiError::InvalidResponse { path, .. }) if path == "Link"));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[test]
    fn rejects_malformed_or_missing_next_cursors() {
        let initial = Url::parse("https://api.github.com/attestations?per_page=100").unwrap();
        for link in [
            "<not-a-url>; rel=\"next\"",
            "<https://api.github.com/attestations?per_page=100>; rel=\"next\"",
            "<https://api.github.com/attestations?after=>; rel=\"next\"",
            "<https://api.github.com/attestations?after=a&before=b>; rel=\"next\"",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(reqwest::header::LINK, link.parse().unwrap());
            assert!(matches!(
                next_attestation_page(&initial, &headers),
                Err(ApiError::InvalidResponse { path, .. }) if path == "Link"
            ));
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LINK,
            "<https://api.github.com/attestations?before=a>; rel=\"prev\""
                .parse()
                .unwrap(),
        );
        assert_eq!(next_attestation_page(&initial, &headers).unwrap(), None);
    }

    #[test]
    fn parses_slsa_v02_invoked_source_and_commit() {
        let statement = json!({
            "predicateType": SLSA_V02,
            "predicate": {
                "invocation": {
                    "configSource": {
                        "uri": "git+https://github.com/nearai/compose-manager.git@refs/heads/master",
                        "entryPoint": ".github/workflows/build.yml",
                        "digest": { "sha1": COMMIT }
                    }
                }
            }
        });
        let provenance = serde_json::from_value(statement).unwrap();
        let result = source_commit(provenance, &policy(), "refs/heads/master", COMMIT).unwrap();

        assert_eq!(result, (COMMIT.to_owned(), SLSA_V02));
    }

    #[test]
    fn selects_the_exact_slsa_v1_source_instead_of_the_first_dependency() {
        let statement = json!({
            "predicateType": SLSA_V1,
            "predicate": {
                "buildDefinition": {
                    "externalParameters": {
                        "workflow": {
                            "repository": "https://github.com/nearai/compose-manager",
                            "path": ".github/workflows/build.yml",
                            "ref": "refs/heads/master"
                        }
                    },
                    "resolvedDependencies": [
                        {
                            "uri": "git+https://github.com/other/repo@refs/heads/main",
                            "digest": { "gitCommit": "a".repeat(40) }
                        },
                        {
                            "uri": "git+https://github.com/nearai/compose-manager@refs/heads/master",
                            "digest": { "gitCommit": COMMIT }
                        }
                    ]
                }
            }
        });
        let provenance = serde_json::from_value(statement.clone()).unwrap();
        let result = source_commit(provenance, &policy(), "refs/heads/master", COMMIT).unwrap();
        assert_eq!(result, (COMMIT.to_owned(), SLSA_V1));

        let provenance = serde_json::from_value(statement).unwrap();
        assert_eq!(
            source_commit(provenance, &policy(), "refs/heads/feature", COMMIT),
            Err(Reason::SourceMismatch)
        );
    }
}
