use crate::{
    fetch_image_provenance, verify_image_provenance, verify_image_provenance_with_signer_identity,
    DeploymentImagesFailureReason as Reason, ImageProvenancePolicy, VerificationError,
};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Verify the required image repositories in a measured deployment's Compose file.
///
/// Call this from a [`crate::DeploymentVerifier`] after the SDK has verified the
/// quote and `app_compose` measurement binding. This helper does not verify that
/// binding itself. Policy keys are container image repositories; policy values
/// identify caller-approved GitHub builds. Unlisted literal images are ignored.
/// Every required repository must appear and every matching reference must have
/// an explicit SHA-256 digest. Compose variables are never resolved, including
/// their defaults. The optional token is a GitHub token, not a Gateway API key.
pub async fn verify_deployment_image_provenance(
    app_compose: &str,
    image_policies: &BTreeMap<String, ImageProvenancePolicy>,
    github_token: Option<&str>,
) -> Result<(), VerificationError> {
    verify_deployment_image_provenance_inner(app_compose, image_policies, None, github_token).await
}

/// Verify deployment image provenance with reusable-workflow signer identities.
///
/// `image_signer_identities` must use exact keys from `image_policies`; each
/// value is an exact certificate SAN URI. Unknown keys are rejected before
/// fetching provenance. Policies absent from this map use the source workflow
/// as their signer.
pub async fn verify_deployment_image_provenance_with_signer_identities(
    app_compose: &str,
    image_policies: &BTreeMap<String, ImageProvenancePolicy>,
    image_signer_identities: &BTreeMap<String, String>,
    github_token: Option<&str>,
) -> Result<(), VerificationError> {
    for repository in image_signer_identities.keys() {
        if !image_policies.contains_key(repository) {
            return Err(VerificationError::InvalidInput {
                field: "image_signer_identities".to_owned(),
                reason: format!("{repository:?} must be an exact key from image_policies"),
            });
        }
    }
    verify_deployment_image_provenance_inner(
        app_compose,
        image_policies,
        Some(image_signer_identities),
        github_token,
    )
    .await
}

async fn verify_deployment_image_provenance_inner(
    app_compose: &str,
    image_policies: &BTreeMap<String, ImageProvenancePolicy>,
    image_signer_identities: Option<&BTreeMap<String, String>>,
    github_token: Option<&str>,
) -> Result<(), VerificationError> {
    // Finish all Compose/reference checks before making any external request.
    for image in select_images(app_compose, image_policies)? {
        let bundles = fetch_image_provenance(&image.policy.repository, &image.digest, github_token)
            .await
            .map_err(|source| VerificationError::ImageProvenanceRequestFailed {
                image_repository: image.repository.to_owned(),
                digest: image.digest.clone(),
                source: Box::new(source),
            })?;
        if let Some(signer_identity) =
            image_signer_identities.and_then(|identities| identities.get(image.policy_key))
        {
            verify_image_provenance_with_signer_identity(
                &bundles,
                &image.digest,
                image.policy,
                signer_identity,
            )
            .await?;
        } else {
            verify_image_provenance(&bundles, &image.digest, image.policy).await?;
        }
    }
    Ok(())
}

struct SelectedImage<'a> {
    policy_key: &'a str,
    repository: &'a str,
    digest: String,
    policy: &'a ImageProvenancePolicy,
}

#[derive(Deserialize)]
struct AppCompose {
    docker_compose_file: String,
}

fn select_images<'a>(
    app_compose: &str,
    image_policies: &'a BTreeMap<String, ImageProvenancePolicy>,
) -> Result<Vec<SelectedImage<'a>>, VerificationError> {
    if image_policies.is_empty() {
        return Err(images_error(Reason::EmptyPolicy, None, None));
    }
    let app: AppCompose = serde_json::from_str(app_compose)
        .map_err(|_| images_error(Reason::InvalidAppCompose, None, None))?;
    let invalid_compose = || images_error(Reason::InvalidDockerCompose, None, None);
    let mut value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&app.docker_compose_file).map_err(|_| invalid_compose())?;
    value.apply_merge().map_err(|_| invalid_compose())?;
    let services = value
        .get("services")
        .and_then(serde_yaml_ng::Value::as_mapping)
        .ok_or_else(invalid_compose)?;
    let mut images = Vec::new();
    for (service, config) in services {
        let service = service.as_str().ok_or_else(invalid_compose)?;
        if !config.is_mapping() {
            return Err(images_error(
                Reason::InvalidDockerCompose,
                None,
                Some(service),
            ));
        }
        let Some(image) = config.get("image").filter(|image| !image.is_null()) else {
            continue;
        };
        let image = image
            .as_str()
            .ok_or_else(|| images_error(Reason::InvalidDockerCompose, None, Some(service)))?;
        if image.contains('$') {
            return Err(images_error(Reason::UnresolvedImage, None, Some(service)));
        }
        images.push((service, image.strip_prefix("docker.io/").unwrap_or(image)));
    }

    let mut selected = Vec::new();
    for (repository, policy) in image_policies {
        let normalized = repository.strip_prefix("docker.io/").unwrap_or(repository);
        let mut found = false;
        for (service, image) in &images {
            let Some(suffix) = image.strip_prefix(normalized) else {
                continue;
            };
            if !suffix.is_empty() && !suffix.starts_with(['@', ':']) {
                continue;
            }
            found = true;
            let digest = pinned_digest(suffix).ok_or_else(|| {
                images_error(Reason::ImageNotPinned, Some(normalized), Some(service))
            })?;
            selected.push(SelectedImage {
                policy_key: repository,
                repository: normalized,
                digest: digest.to_ascii_lowercase(),
                policy,
            });
        }
        if !found {
            return Err(images_error(Reason::ImageMissing, Some(normalized), None));
        }
    }
    Ok(selected)
}

fn pinned_digest(suffix: &str) -> Option<&str> {
    let (tag, digest) = suffix.split_once('@')?;
    if !tag.is_empty() {
        let tag = tag.strip_prefix(':')?;
        if !(1..=128).contains(&tag.len())
            || !tag.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            || !tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
        {
            return None;
        }
    }
    let hash = digest.strip_prefix("sha256:")?;
    (hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit())).then_some(digest)
}

fn images_error(
    reason: Reason,
    image_repository: Option<&str>,
    service: Option<&str>,
) -> VerificationError {
    VerificationError::DeploymentImagesInvalid {
        reason,
        image_repository: image_repository.map(str::to_owned),
        service: service.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiError, ApiResource};
    use serde_json::json;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn policies() -> BTreeMap<String, ImageProvenancePolicy> {
        BTreeMap::from([(
            "example/app".to_owned(),
            ImageProvenancePolicy::new(
                "example/source".to_owned(),
                ".github/workflows/build.yml".to_owned(),
            ),
        )])
    }

    fn app_compose(yaml: &str) -> String {
        json!({ "docker_compose_file": yaml }).to_string()
    }

    #[test]
    fn selects_all_pinned_references_with_aliases_merges_and_registry_aliases() {
        let mut policies = policies();
        let mut second_policy = policies["example/app"].clone();
        second_policy.workflow = ".github/workflows/release.yml".to_owned();
        policies.insert("docker.io/example/app".to_owned(), second_policy);
        let app = app_compose(&format!(
            "x-app: &app\n  image: docker.io/example/app@{DIGEST}\nservices:\n  a: *app\n  b:\n    <<: *app\n    image: example/app:release-1@sha256:{}\n  unrelated:\n    image: example/other:latest\n  empty: {{}}\n  null_image:\n    image: null\n",
            "A".repeat(64)
        ));

        let images = select_images(&app, &policies).unwrap();

        assert_eq!(images.len(), 4);
        assert!(images.iter().all(|image| image.digest == DIGEST));
        assert_eq!(images[0].repository, "example/app");
        assert_eq!(images[0].policy.workflow, ".github/workflows/release.yml");
        assert_eq!(images[2].repository, "example/app");
        assert_eq!(images[2].policy.workflow, ".github/workflows/build.yml");
    }

    #[test]
    fn rejects_empty_policies_and_invalid_compose_shapes() {
        let policies = policies();
        assert!(matches!(
            select_images("{}", &BTreeMap::new()),
            Err(VerificationError::DeploymentImagesInvalid {
                reason: Reason::EmptyPolicy,
                ..
            })
        ));
        for (app, expected) in [
            ("not JSON".to_owned(), Reason::InvalidAppCompose),
            ("{}".to_owned(), Reason::InvalidAppCompose),
            (app_compose("services: ["), Reason::InvalidDockerCompose),
            (app_compose("services: []"), Reason::InvalidDockerCompose),
            (
                app_compose("services: {app: null}"),
                Reason::InvalidDockerCompose,
            ),
            (
                app_compose("services: {app: {image: 42}}"),
                Reason::InvalidDockerCompose,
            ),
        ] {
            assert!(
                matches!(
                    select_images(&app, &policies),
                    Err(VerificationError::DeploymentImagesInvalid { reason, .. }) if reason == expected
                ),
                "{app}"
            );
        }
    }

    #[test]
    fn rejects_every_unpinned_matching_reference_even_beside_a_pinned_one() {
        let policies = policies();
        for reference in [
            "example/app".to_owned(),
            "example/app:latest".to_owned(),
            "example/app@sha256:abc".to_owned(),
            format!("example/app:-bad@{DIGEST}"),
            format!("example/app:é@{DIGEST}"),
            format!("example/app:{}@{DIGEST}", "a".repeat(129)),
        ] {
            let app = app_compose(&format!(
                "services:\n  valid:\n    image: example/app@{DIGEST}\n  invalid:\n    image: {reference}\n"
            ));
            assert!(
                matches!(
                    select_images(&app, &policies),
                    Err(VerificationError::DeploymentImagesInvalid {
                        reason: Reason::ImageNotPinned,
                        image_repository: Some(repository), service: Some(service),
                    }) if repository == "example/app" && service == "invalid"
                ),
                "{reference}"
            );
        }
    }

    #[test]
    fn requires_each_repository_and_rejects_variables_in_unrelated_images() {
        let policies = policies();
        let app = app_compose("services: {app: {image: example/app-extra:latest}}");
        assert!(matches!(
            select_images(&app, &policies),
            Err(VerificationError::DeploymentImagesInvalid {
                reason: Reason::ImageMissing,
                ..
            })
        ));
        let app = app_compose(&format!(
            "services:\n  app:\n    image: example/app@{DIGEST}\n  other:\n    image: ${{OTHER_IMAGE:-example/other:latest}}\n"
        ));
        assert!(matches!(
            select_images(&app, &policies),
            Err(VerificationError::DeploymentImagesInvalid {
                reason: Reason::UnresolvedImage, service: Some(service), ..
            }) if service == "other"
        ));
    }

    #[tokio::test]
    async fn rejects_unmatched_signer_keys_before_fetch() {
        let mut policies = policies();
        // A fetch would fail locally, so this also checks validation happens first.
        policies.get_mut("example/app").unwrap().repository = "invalid".to_owned();
        let app = app_compose(&format!(
            "services:\n  app:\n    image: example/app@{DIGEST}\n"
        ));

        for key in ["example/ap", "docker.io/example/app"] {
            let identities = BTreeMap::from([(key.to_owned(), "signer".to_owned())]);
            let error = verify_deployment_image_provenance_with_signer_identities(
                &app,
                &policies,
                &identities,
                None,
            )
            .await
            .unwrap_err();

            assert!(matches!(
                error,
                VerificationError::InvalidInput { field, reason }
                    if field == "image_signer_identities" && reason.contains(key)
            ));
        }
    }

    #[tokio::test]
    async fn validates_all_references_before_fetch_and_preserves_request_errors() {
        let mut policies = policies();
        // This fails locally inside fetch_image_provenance, without a network request.
        policies.get_mut("example/app").unwrap().repository = "invalid".to_owned();
        policies.insert("example/other".to_owned(), policies["example/app"].clone());
        let app = app_compose(&format!(
            "services:\n  app:\n    image: example/app@{DIGEST}\n  other:\n    image: example/other:latest\n"
        ));
        let error = verify_deployment_image_provenance(&app, &policies, None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            VerificationError::DeploymentImagesInvalid {
                reason: Reason::ImageNotPinned,
                ..
            }
        ));
        assert_eq!(error.code(), "provenance.deployment_images_invalid");

        policies.remove("example/other");
        let error = verify_deployment_image_provenance(&app, &policies, None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "provenance.image_request_failed");
        assert!(!error.retryable());
        assert!(matches!(
            error,
            VerificationError::ImageProvenanceRequestFailed {
                image_repository, digest, source,
            } if image_repository == "example/app" && digest == DIGEST
                && matches!(*source, ApiError::InvalidInput { .. })
        ));
        let retryable = VerificationError::ImageProvenanceRequestFailed {
            image_repository: "example/app".to_owned(),
            digest: DIGEST.to_owned(),
            source: Box::new(ApiError::HttpStatus {
                resource: ApiResource::ImageProvenance,
                status: 503,
            }),
        };
        assert!(retryable.retryable());
        assert!(std::error::Error::source(&retryable).is_some());
    }
}
