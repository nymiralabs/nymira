use crate::errors::VerificationError;
use crate::types::{AttestationEventLog, RuntimeMeasurements};
use crate::util::{decode_hex, sha384};
use serde_json::{Map, Value};

const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x0800_0001;

struct EventLogEntry {
    path: String,
    digest: String,
    event_type: u32,
    event: String,
    event_payload: String,
    imr: u32,
}

/// Replay RTMR3 from dstack's event log and return authenticated runtime
/// metadata. Runtime event payloads are hashed again rather than trusting the
/// digest supplied by the event log.
pub fn verify_and_replay_rtmr3(
    event_log: &AttestationEventLog,
    quoted_rtmr3: &[u8],
) -> Result<RuntimeMeasurements, VerificationError> {
    if quoted_rtmr3.len() != 48 {
        return Err(VerificationError::Rtmr3Mismatch {
            reason: "wrong_length",
        });
    }

    let events = parse_event_log(event_log)?;
    let mut replayed = vec![0u8; 48];
    let mut count = 0usize;
    let mut measurements = RuntimeMeasurements::default();

    for event in events {
        if event.imr != 3 {
            continue;
        }
        count += 1;
        let digest = event_digest(&event)?;
        let mut input = Vec::with_capacity(replayed.len() + digest.len());
        input.extend_from_slice(&replayed);
        input.extend_from_slice(&digest);
        replayed = sha384(input);

        if event.event_type == DSTACK_RUNTIME_EVENT_TYPE {
            match event.event.as_str() {
                "os-image-hash" => measurements.os_image_hash = Some(event.event_payload),
                "compose-hash" => measurements.compose_hash = Some(event.event_payload),
                _ => {}
            }
        }
    }

    if count == 0 {
        return Err(VerificationError::Rtmr3Mismatch {
            reason: "no_events",
        });
    }
    if replayed != quoted_rtmr3 {
        return Err(VerificationError::Rtmr3Mismatch {
            reason: "replay_mismatch",
        });
    }
    Ok(measurements)
}

fn parse_event_log(
    event_log: &AttestationEventLog,
) -> Result<Vec<EventLogEntry>, VerificationError> {
    let value = match event_log {
        AttestationEventLog::Json(value) => {
            serde_json::from_str(value).map_err(|_| VerificationError::EventLogInvalid {
                path: "event_log".to_owned(),
                reason: "invalid_json",
            })?
        }
        AttestationEventLog::Entries(entries) => Value::Array(entries.clone()),
    };
    let entries = value
        .as_array()
        .ok_or_else(|| VerificationError::EventLogInvalid {
            path: "event_log".to_owned(),
            reason: "invalid_type",
        })?;
    entries
        .iter()
        .enumerate()
        .map(|(index, value)| parse_event_log_entry(value, index))
        .collect()
}

fn parse_event_log_entry(value: &Value, index: usize) -> Result<EventLogEntry, VerificationError> {
    let path = format!("event_log[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| VerificationError::EventLogInvalid {
            path: path.clone(),
            reason: "invalid_type",
        })?;
    let digest = required_string(object, "digest", &path)?;
    let event = optional_string(object, "event", &path)?.unwrap_or_default();
    let event_payload = optional_string(object, "event_payload", &path)?.unwrap_or_default();
    let imr = required_u32(object, "imr", &path)?;
    let event_type = optional_u32(object, "event_type", &path)?.unwrap_or_default();

    Ok(EventLogEntry {
        path,
        digest,
        event_type,
        event,
        event_payload,
        imr,
    })
}

fn event_digest(entry: &EventLogEntry) -> Result<Vec<u8>, VerificationError> {
    if entry.event_type == DSTACK_RUNTIME_EVENT_TYPE {
        let payload = decode_event_hex(
            &entry.event_payload,
            &format!("{}.event_payload", entry.path),
            true,
        )?;
        let mut data = Vec::new();
        data.extend_from_slice(&DSTACK_RUNTIME_EVENT_TYPE.to_le_bytes());
        data.extend_from_slice(b":");
        data.extend_from_slice(entry.event.as_bytes());
        data.extend_from_slice(b":");
        data.extend_from_slice(&payload);
        let computed = sha384(data);

        if !entry.digest.is_empty() {
            let stored = decode_event_hex(&entry.digest, &format!("{}.digest", entry.path), false)?;
            if stored.len() != 48 {
                return Err(VerificationError::EventLogInvalid {
                    path: format!("{}.digest", entry.path),
                    reason: "wrong_length",
                });
            }
            if stored != computed {
                return Err(VerificationError::EventLogInvalid {
                    path: format!("{}.digest", entry.path),
                    reason: "digest_mismatch",
                });
            }
        }
        return Ok(computed);
    }

    let digest = decode_event_hex(&entry.digest, &format!("{}.digest", entry.path), false)?;
    if digest.len() != 48 {
        return Err(VerificationError::EventLogInvalid {
            path: format!("{}.digest", entry.path),
            reason: "wrong_length",
        });
    }
    Ok(digest)
}

fn decode_event_hex(
    value: &str,
    path: &str,
    allow_empty: bool,
) -> Result<Vec<u8>, VerificationError> {
    let normalized = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if (!allow_empty && normalized.is_empty()) || !normalized.len().is_multiple_of(2) {
        return Err(VerificationError::EventLogInvalid {
            path: path.to_owned(),
            reason: "invalid_hex",
        });
    }
    decode_hex(value).map_err(|_| VerificationError::EventLogInvalid {
        path: path.to_owned(),
        reason: "invalid_hex",
    })
}

fn required_string(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<String, VerificationError> {
    optional_string(object, field, path)?.ok_or_else(|| VerificationError::EventLogInvalid {
        path: format!("{path}.{field}"),
        reason: "invalid_type",
    })
}

fn optional_string(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<Option<String>, VerificationError> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(VerificationError::EventLogInvalid {
            path: format!("{path}.{field}"),
            reason: "invalid_type",
        }),
    }
}

fn required_u32(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<u32, VerificationError> {
    optional_u32(object, field, path)?.ok_or_else(|| VerificationError::EventLogInvalid {
        path: format!("{path}.{field}"),
        reason: "invalid_type",
    })
}

fn optional_u32(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<Option<u32>, VerificationError> {
    match object.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| VerificationError::EventLogInvalid {
                path: format!("{path}.{field}"),
                reason: "invalid_type",
            }),
    }
}
