//! The depth-32 note tree as zkapi's `Frontier` keeps it: leaves by note ID and the frontier
//! `filled[1..32]`. Hashes are `zkapi_core::v2::merkle_node` with the embedded Poseidon parameters.

use zkapi_core::v2::merkle_node;
use zkapi_core::{Felt252, MERKLE_DEPTH};

use crate::zero_hashes::ZERO_HASHES;

fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    merkle_node(&Felt252(*left), &Felt252(*right)).0
}

/// Records the leaf at `index` (the next free slot). `previous` is the effective leaf at
/// `index - 1` (ignored for even `index`). Nothing changes for an even index; for an odd one
/// `trailing_ones(index)` compressions write `filled[trailing_ones(index)]`.
pub fn append(
    filled: &mut [[u8; 32]; MERKLE_DEPTH],
    index: u32,
    previous: &[u8; 32],
    leaf: &[u8; 32],
) {
    if index & 1 == 0 {
        return;
    }
    let top = index.trailing_ones() as usize;
    let mut current = node(previous, leaf);
    for sibling in &filled[1..top] {
        current = node(sibling, &current);
    }
    filled[top] = current;
}

/// The root with `next` leaves inserted; `last` is the effective leaf at `next - 1`.
pub fn root(filled: &[[u8; 32]; MERKLE_DEPTH], next: u32, last: &[u8; 32]) -> [u8; 32] {
    let mut current = [0u8; 32];
    let mut empty = true;
    for level in 0..MERKLE_DEPTH {
        if (next >> level) & 1 == 1 {
            let left = if level == 0 { last } else { &filled[level] };
            current = node(left, &current);
            empty = false;
        } else if empty {
            current = ZERO_HASHES[level + 1];
        } else {
            current = node(&current, &ZERO_HASHES[level]);
        }
    }
    current
}
