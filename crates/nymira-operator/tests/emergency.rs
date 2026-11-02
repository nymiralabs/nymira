//! The key-leak runbook's code half: new leases and clearances stop, `/v1/status` says why, the
//! stop survives a restart, and a leaked provider credential can be cut off.

mod common;

use common::*;
use nymira_operator::store::{self, Store};

const NOTICE: &str = "A signing key may have leaked. Exit your notes; a new vault follows.";

#[tokio::test(flavor = "multi_thread")]
async fn an_emergency_stops_leases_and_clearances_and_shows_on_status() {
    let rig = Rig::new(Setup::default()).await;
    assert!(rig.status().await["emergency_notice"].is_null());
    let (s, open, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201, "{open}");

    rig.op.set_emergency(NOTICE).await.unwrap();
    assert_eq!(rig.status().await["emergency_notice"], NOTICE);

    let (s, v, _) = rig.lease(&rig.fresh(2)).await;
    json_error(s, &v, 503, "emergency");
    assert_eq!(v["error"]["message"], NOTICE);
    let (s, v) = rig.clearance(&rig.ownership(3)).await;
    json_error(s, &v, 503, "emergency");

    // The lease that was open keeps working: users are told to exit, not cut off.
    let (s, _, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(
        s, 503,
        "a repeat of an opened lease is also a new admission"
    );

    rig.op.clear_emergency().await.unwrap();
    assert!(rig.status().await["emergency_notice"].is_null());
    let (s, _, _) = rig.lease(&rig.fresh(4)).await;
    assert_eq!(s, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stop_is_in_the_database_so_a_restart_and_another_process_see_it() {
    let rig = Rig::new(Setup::default()).await;
    // A second handle on the file, as the `emergency` command of another process is.
    let other = Store::open(&rig.dir.path().join("operator.db")).unwrap();
    other
        .run(|c| store::set_emergency(c, NOTICE, 1))
        .await
        .unwrap();
    assert_eq!(rig.status().await["emergency_notice"], NOTICE);
    let (s, v, _) = rig.lease(&rig.fresh(1)).await;
    json_error(s, &v, 503, "emergency");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leaked_provider_credential_is_cut_off_by_revoking_every_key() {
    let rig = Rig::new(Setup::default()).await;
    for n in 1..=3 {
        let (s, _, _) = rig.lease(&rig.fresh(n)).await;
        assert_eq!(s, 201);
    }
    rig.provider.orphan_key("stray", 1_000);
    assert_eq!(rig.provider.live_keys(), 4);
    assert_eq!(rig.op.revoke_provider_keys().await.unwrap(), 4);
    assert_eq!(rig.provider.live_keys(), 0);
}
