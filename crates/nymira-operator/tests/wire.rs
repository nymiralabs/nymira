//! Every wire type, over a seeded corpus: valid bodies, their mutations and raw noise. This is
//! what runs on stable; the same entry point is the body of each `cargo-fuzz` target.

mod common;

use common::*;
use nymira_operator::wire::{chat_agrees, exercise, Wire};
use rand::{Rng, SeedableRng};
use serde_json::json;

fn seeds() -> Vec<(Wire, Vec<u8>)> {
    let lease = body(real_request());
    let chat = serde_json::to_vec(&json!({
        "model": "z-ai/glm-5.3-flash",
        "messages": [{"role": "user", "content": "ab".repeat(80)}],
        "max_tokens": 10, "stream": true,
    }))
    .unwrap();
    let reconcile = serde_json::to_vec(&json!({
        "lease_id": "l1",
        "request": serde_json::from_slice::<serde_json::Value>(&lease).unwrap(),
    }))
    .unwrap();
    let close = serde_json::to_vec(&json!({"proof": "AA==", "inputs": vec!["AA=="; 16]})).unwrap();
    vec![
        (Wire::Lease, lease.clone()),
        (Wire::Clearance, lease),
        (Wire::Close, close),
        (Wire::Chat, chat.clone()),
        (Wire::Reconcile, reconcile),
        (
            Wire::SignerCall,
            br#"{"op":"clearance","nullifier":"0x01"}"#.to_vec(),
        ),
        (
            Wire::ReplicaCall,
            br#"{"op":"record","kind":"signed","key":"l1","payload":null}"#.to_vec(),
        ),
        (
            Wire::ReplogEntry,
            br#"{"seq":1,"prev":"00","hash":"00","kind":"x","key":"k","hour":0,"payload":null}"#
                .to_vec(),
        ),
        (Wire::Json, chat),
    ]
}

fn mutate(rng: &mut rand::rngs::StdRng, seed: &[u8]) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..rng.gen_range(1..6) {
        if b.is_empty() {
            b.push(rng.gen());
            continue;
        }
        let at = rng.gen_range(0..b.len());
        match rng.gen_range(0..7) {
            0 => b[at] = rng.gen(),
            1 => b[at] ^= 1 << rng.gen_range(0..8),
            2 => b.truncate(at),
            3 => b.insert(at, rng.gen()),
            4 => {
                let end = (at + rng.gen_range(1..32)).min(b.len());
                let piece = b[at..end].to_vec();
                b.splice(at..at, piece);
            }
            5 => {
                b.remove(at);
            }
            _ => {
                b.extend_from_slice(b"{\"a\":{\"a\":{\"a\":{\"a\":[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[")
            }
        }
    }
    b
}

#[test]
fn no_wire_type_panics_on_seeds_mutations_or_noise() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(29);
    let seeds = seeds();
    // The valid seeds are valid for the parsers that see their own kind.
    assert!(exercise(Wire::Lease, &seeds[0].1));
    assert!(exercise(Wire::Chat, &seeds[3].1));
    assert!(exercise(Wire::SignerCall, &seeds[5].1));
    assert!(exercise(Wire::ReplicaCall, &seeds[6].1));
    for (wire, seed) in &seeds {
        for _ in 0..1500 {
            let input = mutate(&mut rng, seed);
            let outcome = std::panic::catch_unwind(|| exercise(*wire, &input));
            assert!(
                outcome.is_ok(),
                "{wire:?} panicked on {:?}",
                String::from_utf8_lossy(&input)
            );
        }
    }
    for wire in Wire::ALL {
        for _ in 0..500 {
            let len = rng.gen_range(0..300);
            let noise: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
            let outcome = std::panic::catch_unwind(|| exercise(wire, &noise));
            assert!(outcome.is_ok(), "{wire:?} panicked on noise");
        }
        for input in [
            &b""[..],
            b"null",
            b"[]",
            b"{}",
            b"\xff\xfe",
            &[b'['; 5000][..],
        ] {
            assert!(
                std::panic::catch_unwind(|| exercise(wire, input)).is_ok(),
                "{wire:?}"
            );
        }
    }
}

#[test]
fn the_chat_parser_and_the_reference_reader_agree_on_mutations() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(30);
    let chat = &seeds()[3].1;
    for _ in 0..3000 {
        let input = mutate(&mut rng, chat);
        assert!(chat_agrees(&input), "{:?}", String::from_utf8_lossy(&input));
    }
}

#[test]
fn every_wire_type_has_a_fuzz_target() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/fuzz_targets");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            e.unwrap()
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names.len(), Wire::ALL.len());
    for wire in Wire::ALL {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fuzz/fuzz_targets")
                .join(format!(
                    "{}.rs",
                    names
                        .iter()
                        .find(|n| {
                            std::fs::read_to_string(
                                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                                    .join("fuzz/fuzz_targets")
                                    .join(format!("{n}.rs")),
                            )
                            .unwrap()
                            .contains(&format!("Wire::{wire:?},"))
                        })
                        .unwrap_or_else(|| panic!("no fuzz target for {wire:?}"))
                )),
        )
        .unwrap();
        assert!(source.contains("fuzz_target!"));
    }
}
