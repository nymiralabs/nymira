//! The Intents deposit message parser takes any bytes without panicking.
//! Run with `cargo +nightly fuzz run intents_msg` from `contracts/vault`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        nymira_vault::fuzzing::intents_msg(text);
    }
});
