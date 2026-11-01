//! Probe: what does the host's alt_bn128 enforce?
use ark_bn254::{Fq, Fq2, G1Affine, G2Affine};
use ark_ec::{AffineRepr, CurveGroup};
use ark_ff::{BigInteger, PrimeField, Zero};
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{env, testing_env};

fn le<F: PrimeField>(f: &F) -> Vec<u8> {
    let mut v = f.into_bigint().to_bytes_le();
    v.resize(32, 0);
    v
}
fn g1(p: &G1Affine) -> Vec<u8> {
    [le(&p.x), le(&p.y)].concat()
}
fn g2(p: &G2Affine) -> Vec<u8> {
    [le(&p.x.c0), le(&p.x.c1), le(&p.y.c0), le(&p.y.c1)].concat()
}

fn run(name: &str, f: impl FnOnce() -> bool + std::panic::UnwindSafe) {
    let r = std::panic::catch_unwind(f);
    println!("PROBE {name}: {:?}", r.map_err(|_| "panic"));
}

#[test]
fn probe() {
    testing_env!(VMContextBuilder::new().build());
    let g1gen = G1Affine::generator();
    let g2gen = G2Affine::generator();
    // Twist point outside the order-r subgroup.
    let mut bad = None;
    for i in 1u64..200 {
        let x = Fq2::new(Fq::from(i), Fq::from(1u64));
        if let Some(p) = G2Affine::get_point_from_x_unchecked(x, false) {
            if p.is_on_curve() && !p.is_in_correct_subgroup_assuming_on_curve() {
                bad = Some(p);
                break;
            }
        }
    }
    let bad = bad.expect("found twist point outside subgroup");
    // e(P, Q) * e(-P, Q) = 1 with a good Q passes; use a bad Q in a pair with itself negated.
    let neg_g1 = (-g1gen.into_group()).into_affine();
    run("good pair", || {
        env::alt_bn128_pairing_check([g1(&g1gen), g2(&g2gen), g1(&neg_g1), g2(&g2gen)].concat())
    });
    run("subgroup-bad G2", || {
        env::alt_bn128_pairing_check([g1(&g1gen), g2(&bad), g1(&neg_g1), g2(&bad)].concat())
    });
    run("subgroup-bad G2 alone single pair", || {
        env::alt_bn128_pairing_check([g1(&g1gen), g2(&bad)].concat())
    });
    // Infinity encodings.
    run("G1 infinity in pairing", || {
        env::alt_bn128_pairing_check([vec![0u8; 64], g2(&g2gen)].concat())
    });
    run("G2 infinity in pairing", || {
        env::alt_bn128_pairing_check([g1(&g1gen), vec![0u8; 128]].concat())
    });
    // Off-curve G1.
    let mut off = g1(&g1gen);
    off[0] ^= 1;
    run("G1 off curve in pairing", || {
        env::alt_bn128_pairing_check([off.clone(), g2(&g2gen)].concat())
    });
    run("G1 off curve in sum", || {
        let mut v = vec![0u8];
        v.extend_from_slice(&off);
        env::alt_bn128_g1_sum(&v).len() > 0
    });
    // Coordinate >= p (add p to x).
    let p_le = Fq::MODULUS.to_bytes_le();
    let mut big = g1(&g1gen);
    let mut carry = 0u16;
    for i in 0..32 {
        let s = big[i] as u16 + p_le[i] as u16 + carry;
        big[i] = s as u8;
        carry = s >> 8;
    }
    run("G1 x >= p in pairing", || {
        env::alt_bn128_pairing_check([big.clone(), g2(&g2gen)].concat())
    });
    run("G1 x >= p in sum", || {
        let mut v = vec![0u8];
        v.extend_from_slice(&big);
        env::alt_bn128_g1_sum(&v).len() > 0
    });
    run("G1 infinity in sum", || {
        let mut v = vec![0u8];
        v.extend_from_slice(&[0u8; 64]);
        env::alt_bn128_g1_sum(&v).len() > 0
    });
    let _ = Zero::is_zero(&Fq::from(0u64));
}
