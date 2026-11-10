# nymira-verifier

[README](../../README.md) · [Website](https://nymira.pages.dev) · [Documentation](https://nymira.pages.dev/#docs) · [Try it in 20 minutes](../../JUDGES.md) · [Threat model](../../THREAT_MODEL.md) · [Releases](https://github.com/nymiralabs/nymira/releases)

Groth16 verification over NEAR's `alt_bn128` host functions, for zkapi proofs at the pinned commit.

What the host enforces and what this crate adds, confirmed on the sandbox and under `testing_env!`:

- The host aborts on points off the curve, on G2 points outside the order-r subgroup and on coordinates at or above the base modulus. The verifier therefore does not multiply `B` by the group order.
- The host accepts the point at infinity. The verifier refuses it in the proof and in the key.
- The verifier refuses public inputs at or above the scalar modulus and any input count other than `ic.len() - 1` (12 for a request proof, 16 for a withdrawal proof), before any host call.

## Gas

Measured on the sandbox (near-workspaces 0.23.0) with the development fixtures at the zkapi pin, from `contracts/verifier-probe/tests/sandbox.rs`.

| Statement | Verification | Planning estimate |
|---|---|---|
| Request (12 inputs) | 35.02 Tgas | 35.3 Tgas |
| Withdrawal (16 inputs) | 36.30 Tgas | 36.7 Tgas |

Building the host key from the canonical bytes (arkworks decode with curve and subgroup checks) costs 39.9 Tgas for the request key and 41.8 Tgas for the withdrawal key. The vault does it once, in `new()`.
