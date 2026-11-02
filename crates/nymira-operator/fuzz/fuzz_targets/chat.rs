#![no_main]
use libfuzzer_sys::fuzz_target;
use nymira_operator::wire::Wire;

fuzz_target!(|data: &[u8]| {
    nymira_operator::wire::exercise(Wire::Chat, data);
    assert!(nymira_operator::wire::chat_agrees(data), "the parser and the reference reader disagree");
});
