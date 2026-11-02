//! `nymirad`, the local gateway: owner token and request guard, vault pins, note secrets,
//! deposit, backup and status.
//!
//! Chat, leases, Tor management, close and exit belong to later tickets; the HTTP routes for
//! chat answer 501 until then.

pub mod backup;
pub mod calendar;
pub mod config;
pub mod deposit;
pub mod gate;
pub mod guard;
pub mod hardening;
pub mod mode;
pub mod openai;
pub mod pins;
pub mod quotes;
pub mod rpc;
pub mod secrets;
pub mod sizing;
pub mod snapshot;
pub mod status;
pub mod token;
