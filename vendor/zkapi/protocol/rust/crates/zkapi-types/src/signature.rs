//! Baby-JubJub Schnorr signatures for zkAPI v2.

use serde::{Deserialize, Serialize};

use crate::Felt252;

/// Compact proof-friendly Schnorr signature used by zkAPI v2.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchnorrSignature {
    pub r_x: Felt252,
    pub r_y: Felt252,
    pub s: Felt252,
}

impl SchnorrSignature {
    pub const IDENTITY: Self = Self {
        r_x: Felt252::ZERO,
        r_y: Felt252::ONE,
        s: Felt252::ZERO,
    };
}
