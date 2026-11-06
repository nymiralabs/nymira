//! The cases in `shared/set-size-cases.json`, which the web panel's tests run as well: the same
//! provider sets and team lists must print the same set size and the same sentences on both sides.

use std::collections::BTreeSet;

use nymirad::setsize::{evaluate, Raw};
use serde_json::Value;

const CASES: &str = include_str!("../../../shared/set-size-cases.json");

fn ids(v: &Value) -> Vec<u32> {
    v.as_array()
        .expect("a list of IDs")
        .iter()
        .map(|n| n.as_u64().expect("an ID") as u32)
        .collect()
}

#[test]
fn nymirad_prints_what_the_shared_cases_say() {
    let cases: Vec<Value> = serde_json::from_str(CASES).expect("cases parse");
    assert!(cases.len() >= 7);
    for case in cases {
        let raw = Raw {
            sets: case["sets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| ids(s).into_iter().collect::<BTreeSet<u32>>())
                .collect(),
            one_provider: case["one_provider"].as_str().map(str::to_string),
        };
        let operator = case["operator"].as_array().map(|_| ids(&case["operator"]));
        let size = evaluate(&raw, &ids(&case["pinned"]), operator.as_deref());
        let name = case["name"].as_str().unwrap();
        assert_eq!(
            size.headline(),
            case["headline"].as_str().unwrap(),
            "{name}"
        );
        let want: Vec<&str> = case["differences"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d.as_str().unwrap())
            .collect();
        assert_eq!(size.differences(), want, "{name}");
    }
}
