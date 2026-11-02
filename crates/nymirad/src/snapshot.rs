//! The vault as of one final block: parameters, root, leaf count and the tree rebuilt from
//! `get_leaves`. The operator never supplies a path.

use serde_json::{json, Value};
use zkapi_client::tree::SparseTree;
use zkapi_types::Felt252;

use crate::rpc::{Rpc, RpcError};

/// The vault's page cap for `get_leaves`.
const PAGE: u32 = 100;

pub struct Snapshot {
    pub block_hash: String,
    pub block_height: u64,
    pub root: Felt252,
    pub leaf_count: u32,
    /// `config()` at the snapshot block.
    pub config: Value,
    pub tree: SparseTree,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("the leaves do not build the vault's root ({0})")]
    RootMismatch(&'static str),
    #[error("the vault returned a malformed {0}")]
    Malformed(&'static str),
}

fn word(v: &Value, what: &'static str) -> Result<Felt252, SnapshotError> {
    v.as_str()
        .and_then(|s| Felt252::from_hex(s).ok())
        .ok_or(SnapshotError::Malformed(what))
}

/// The tree of `leaves`, checked against the vault's `expected` root.
pub fn tree_from_leaves(
    leaves: &[Felt252],
    expected: &Felt252,
) -> Result<SparseTree, SnapshotError> {
    let tree = SparseTree::from_leaves(leaves);
    if tree.root() != *expected {
        return Err(SnapshotError::RootMismatch(
            "rebuilt root differs from current_root",
        ));
    }
    Ok(tree)
}

/// Reads the vault at the latest final block and rebuilds its tree from `get_leaves`, starting
/// at 0.
pub async fn take(rpc: &Rpc, vault: &str) -> Result<Snapshot, SnapshotError> {
    let (block_hash, block_height) = rpc.final_block().await?;
    let at = json!({ "block_id": block_hash });
    let view = |method: &'static str, args: Value| rpc.view_at(at.clone(), vault, method, args);

    let config = view("config", json!({})).await?;
    let root = word(&view("current_root", json!({})).await?, "current_root")?;
    let leaf_count = view("leaf_count", json!({}))
        .await?
        .as_u64()
        .ok_or(SnapshotError::Malformed("leaf_count"))? as u32;

    let mut leaves = Vec::with_capacity(leaf_count as usize);
    while (leaves.len() as u32) < leaf_count {
        let page = view("get_leaves", json!({ "from": leaves.len(), "limit": PAGE })).await?;
        let page = page
            .as_array()
            .filter(|p| !p.is_empty())
            .ok_or(SnapshotError::Malformed("get_leaves"))?;
        for leaf in page {
            leaves.push(word(leaf, "leaf")?);
        }
    }
    if leaves.len() as u32 != leaf_count {
        return Err(SnapshotError::Malformed("get_leaves"));
    }
    let tree = tree_from_leaves(&leaves, &root)?;
    Ok(Snapshot {
        block_hash,
        block_height,
        root,
        leaf_count,
        config,
        tree,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_root_is_refused() {
        let leaves = [Felt252::from_u64(5), Felt252::from_u64(6)];
        let right = SparseTree::from_leaves(&leaves).root();
        assert!(tree_from_leaves(&leaves, &right).is_ok());
        assert!(matches!(
            tree_from_leaves(&leaves, &Felt252::from_u64(1)),
            Err(SnapshotError::RootMismatch(_))
        ));
        // A leaf the vault did not report changes the root.
        let altered = [Felt252::from_u64(5), Felt252::from_u64(7)];
        assert!(tree_from_leaves(&altered, &right).is_err());
    }

    #[test]
    fn an_empty_vault_has_the_empty_root() {
        let empty = SparseTree::new().root();
        assert!(tree_from_leaves(&[], &empty).is_ok());
    }
}
