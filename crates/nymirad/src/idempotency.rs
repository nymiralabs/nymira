//! Idempotency keys for agent chat requests.
//!
//! An agent that sends `Idempotency-Key` and then retries the same request (a timeout, a lost
//! connection) gets the first answer back instead of a second charge. A result is found by
//! (token, note, key, request): the key alone, another token, another note or another request
//! under the same key finds nothing. Results live in memory only, for a few minutes, with an
//! entry cap. Nothing here is written to the state directory or a backup, and nothing is logged.
//!
//! A retry that arrives while the first request still runs waits for it and gets its answer.
//! Only answered requests are kept: a refusal or a failure is run again by the retry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const HEADER: &str = "idempotency-key";
/// How long a result can be replayed.
pub const TTL_SECS: u64 = 300;
/// At most this many results at once; the oldest goes first.
pub const MAX_ENTRIES: usize = 64;
const MAX_KEY_LEN: usize = 128;

/// An answered request, as it is replayed.
#[derive(Clone)]
pub struct Stored {
    pub answer: Value,
    pub lease: String,
    pub signature_kind: Option<&'static str>,
}

pub type Cell = Arc<tokio::sync::OnceCell<Option<Stored>>>;

#[derive(Default)]
pub struct Idempotency {
    map: Mutex<HashMap<String, (u64, Cell)>>,
}

/// A key is 1 to 128 printable ASCII characters.
pub fn valid_key(key: &str) -> bool {
    (1..=MAX_KEY_LEN).contains(&key.len()) && key.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// Where a result is kept: a hash over the token, the note, the key and the request.
pub fn entry_key(token_hash: &str, note: &str, key: &str, request: &Value) -> String {
    let mut h = Sha256::new();
    for part in [token_hash.as_bytes(), note.as_bytes(), key.as_bytes()] {
        h.update(part);
        h.update([0]);
    }
    h.update(serde_json::to_vec(request).expect("a JSON value serializes"));
    hex::encode(h.finalize())
}

impl Idempotency {
    /// The cell for `key`, made if it is new. Expired entries are dropped first; at the cap the
    /// oldest entry makes room.
    pub fn slot(&self, key: &str, now: u64) -> Cell {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, (at, _)| now < *at + TTL_SECS);
        if let Some((_, cell)) = map.get(key) {
            return cell.clone();
        }
        if map.len() >= MAX_ENTRIES {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        let cell = Cell::default();
        map.insert(key.into(), (now, cell.clone()));
        cell
    }

    /// Drops an entry whose request did not end in an answer.
    pub fn forget(&self, key: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_entry_depends_on_token_note_key_and_request() {
        let r = json!({"a": 1});
        let base = entry_key("t", "n", "k", &r);
        assert_eq!(base, entry_key("t", "n", "k", &json!({"a": 1})));
        assert_ne!(base, entry_key("t2", "n", "k", &r));
        assert_ne!(base, entry_key("t", "n2", "k", &r));
        assert_ne!(base, entry_key("t", "n", "k2", &r));
        assert_ne!(base, entry_key("t", "n", "k", &json!({"a": 2})));
        // The parts cannot run into each other.
        assert_ne!(entry_key("t", "nk", "", &r), entry_key("t", "n", "k", &r));
    }

    #[test]
    fn keys_are_short_printable_and_non_empty() {
        assert!(valid_key("abc-123"));
        assert!(!valid_key(""));
        assert!(!valid_key("has space"));
        assert!(!valid_key(&"x".repeat(129)));
        assert!(valid_key(&"x".repeat(128)));
    }

    #[test]
    fn entries_expire_and_the_oldest_makes_room_at_the_cap() {
        let i = Idempotency::default();
        let first = i.slot("first", 1_000);
        assert!(Arc::ptr_eq(&first, &i.slot("first", 1_001)));
        for n in 0..MAX_ENTRIES {
            i.slot(&format!("k{n}"), 1_002);
        }
        assert_eq!(i.len(), MAX_ENTRIES);
        assert!(
            !Arc::ptr_eq(&first, &i.slot("first", 1_003)),
            "the oldest was dropped"
        );
        assert_eq!(i.len(), MAX_ENTRIES);
        i.slot("late", 1_003 + TTL_SECS);
        assert_eq!(i.len(), 1, "everything older than the TTL is gone");
    }
}
