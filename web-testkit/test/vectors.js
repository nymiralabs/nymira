// The vectors of crates/nymira-near-codec/VECTORS.md (computed there with
// Python's hashlib, independently of the Rust code). Large numbers are strings
// so the table can cross into a page.

export const vectors = {
  chainId: [
    ['testnet', '7718972759422244814'],
    ['mainnet', '11955663840136396698'],
  ],
  // The second digest is above the scalar modulus, so it exercises the reduction.
  contractAddress: [
    ['vault.nymira.testnet', '2a65065f1eaad4681d3c1d96524e57fc77e645542824b93f3406f437dc746207'],
    ['vault.nymira.near', '102f2fd23933c459339426e689976b38a531f662c4a11b0db57e1fbbf0c757e6'],
  ],
  destination: [
    ['alice.near', '2dd5dda540767b3a1aa33544bcba38042f4df6de'],
    ['vault.nymira.near', 'a15c1b2adcc8a4d65c84f80a0e1b74501dcdaf3c'],
  ],
  unixSeconds: [
    ['1700000000999999999', '1700000000'],
    ['0', '0'],
    ['18446744073709551615', '18446744073'],
  ],
  noteFields: [
    ['0', '1000000', '4000000000', '0000000000000000000000000000000000000000000f424000000000ee6b2800'],
    ['7', '5000000', '1900000000', '0000000000000007000000000000000000000000004c4b4000000000713fb300'],
    [
      '4294967295',
      '340282366920938463463374607431768211455',
      '18446744073709551615',
      '00000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
    ],
  ],
};

/**
 * Runs every vector through a loaded codec and returns what it produced, in the
 * shape of `expected()`. Self-contained, so it can run inside a page.
 */
export function compute(codec, table) {
  const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
  return {
    chainId: table.chainId.map(([network]) => [network, String(codec.chainId(network))]),
    contractAddress: table.contractAddress.map(([id]) => [id, hex(codec.contractAddress(id))]),
    destination: table.destination.map(([id]) => [id, hex(codec.destination(id))]),
    unixSeconds: table.unixSeconds.map(([ns]) => [ns, String(codec.unixSeconds(BigInt(ns)))]),
    noteFields: table.noteFields.map(([id, deposit, expiry]) => [
      id,
      deposit,
      expiry,
      hex(codec.noteFields(Number(id), BigInt(deposit), BigInt(expiry))),
    ]),
  };
}

export const expected = () => structuredClone(vectors);
