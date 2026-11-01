//! The verifying keys this vault accepts, as keccak256 hashes of their canonical bytes.

/// keccak256 of the request and withdrawal verifying keys (the canonical arkworks
/// serialization zkapi's own pin check hashes).
pub struct VkPins {
    pub request: [u8; 32],
    pub withdrawal: [u8; 32],
}

/// Development keys: zkapi's `v2-concession-dev` setup, `vk_keccak256` of its manifest. Sandbox
/// builds and unit tests. The `rehearsal` keys replace them here once the rehearsal exists; no
/// build with `dev-setup` is ever a mainnet or judge build (see `lib.rs`).
#[cfg(any(feature = "dev-setup", test))]
pub const PINS: VkPins = VkPins {
    request: [
        0x38, 0x34, 0x8b, 0x67, 0x0f, 0xd2, 0x1c, 0x8c, 0x62, 0x20, 0x75, 0xf8, 0x4f, 0x5c, 0x4a,
        0x24, 0x6f, 0x54, 0x08, 0x1e, 0x09, 0x88, 0x6b, 0x0a, 0x3c, 0x86, 0xc8, 0x9e, 0xdc, 0x01,
        0xe1, 0xc8,
    ],
    withdrawal: [
        0xc1, 0x48, 0x44, 0xd3, 0xaf, 0x2b, 0x29, 0x30, 0x38, 0x52, 0xe7, 0xe4, 0x7e, 0x64, 0x28,
        0x30, 0x55, 0x2e, 0x0c, 0x73, 0xdf, 0x3e, 0xbb, 0xed, 0x8f, 0x0d, 0x27, 0xc3, 0x16, 0xbe,
        0x9f, 0xfa,
    ],
};

/// The ceremony (or fallback) keys. Placeholder until the ceremony publishes its hashes: no
/// verifying key hashes to zero, so `new()` accepts nothing and a release build cannot start.
#[cfg(not(any(feature = "dev-setup", test)))]
pub const PINS: VkPins = VkPins {
    request: [0; 32],
    withdrawal: [0; 32],
};
