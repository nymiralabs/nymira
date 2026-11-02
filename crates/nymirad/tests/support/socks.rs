//! A SOCKS5 proxy for tests: user/password auth, CONNECT by name or address. It records what each
//! client asked for and either tunnels to a fixed local target or answers with a chosen error.

// Each test binary uses a part of this.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub user: String,
    pub password: String,
    pub host: String,
    pub port: u16,
    /// 1 = IPv4, 3 = domain name, 4 = IPv6.
    pub atyp: u8,
}

#[derive(Clone, Copy)]
pub enum Behavior {
    /// Tunnel every CONNECT to this address.
    Tunnel(SocketAddr),
    /// Answer every CONNECT with this SOCKS5 reply code.
    Refuse(u8),
    /// Tunnel each CONNECT to the address it names (IP literals only).
    Passthrough,
}

pub struct FakeSocks {
    pub addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeSocks {
    pub async fn start(behavior: Behavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let log = log.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, behavior, log).await;
                });
            }
        });
        Self { addr, seen }
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn serve(
    mut s: TcpStream,
    behavior: Behavior,
    log: Arc<Mutex<Vec<Seen>>>,
) -> std::io::Result<()> {
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    let mut methods = vec![0u8; head[1] as usize];
    s.read_exact(&mut methods).await?;
    if !methods.contains(&2) {
        return s.write_all(&[5, 0xff]).await;
    }
    s.write_all(&[5, 2]).await?;
    let mut ver = [0u8; 2];
    s.read_exact(&mut ver).await?;
    let mut user = vec![0u8; ver[1] as usize];
    s.read_exact(&mut user).await?;
    let mut plen = [0u8; 1];
    s.read_exact(&mut plen).await?;
    let mut password = vec![0u8; plen[0] as usize];
    s.read_exact(&mut password).await?;
    s.write_all(&[1, 0]).await?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await?;
            let mut name = vec![0u8; n[0] as usize];
            s.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        _ => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
            std::net::Ipv6Addr::from(a).to_string()
        }
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await?;
    log.lock().unwrap().push(Seen {
        user: String::from_utf8_lossy(&user).into_owned(),
        password: String::from_utf8_lossy(&password).into_owned(),
        host,
        port: u16::from_be_bytes(port),
        atyp: req[3],
    });
    match behavior {
        Behavior::Refuse(code) => s.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await,
        Behavior::Passthrough => {
            let (host, port) = {
                let l = log.lock().unwrap();
                let last = l.last().expect("just pushed");
                (last.host.clone(), last.port)
            };
            let mut up = TcpStream::connect((host.as_str(), port)).await?;
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            tokio::io::copy_bidirectional(&mut s, &mut up)
                .await
                .map(|_| ())
        }
        Behavior::Tunnel(target) => {
            let mut up = TcpStream::connect(target).await?;
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            tokio::io::copy_bidirectional(&mut s, &mut up)
                .await
                .map(|_| ())
        }
    }
}
