//! Clearance, the operator-sent close and the nullifier status routes.

use axum::http::StatusCode;
use serde_json::{json, Value};
use zkapi_types::wire::{ClearanceRequestV2C, ClearanceResponseV2, RecoveryResponseV2};
use zkapi_types::{Felt252, PROTOCOL_VERSION};

use super::{db_error, felt_hex, Operator, LEASE_BODY_LIMIT, OWNERSHIP_LIFE_SECS};
use crate::closer::{self, CloseError};
use crate::error::{ApiError, ApiResult, Code};
use crate::json::parse_strict;
use crate::signer::SignerError;
use crate::store;

impl From<CloseError> for ApiError {
    fn from(e: CloseError) -> Self {
        let mut error = ApiError::new(e.code(), e.message);
        error.retry_after_secs = e.retry_after_secs;
        error
    }
}

/// `0x` and 64 hex digits, lowercased; anything else is not a nullifier.
fn nullifier_hex(text: &str) -> ApiResult<String> {
    let lower = text.to_ascii_lowercase();
    let ok = lower.len() == 66
        && lower.starts_with("0x")
        && lower[2..].bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(lower)
    } else {
        Err(ApiError::new(
            Code::MalformedRequest,
            "a nullifier is 0x and 64 hex digits",
        ))
    }
}

fn malformed(why: impl Into<String>) -> ApiError {
    ApiError::new(Code::MalformedRequest, why)
}

impl Operator {
    /// `POST /v2/withdraw/clearance`: a kind-2 ownership proof for a nullifier with no open
    /// lease gets a clearance signature, and the nullifier is spent at this operator for good.
    pub async fn clearance(&self, body: &[u8]) -> ApiResult<Value> {
        if body.len() > LEASE_BODY_LIMIT {
            return Err(ApiError::new(
                Code::BodyTooLarge,
                "request body over 64 KiB",
            ));
        }
        let value = parse_strict(body).map_err(malformed)?;
        let request: ClearanceRequestV2C =
            serde_json::from_value(value).map_err(|e| malformed(e.to_string()))?;
        let proof = &request.ownership_proof;
        let inputs = &proof.public_inputs;
        if inputs.protocol_version != PROTOCOL_VERSION {
            return Err(ApiError::new(
                Code::UnsupportedVersion,
                "protocol version not served",
            ));
        }
        if request.withdrawal_nullifier != inputs.withdrawal_nullifier {
            return Err(malformed("the nullifier differs from the proof's"));
        }
        let nullifier = felt_hex(&inputs.withdrawal_nullifier);
        let invalid = |why: &str| ApiError::new(Code::ProofInvalid, why.to_string());
        if inputs.kind != 2
            || inputs.next_nullifier != Felt252::ZERO
            || inputs.conceded != 0
            || inputs.destination != [0u8; 20]
        {
            return Err(invalid("not an ownership proof"));
        }
        if inputs.chain_id != self.cfg.chain_id
            || inputs.contract_address != self.cfg.contract_address
        {
            return Err(invalid("public inputs name another deployment"));
        }
        if inputs.state_signing_key_x != self.state_pk.x
            || inputs.state_signing_key_y != self.state_pk.y
        {
            return Err(invalid("the state key is not the operator's"));
        }
        let now = self.clock.now_secs();
        if inputs.valid_until < now || inputs.valid_until > now + OWNERSHIP_LIFE_SECS {
            return Err(ApiError::new(
                Code::ProofExpired,
                "an ownership proof must end within 3,300 seconds",
            ));
        }
        self.refresh_chain(false).await?;
        {
            let cache = self.cache.lock().await;
            let config = cache.config.as_ref().expect("loaded");
            let (x, y) = (
                felt_hex(&inputs.clearance_signing_key_x),
                felt_hex(&inputs.clearance_signing_key_y),
            );
            if x != config.clearance_pk.x.to_ascii_lowercase()
                || y != config.clearance_pk.y.to_ascii_lowercase()
            {
                return Err(invalid("the clearance key is not the operator's"));
            }
        }

        // Counted once the cheap checks have passed, as for a lease.
        self.count_attempt(&nullifier)?;

        // A nullifier another path holds is refused before the pairing is paid for.
        let held = {
            let n = nullifier.clone();
            self.store
                .run(move |c| store::nullifier_row(c, &n))
                .await
                .map_err(db_error)?
        };
        if let Some(row) = &held {
            if row.state != store::state::CLEARANCE_RESERVED {
                return Err(held_error(&row.state));
            }
        }

        // The vault would refuse the close later; saying so now spares the client a signature it
        // cannot use.
        let status = self
            .vault
            .note(inputs.note_id)
            .await
            .map_err(|_| {
                ApiError::new(Code::OperatorCapacity, "the vault could not be read").retry_after(5)
            })?
            .map(|n| n.status);
        match status.as_deref() {
            Some("Active") => {}
            Some("PendingExit") => return Err(held_error(store::state::EXIT)),
            _ => return Err(ApiError::new(Code::NoteNotActive, "the note is not active")),
        }

        let permit = self.verify_slots.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(Code::OperatorCapacity, "verification pool full").retry_after(1)
        })?;
        self.note_load((self.cfg.max_verifications - self.verify_slots.available_permits()) as u32);
        let verifier = self.withdrawal_verifier.clone();
        let (public, wire) = (inputs.clone(), proof.proof.clone());
        let ok = tokio::task::spawn_blocking(move || {
            let ok = verifier.verify(&public, &wire);
            drop(permit);
            ok
        })
        .await
        .map_err(|_| invalid("verification failed"))?;
        if !ok {
            return Err(invalid("Groth16 verification failed"));
        }

        // The compare-and-set against the lease path: one winner for one nullifier.
        if let Err(state) = self.reserve_clearance(&nullifier).await {
            return Err(held_error(&state));
        }
        let signature = match self
            .signer
            .sign_clearance(inputs.withdrawal_nullifier)
            .await
        {
            Ok(signature) => signature,
            Err(SignerError::Refused(why)) => {
                return Err(ApiError::new(
                    Code::NullifierSeen,
                    format!("refused: {why}"),
                ))
            }
            Err(SignerError::Internal(_)) => {
                return Err(
                    ApiError::new(Code::OperatorCapacity, "the signer failed").retry_after(5)
                )
            }
        };
        Ok(serde_json::to_value(ClearanceResponseV2 {
            status: "signed".into(),
            withdrawal_nullifier: inputs.withdrawal_nullifier,
            signature,
        })
        .expect("a clearance serialises"))
    }

    /// `POST /v1/close`: hands the bytes to the closer and returns the transaction hash. This
    /// process holds no NEAR key.
    pub async fn close(&self, body: &[u8]) -> ApiResult<Value> {
        if body.len() > closer::CLOSE_BODY_LIMIT {
            return Err(ApiError::new(
                Code::BodyTooLarge,
                "request body over 64 KiB",
            ));
        }
        let hash = self.closer.close(body).await?;
        Ok(closer::answer(&hash))
    }

    /// `GET /v2/nullifiers/{n}`: `reserved`, `finalized`, `clearance_reserved` or `unknown`.
    pub async fn nullifier_status(&self, nullifier: &str) -> ApiResult<(StatusCode, Value)> {
        let n = nullifier_hex(nullifier)?;
        let recovery = self.recovery_for_nullifier(&n).await?;
        Ok((
            StatusCode::OK,
            serde_json::to_value(recovery).expect("recovery serialises"),
        ))
    }

    /// `GET /v2/nullifiers/{n}/evidence`: the status with what the operator signed for it.
    pub async fn nullifier_evidence(&self, nullifier: &str) -> ApiResult<(StatusCode, Value)> {
        let n = nullifier_hex(nullifier)?;
        let recovery = self.recovery_for_nullifier(&n).await?;
        let charge = recovery
            .request_response
            .as_ref()
            .map(|r| r.charge_applied.to_string());
        Ok((
            StatusCode::OK,
            json!({
                "nullifier": n,
                "nullifier_status": recovery.nullifier_status,
                "request_response": recovery.request_response,
                "charge_applied": charge,
            }),
        ))
    }

    async fn recovery_for_nullifier(&self, n: &str) -> ApiResult<RecoveryResponseV2> {
        let key = n.to_string();
        let (row, lease) = self
            .store
            .run(move |c| {
                Ok((
                    store::nullifier_row(c, &key)?,
                    store::lease_by_nullifier(c, &key)?,
                ))
            })
            .await
            .map_err(db_error)?;
        let plain = |status: &str, nullifier_status: &str| RecoveryResponseV2 {
            status: status.into(),
            nullifier_status: nullifier_status.into(),
            request_response: None,
            settlement_deadline: None,
        };
        Ok(match row.map(|r| r.state) {
            None => plain("unknown", "unknown"),
            Some(state) => match (state.as_str(), lease) {
                (store::state::CLEARANCE_RESERVED, _) => {
                    plain("clearance_reserved", "clearance_reserved")
                }
                // A nullifier in an exit is spent for good, which is what `finalized` says here.
                (store::state::EXIT, _) => plain("finalized", "finalized"),
                (_, Some(lease)) => self.recovery_of(&lease).await?,
                _ => plain("unknown", "unknown"),
            },
        })
    }
}

/// The refusal for a nullifier another path already holds.
fn held_error(state: &str) -> ApiError {
    match state {
        store::state::RESERVED => {
            ApiError::new(Code::LeaseOpen, "a lease is open for this nullifier")
        }
        store::state::EXIT => ApiError::new(
            Code::NullifierInExit,
            "nullifier is in a pending or finished exit",
        ),
        _ => ApiError::new(Code::NullifierSeen, "nullifier already used"),
    }
}
