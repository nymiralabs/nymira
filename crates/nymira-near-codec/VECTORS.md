# Codec test vectors

How `nymira-near-codec` maps zkapi's three EVM-shaped public inputs to NEAR values. Any client can check its own mapping against these. All values were computed with Python's `hashlib`, independently of the Rust code, and are asserted in `src/lib.rs`.

## chain_id

First 8 bytes of `sha256(label)`, read big-endian. The label is `near:testnet` on the hackathon deployment and `near:mainnet` in the mainnet profile.

| Label | chain_id |
|---|---|
| `near:testnet` | 7718972759422244814 |
| `near:mainnet` | 11955663840136396698 |

## contract_address

`sha256(vault account id)` reduced modulo the BN254 scalar field `r`, as a 32-byte big-endian word.

| Vault account | contract_address |
|---|---|
| `vault.nymira.testnet` | `2a65065f1eaad4681d3c1d96524e57fc77e645542824b93f3406f437dc746207` |
| `vault.nymira.near` | `102f2fd23933c459339426e689976b38a531f662c4a11b0db57e1fbbf0c757e6` |

The digest of `vault.nymira.near` is above `r`, so it exercises the reduction. The first one is below `r` and passes through unchanged.

## destination

First 20 bytes of `sha256(payout account id)`.

| Payout account | destination |
|---|---|
| `alice.near` | `2dd5dda540767b3a1aa33544bcba38042f4df6de` |
| `vault.nymira.near` | `a15c1b2adcc8a4d65c84f80a0e1b74501dcdaf3c` |

## Time

Every circuit-facing time is Unix seconds: NEAR's `block_timestamp` (nanoseconds) divided by 10^9, rounding down. The codec has no nanosecond field.

| block_timestamp (ns) | seconds |
|---|---|
| 1700000000999999999 | 1700000000 |
| 0 | 0 |
| 18446744073709551615 (`u64::MAX`) | 18446744073 |

## noteFields (leaf-bound withdrawal)

`note_id * 2^192 + deposit * 2^64 + expiry` as a 32-byte big-endian word. `deposit` is a `u128` in token units, `expiry` a `u64` in Unix seconds.

| note_id | deposit | expiry | noteFields |
|---|---|---|---|
| 0 | 1000000 | 4000000000 | `0000000000000000000000000000000000000000000f424000000000ee6b2800` |
| 7 | 5000000 | 1900000000 | `0000000000000007000000000000000000000000004c4b4000000000713fb300` |
| 4294967295 | 2^128-1 | 2^64-1 | `00000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffff` |

The root-based layout of the earlier zkapi pin (`activeRoot`, `noteId`) is not in the pinned circuit and does not ship. It would pass both values through as plain field elements, with no packing.

## Byte formats

- Host G1 point: `x || y`, each 32 bytes little-endian (64 bytes). G2: `x.c0 || x.c1 || y.c0 || y.c1` (128 bytes).
- zkapi wire proof: 8 big-endian 32-byte words, `a.x a.y b.x.c0 b.x.c1 b.y.c0 b.y.c1 c.x c.y`. The codec reverses each word and refuses any word at or above the base modulus `p`.
- Public inputs: big-endian wire words, reversed to little-endian scalars, each below `r`.
- Verifying key: the canonical bytes are the arkworks compressed serialization, the string zkapi's pin check hashes. The codec derives the host form from that string only and refuses trailing bytes and the point at infinity.

`crates/nymira-verifier/tests/fixtures/near-fixture.json` holds real proofs made with these mappings (testnet, `vault.nymira.testnet`, payout `alice.testnet`).
