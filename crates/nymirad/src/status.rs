//! `nymirad status`: operator status, note balances and expiries, the tripwire and quotes.

use serde_json::Value;

use crate::pins::Tripwire;
use crate::quotes::{self, View};
use crate::rpc::{client, send_error, RpcError, Transport};
use crate::secrets::{NoteEntry, NoteStatus};

/// The operator's `/v1/status` document. Fields this build does not know are ignored.
pub async fn operator_status(base: &str, transport: &Transport) -> Result<Value, RpcError> {
    let reply = client(transport)?
        .get(format!("{}/v1/status", base.trim_end_matches('/')))
        .send()
        .await
        .map_err(send_error)?;
    if !transport.is_direct() && crate::net::refuses_tor(reply.status().as_u16()) {
        return Err(RpcError::TorRefused(format!("status {}", reply.status())));
    }
    reply
        .json()
        .await
        .map_err(|e| RpcError::Shape(e.without_url().to_string()))
}

fn usdc(base_units: &str) -> String {
    let n: u128 = base_units.parse().unwrap_or(0);
    format!("{}.{:06} USDC", n / 1_000_000, n % 1_000_000)
}

/// One line per lease record: metadata only.
pub fn lease_line(r: &crate::records::LeaseRecord) -> String {
    let trust = r.trust.as_ref().map_or("open", |t| t.label());
    let charge = r.charge.as_deref().unwrap_or("-");
    let mut line = format!(
        "Lease {} on note {}: {} requests, {} model, charge {charge}, {trust}",
        r.lease_id, r.note_id, r.requests, r.model
    );
    for w in &r.warnings {
        line.push_str(&format!("\n  warning: {w}"));
    }
    line
}

/// The text `nymirad status` prints. Nothing in it comes from a secret.
pub fn render(
    tor_line: &str,
    operator: Result<&Value, &RpcError>,
    notes: &[NoteEntry],
    tripwire: &Tripwire,
    quotes: &View,
    now_secs: u64,
) -> String {
    let mut out = Vec::new();
    out.push(tor_line.to_string());
    match operator {
        Ok(s) => {
            let heartbeat = s["heartbeat_age_secs"]
                .as_u64()
                .map_or("none yet".to_string(), |a| format!("{a} s ago"));
            out.push(format!(
                "Operator: {} (heartbeat {heartbeat}, degraded: {}, recovery mode: {})",
                s["vault"].as_str().unwrap_or("?"),
                s["degraded"],
                s["recovery_mode"]
            ));
            if let Some(notice) = s["emergency_notice"].as_str() {
                out.push(format!(
                    "Operator notice: {notice}\n  New leases are stopped. To get your money out, run `nymirad exit --note ID` for each open note."
                ));
            }
        }
        Err(e) => out.push(format!("Operator: unreachable ({e})")),
    }
    match tripwire.tripped() {
        None => out.push("Vault: matches the pins".into()),
        Some(d) => out.push(format!(
            "Vault: STOPPED, differs from the pins: {}",
            d.join(", ")
        )),
    }
    if notes.is_empty() {
        out.push("Notes: none".into());
    }
    for n in notes {
        let line = match (n.status, n.note_id, n.expiry_ns) {
            (NoteStatus::Accepted, Some(id), Some(exp)) => {
                let mut line = format!(
                    "Note {id}: {} deposited, expires {} ({} days left)",
                    usdc(n.deposit_amount.as_deref().unwrap_or("0")),
                    crate::calendar::stamp(exp),
                    exp.saturating_sub(now_secs) / 86_400
                );
                if crate::expiry::lease_blocked(exp, now_secs) {
                    line.push_str("\n  warning: no new lease on this note (5 days or less left); close it or exit before it expires");
                } else if let Some(level) = crate::expiry::notice_level(exp, now_secs) {
                    line.push_str(&format!("\n  notice: {level} days or less left"));
                }
                line
            }
            (NoteStatus::Pending, ..) => format!("Note {}: pending, run recover", n.commitment),
            _ => format!("Note {}: {:?}", n.commitment, n.status),
        };
        out.push(line);
    }
    match quotes {
        View::Off(why) => out.push(format!("Quotes: off ({why})")),
        View::Lines(lines) => {
            for l in lines {
                out.push(format!(
                    "Quote {}: 1 USDC buys {} ({})",
                    l.label,
                    l.text,
                    quotes::LABEL
                ));
            }
        }
    }
    out.join("\n")
}
