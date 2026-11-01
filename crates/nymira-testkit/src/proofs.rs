//! Proof generator: real zkapi request and withdrawal proofs with NEAR-mapped
//! public inputs, from the development setup (`vendor/zkapi/protocol/setup/
//! v2-concession-dev`). Needs a vault built with the `dev-setup` feature.
//!
//! Layout: the pinned circuit is leaf-bound (`noteLeaf`, `noteFields`). The
//! root-based layout of the earlier zkapi pin is not in the pinned circuit, so
//! there is nothing to prove with it and it is not generated.
//!
//! Negative variants, in the vocabulary of the tickets:
//! * wrong clearance key: [`ProofGen::withdrawal_with_clearance_seed`] makes a valid proof whose
//!   clearance-key inputs are another key's;
//! * right note id, wrong leaf: a valid proof of another note with the same id;
//! * forged `noteFields`, right leaf with a wrong deposit: [`Bundle::with_input`] replaces the
//!   input of a valid proof, which then fails verification;
//! * old state: pass an earlier [`DevState`]; old root: pass an earlier tree.

use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use base64::Engine;
use nymira_near_codec::{
    chain_id, contract_address, destination, public_input_from_wire, CodecError, Network,
};
use zkapi_core::MerkleTree;
use zkapi_proof::compact::{
    CompactSigner, RequestProver, RequestVerifier, WithdrawalProver, WithdrawalVerifier,
    DEVELOPMENT_SIGNING_SEEDS,
};
use zkapi_proof::dev::{DevDeployment, DevNote, DevRequest, DevState, DevWithdrawal};
use zkapi_types::wire::{CurvePointWire, Groth16ProofWire, ProofBackendWire};
use zkapi_types::{Felt252, WithdrawalKind};

/// The circuit layout of the pinned zkapi commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Withdrawals carry `noteLeaf` and `noteFields` and no Merkle path.
    LeafBound,
}

pub const LAYOUT: Layout = Layout::LeafBound;

/// Which statement a proof belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Request,
    Withdrawal,
}

/// Withdrawal kinds, 0, 1 and 2 as the circuit's `kind` input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WithdrawalKindId {
    Escape = 0,
    MutualClose = 1,
    Ownership = 2,
}

/// Positions of the withdrawal statement's 16 public inputs.
pub mod withdrawal_index {
    pub const PROTOCOL_VERSION: usize = 0;
    pub const CHAIN_ID: usize = 1;
    pub const CONTRACT_ADDRESS: usize = 2;
    pub const NOTE_LEAF: usize = 3;
    pub const STATE_KEY_X: usize = 4;
    pub const STATE_KEY_Y: usize = 5;
    pub const CLEARANCE_KEY_X: usize = 6;
    pub const CLEARANCE_KEY_Y: usize = 7;
    pub const NOTE_FIELDS: usize = 8;
    pub const FINAL_BALANCE: usize = 9;
    pub const DESTINATION: usize = 10;
    pub const WITHDRAWAL_NULLIFIER: usize = 11;
    pub const KIND: usize = 12;
    pub const NEXT_NULLIFIER: usize = 13;
    pub const CONCEDED: usize = 14;
    pub const VALID_UNTIL: usize = 15;
}

/// Positions of the request statement's 12 public inputs.
pub mod request_index {
    pub const ACTIVE_ROOT: usize = 3;
    pub const REQUEST_NULLIFIER: usize = 8;
}

/// A proof and its public inputs as zkapi's wire format has them.
#[derive(Clone, Debug)]
pub struct Bundle {
    /// Big-endian 32-byte public input words, in circuit order.
    pub inputs_wire: Vec<[u8; 32]>,
    /// The 256-byte wire proof.
    pub proof_wire: Vec<u8>,
}

impl Bundle {
    /// The same proof with public input `index` replaced: a forgery that must not verify.
    pub fn with_input(&self, index: usize, word: [u8; 32]) -> Self {
        let mut forged = self.clone();
        forged.inputs_wire[index] = word;
        forged
    }

    /// Public inputs as little-endian scalars for `nymira_verifier::verify`.
    pub fn inputs_near(&self) -> std::result::Result<Vec<[u8; 32]>, CodecError> {
        self.inputs_wire.iter().map(|word| public_input_from_wire(word)).collect()
    }

    pub fn proof_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.proof_wire)
    }

    pub fn inputs_base64(&self) -> Vec<String> {
        self.inputs_wire
            .iter()
            .map(|word| base64::engine::general_purpose::STANDARD.encode(word))
            .collect()
    }
}

/// The operator's two public keys.
pub struct Keys {
    pub state: CurvePointWire,
    pub clearance: CurvePointWire,
}

/// Generates proofs for one vault account on one network.
pub struct ProofGen {
    deployment: DevDeployment,
    requests: RequestProver,
    request_verifier: RequestVerifier,
    withdrawals: WithdrawalProver,
    withdrawal_verifier: WithdrawalVerifier,
}

fn setup_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/zkapi/protocol/setup/v2-concession-dev")
}

fn seeded(seed: u64) -> CompactSigner {
    CompactSigner::from_seed(&Felt252::from_u64(seed))
}

impl ProofGen {
    /// Inputs carry `chain_id(network)` and `contract_address(vault_account)`. The state and
    /// clearance keys are the development keys (two separate seeds).
    pub fn new(network: Network, vault_account: &str) -> Result<Self> {
        let dir = setup_dir();
        let deployment = DevDeployment::new(
            chain_id(network),
            Felt252(contract_address(vault_account)),
        );
        Ok(Self {
            deployment,
            requests: RequestProver::load(&dir).context("load request keys")?,
            request_verifier: RequestVerifier::load(&dir)?,
            withdrawals: WithdrawalProver::load(&dir).context("load withdrawal keys")?,
            withdrawal_verifier: WithdrawalVerifier::load(&dir)?,
        })
    }

    /// Replaces the state and clearance keys with ones derived from two seeds.
    pub fn with_keys(mut self, state_seed: u64, clearance_seed: u64) -> Result<Self> {
        ensure!(state_seed != clearance_seed, "the two keys must come from different seeds");
        self.deployment.state_signer = seeded(state_seed);
        self.deployment.clearance_signer = seeded(clearance_seed);
        Ok(self)
    }

    pub fn keys(&self) -> Keys {
        Keys { state: self.deployment.state_key(), clearance: self.deployment.clearance_key() }
    }

    /// The default development seeds (state, clearance).
    pub const DEFAULT_SEEDS: [u64; 2] = DEVELOPMENT_SIGNING_SEEDS;

    fn bundle(
        &self,
        inputs: &[Felt252],
        proof: Groth16ProofWire,
    ) -> Result<Bundle> {
        let proof_wire = base64::engine::general_purpose::STANDARD
            .decode(&proof.proof)
            .context("decode proof")?;
        Ok(Bundle { inputs_wire: inputs.iter().map(|felt| felt.0).collect(), proof_wire })
    }

    /// A request proof from `state` against `tree`'s root.
    pub fn request(&self, state: &DevState, tree: &MerkleTree, request: &DevRequest) -> Result<Bundle> {
        let (public, witness) = state.request(&self.deployment, tree, request)?;
        let proof = self.requests.prove(&public, witness)?;
        self.bundle(&public.to_field_elements(), proof)
    }

    /// The state the honest operator signs after serving `request` with `charge`.
    pub fn next_state(
        &self,
        state: &DevState,
        request: &DevRequest,
        charge: u128,
        server_blinding: &Felt252,
    ) -> Result<DevState> {
        state.successor(&self.deployment, request, charge, server_blinding)
    }

    /// A withdrawal proof. `payout_account` is mapped through the codec; kind 2 (ownership)
    /// has a zero destination whatever is passed.
    pub fn withdrawal(
        &self,
        state: &DevState,
        kind: WithdrawalKindId,
        payout_account: &str,
        conceded: u128,
        valid_until: u64,
    ) -> Result<Bundle> {
        self.withdrawal_with(&self.deployment, state, kind, payout_account, conceded, valid_until)
    }

    /// A mutual close whose clearance key comes from `clearance_seed`: a valid proof whose
    /// clearance-key inputs differ from the vault's.
    pub fn withdrawal_with_clearance_seed(
        &self,
        state: &DevState,
        payout_account: &str,
        conceded: u128,
        valid_until: u64,
        clearance_seed: u64,
    ) -> Result<Bundle> {
        let mut other = self.deployment.clone();
        other.clearance_signer = seeded(clearance_seed);
        self.withdrawal_with(
            &other,
            state,
            WithdrawalKindId::MutualClose,
            payout_account,
            conceded,
            valid_until,
        )
    }

    fn withdrawal_with(
        &self,
        deployment: &DevDeployment,
        state: &DevState,
        kind: WithdrawalKindId,
        payout_account: &str,
        conceded: u128,
        valid_until: u64,
    ) -> Result<Bundle> {
        let (wire_kind, destination) = match kind {
            WithdrawalKindId::Escape => (WithdrawalKind::Escape, destination(payout_account)),
            WithdrawalKindId::MutualClose => {
                (WithdrawalKind::MutualClose, destination(payout_account))
            }
            WithdrawalKindId::Ownership => (WithdrawalKind::Ownership, [0u8; 20]),
        };
        let (public, witness) = state.withdrawal(
            deployment,
            &DevWithdrawal { kind: wire_kind, destination, conceded, valid_until },
        )?;
        let proof = self.withdrawals.prove(&public, witness)?;
        self.bundle(&public.to_field_elements(), proof)
    }

    /// zkapi's own verifier on the wire bundle.
    pub fn native_verifies(&self, bundle: &Bundle, kind: Kind) -> Result<bool> {
        let inputs: Vec<Felt252> = bundle.inputs_wire.iter().map(|word| Felt252(*word)).collect();
        let proof = Groth16ProofWire {
            backend: ProofBackendWire::Groth16Bn254,
            proof: base64::engine::general_purpose::STANDARD.encode(&bundle.proof_wire),
        };
        match kind {
            Kind::Request => self.request_verifier.verify_field_elements(&inputs, &proof),
            Kind::Withdrawal => self.withdrawal_verifier.verify_field_elements(&inputs, &proof),
        }
    }

    /// The verifying key of a statement in the host byte format, from the canonical key bytes.
    pub fn verifying_key(&self, kind: Kind) -> nymira_near_codec::VerifyingKey {
        let (file, key_kind) = match kind {
            Kind::Request => (
                zkapi_proof::compact::REQUEST_VERIFYING_KEY_FILE,
                zkapi_proof::compact::KeyKind::Request,
            ),
            Kind::Withdrawal => (
                zkapi_proof::compact::WITHDRAWAL_VERIFYING_KEY_FILE,
                zkapi_proof::compact::KeyKind::Withdrawal,
            ),
        };
        let bytes = std::fs::read(setup_dir().join(file)).expect("setup key file");
        let (_, canonical) =
            zkapi_proof::compact::strip_key_header(&bytes, key_kind).expect("key header");
        nymira_near_codec::VerifyingKey::from_canonical(canonical).expect("canonical key")
    }
}

/// Note with a vault-assigned id and expiry (a thin helper to keep call sites short).
pub fn note(secret: u64, note_id: u32, deposit_amount: u128, expiry: u64) -> DevNote {
    DevNote { secret: Felt252::from_u64(secret), note_id, deposit_amount, expiry }
}
