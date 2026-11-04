//! Recorded 1Click answers through the same verifier used before a payment address is shown.
use serde_json::Value;

#[test]
fn recorded_keyless_quotes_verify_with_the_compiled_manager_key() {
    for text in [
        include_str!("fixtures/oneclick/dry.json"),
        include_str!("fixtures/oneclick/real.json"),
    ] {
        let response: Value = serde_json::from_str(text).unwrap();
        nymirad::oneclick::verify_signature(&response).unwrap();
        let mut changed = response;
        changed["quoteRequest"]["customRecipientMsg"] = "changed".into();
        assert!(nymirad::oneclick::verify_signature(&changed).is_err());
    }
}

#[test]
fn a_quote_is_bound_to_the_requested_amount_recipient_and_message() {
    let response: Value =
        serde_json::from_str(include_str!("fixtures/oneclick/real.json")).unwrap();
    let request = response["quoteRequest"].clone();
    nymirad::oneclick::validate_quote(&response, &request, 0).unwrap();
    let mut wrong = response.clone();
    wrong["quote"]["amountOut"] = "1".into();
    assert!(nymirad::oneclick::validate_quote(&wrong, &request, 0)
        .unwrap_err()
        .contains("amountOut"));
}

#[test]
fn a_quote_to_another_recipient_stops_payment() {
    let response: Value =
        serde_json::from_str(include_str!("fixtures/oneclick/real.json")).unwrap();
    let mut request = response["quoteRequest"].clone();
    request["recipient"] = "other.near".into();
    assert!(nymirad::oneclick::validate_quote(&response, &request, 0)
        .unwrap_err()
        .contains("recipient"));
}

#[test]
fn a_changed_message_stops_payment() {
    let response: Value =
        serde_json::from_str(include_str!("fixtures/oneclick/real.json")).unwrap();
    let mut request = response["quoteRequest"].clone();
    request["customRecipientMsg"] = "{ }".into();
    assert!(nymirad::oneclick::validate_quote(&response, &request, 0)
        .unwrap_err()
        .contains("customRecipientMsg"));
}

#[test]
fn an_expired_quote_offers_a_new_quote_without_payment() {
    let response: Value =
        serde_json::from_str(include_str!("fixtures/oneclick/real.json")).unwrap();
    assert!(nymirad::oneclick::validate_quote(
        &response,
        &response["quoteRequest"],
        u32::MAX as u64
    )
    .unwrap_err()
    .contains("new quote"));
}

#[test]
fn a_bad_signature_stops_payment() {
    let mut response: Value =
        serde_json::from_str(include_str!("fixtures/oneclick/real.json")).unwrap();
    response["signature"] = format!("ed25519:{}", "1".repeat(64)).into();
    assert!(nymirad::oneclick::verify_signature(&response)
        .unwrap_err()
        .contains("signature"));
}
