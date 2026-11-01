//! `nymira-fake-near-ai [--listen ADDR]`. Also reads `NYMIRA_FAKE_NEAR_AI_LISTEN`.
//! Default `127.0.0.1:8790`. Prints the bound address and runs until killed.

use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let mut listen =
        std::env::var("NYMIRA_FAKE_NEAR_AI_LISTEN").unwrap_or_else(|_| "127.0.0.1:8790".into());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let addr: SocketAddr = listen.parse().unwrap_or_else(|e| {
        eprintln!("bad listen address {listen:?}: {e}");
        std::process::exit(2);
    });
    let running = nymira_fake_near_ai::start(addr).await.unwrap_or_else(|e| {
        eprintln!("cannot bind {addr}: {e}");
        std::process::exit(1);
    });
    println!("nymira-fake-near-ai listening on {}", running.url());
    std::future::pending::<()>().await;
}

fn usage() -> ! {
    eprintln!("usage: nymira-fake-near-ai [--listen ADDR]");
    std::process::exit(2);
}
