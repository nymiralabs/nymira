//! `nymirad`, the local gateway: owner token and request guard, vault pins, note secrets,
//! deposit, backup and status.
//!
//! Chat, leases, Tor management, close and exit belong to later tickets; the HTTP routes for
//! chat answer 501 until then.

pub mod agents;
pub mod agentsock;
pub mod attest;
pub mod backup;
pub mod calendar;
pub mod config;
pub mod deposit;
pub mod direct;
pub mod e2ee;
pub mod exits;
pub mod expiry;
#[cfg(feature = "mock")]
pub mod fake_oneclick;
pub mod gate;
pub mod gateway;
pub mod guard;
pub mod hardening;
pub mod intents;
pub mod lease;
pub mod mode;
pub mod models;
pub mod nearai;
pub mod net;
pub mod nras;
pub mod oneclick;
pub mod openai;
pub mod ownersock;
pub mod pins;
pub mod policy;
pub mod quotes;
pub mod ramdir;
#[cfg(feature = "mock")]
pub mod record;
pub mod records;
pub mod restore;
pub mod rpc;
pub mod secrets;
pub mod setsize;
pub mod sizing;
pub mod snapshot;
pub mod status;
pub mod strings;
pub mod submitter;
pub mod token;
pub mod tor;
pub mod walletvault;
pub mod webverify;
