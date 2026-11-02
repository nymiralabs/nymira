//! The tree is rebuilt from `get_leaves` pages of at most 100, so a vault with more than 100
//! leaves needs several reads. A scripted node serves 250 leaves and refuses a page above 100.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use nymirad::rpc::{Rpc, Transport};
use serde_json::{json, Value};
use zkapi_client::tree::SparseTree;
use zkapi_types::Felt252;

struct Node {
    leaves: Vec<Felt252>,
    root: String,
    /// `(from, limit)` of every `get_leaves` call.
    pages: Vec<(u64, u64)>,
}

fn bytes_of(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

async fn rpc(State(node): State<Arc<Mutex<Node>>>, Json(req): Json<Value>) -> Json<Value> {
    let mut node = node.lock().unwrap();
    let params = &req["params"];
    let result = match req["method"].as_str().unwrap() {
        "block" => json!({ "header": { "hash": "BLOCKHASH", "height": 7 } }),
        "query" => {
            let args: Value = serde_json::from_slice(
                &base64::engine::general_purpose::STANDARD
                    .decode(params["args_base64"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
            let answer = match params["method_name"].as_str().unwrap() {
                "config" => json!({}),
                "current_root" => json!(node.root),
                "leaf_count" => json!(node.leaves.len()),
                "get_leaves" => {
                    let (from, limit) = (
                        args["from"].as_u64().unwrap(),
                        args["limit"].as_u64().unwrap(),
                    );
                    node.pages.push((from, limit));
                    // The vault refuses a page above its cap.
                    assert!(limit <= 100, "page of {limit}");
                    let page: Vec<String> = node
                        .leaves
                        .iter()
                        .skip(from as usize)
                        .take(limit as usize)
                        .map(|l| format!("0x{}", hex::encode(l.0)))
                        .collect();
                    json!(page)
                }
                other => panic!("unexpected view {other}"),
            };
            json!({ "result": bytes_of(&answer) })
        }
        other => panic!("unexpected method {other}"),
    };
    Json(json!({ "result": result }))
}

#[tokio::test]
async fn a_tree_of_250_leaves_is_read_in_three_pages_and_matches_the_root() {
    let leaves: Vec<Felt252> = (1..=250u64).map(Felt252::from_u64).collect();
    let root = SparseTree::from_leaves(&leaves).root();
    let node = Arc::new(Mutex::new(Node {
        leaves: leaves.clone(),
        root: format!("0x{}", hex::encode(root.0)),
        pages: vec![],
    }));
    let app = Router::new().route("/", post(rpc)).with_state(node.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let rpc = Rpc::new(url, &Transport::Direct).unwrap();
    let snap = nymirad::snapshot::take(&rpc, "vault.test").await.unwrap();
    assert_eq!((snap.leaf_count, snap.tree.root()), (250, root));
    assert_eq!(
        node.lock().unwrap().pages,
        [(0, 100), (100, 100), (200, 100)]
    );
    // The authentication path of a leaf past the first page is the real one.
    assert_eq!(snap.tree.leaf(249), Felt252::from_u64(250));
}

#[tokio::test]
async fn a_leaf_the_node_leaves_out_of_a_later_page_does_not_build_the_root() {
    let leaves: Vec<Felt252> = (1..=150u64).map(Felt252::from_u64).collect();
    let root = SparseTree::from_leaves(&leaves).root();
    // The node serves a changed leaf in the second page.
    let mut served = leaves.clone();
    served[120] = Felt252::from_u64(9_999);
    let node = Arc::new(Mutex::new(Node {
        leaves: served,
        root: format!("0x{}", hex::encode(root.0)),
        pages: vec![],
    }));
    let app = Router::new().route("/", post(rpc)).with_state(node);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let rpc = Rpc::new(url, &Transport::Direct).unwrap();
    assert!(matches!(
        nymirad::snapshot::take(&rpc, "vault.test").await,
        Err(nymirad::snapshot::SnapshotError::RootMismatch(_))
    ));
}
