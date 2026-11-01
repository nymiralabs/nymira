//! Nullifier state shared by the server transcript store.

use serde::{Deserialize, Serialize};

/// Nullifier status in the server's transcript store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NullifierStatus {
    Reserved,
    Finalized,
    ClearanceReserved,
}
