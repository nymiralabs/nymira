//! `nymirad status`: operator status, note balances and expiries, the tripwire and quotes.

use serde_json::Value;

use crate::agents::AgentEntry;
use crate::pins::Tripwire;
use crate::quotes::{self, View};
use crate::rpc::{client, send_error, RpcError, Transport};
use crate::secrets::{NoteEntry, NoteStatus};
use crate::setsize::SetSize;
use crate::strings;

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
    match &r.set_size {
        Some(size) => {
            line.push_str(&format!("\n  {}", size.headline()));
            for d in size.differences() {
                line.push_str(&format!("\n  note: {d}"));
            }
        }
        None => line.push_str(&format!(
            "\n  {}",
            strings::fill(
                "unavailable",
                &[("why", "not recorded for this lease".into())]
            )
        )),
    }
    for w in &r.warnings {
        line.push_str(&format!("\n  warning: {w}"));
    }
    line
}

/// The "now" figure of `nymirad status`: the same rule at the current final block.
pub type NowFigure = Result<SetSize, String>;

fn now_lines(now: &NowFigure) -> String {
    match now {
        Ok(size) => {
            let mut out = format!("\n  {}: {}", strings::get("now_label"), size.headline());
            for d in size.differences() {
                out.push_str(&format!("\n  note: {d}"));
            }
            out
        }
        Err(why) => format!(
            "\n  {}: {}",
            strings::get("now_label"),
            strings::fill("unavailable", &[("why", why.clone())])
        ),
    }
}

/// The Tor line and what Tor does not cover, then what the set size does not say. Printed after
/// the leases, so it reads as their footnote. `tor` is whether `nymirad` traffic goes through
/// Tor; `figures` whether any set size was shown above.
pub fn privacy_lines(tor: bool, testnet: bool, figures: bool) -> String {
    let mut out = vec![
        strings::get(if tor { "tor_on" } else { "tor_off" }).to_string(),
        strings::get("tor_limits").to_string(),
    ];
    if figures {
        for key in ["balances_private", "includes_unlisted", "five_not_safe"] {
            out.push(strings::get(key).to_string());
        }
        if testnet {
            out.push(strings::get("testnet_notes").to_string());
        }
    }
    out.join("\n")
}

/// The text `nymirad status` prints. Nothing in it comes from a secret.
pub fn render(
    tor_line: &str,
    operator: Result<&Value, &RpcError>,
    notes: &[NoteEntry],
    tripwire: &Tripwire,
    quotes: &View,
    now_secs: u64,
    now_figure: Option<&NowFigure>,
) -> String {
    render_with(
        tor_line,
        operator,
        notes,
        &[],
        tripwire,
        quotes,
        now_secs,
        now_figure,
    )
}

/// The first line of `line` gets `mark` at its end.
fn mark_first_line(line: String, mark: &str) -> String {
    match line.split_once('\n') {
        Some((first, rest)) => format!("{first}{mark}\n{rest}"),
        None => format!("{line}{mark}"),
    }
}

/// `render`, with the agents' budget notes marked on their note lines.
#[allow(clippy::too_many_arguments)]
pub fn render_with(
    tor_line: &str,
    operator: Result<&Value, &RpcError>,
    notes: &[NoteEntry],
    agents: &[AgentEntry],
    tripwire: &Tripwire,
    quotes: &View,
    now_secs: u64,
    now_figure: Option<&NowFigure>,
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
                if let Some(now) = now_figure {
                    line.push_str(&now_lines(now));
                }
                line
            }
            (NoteStatus::Pending, ..) => format!("Note {}: pending, run recover", n.commitment),
            // A note with an ID is named by it: that is what `finalize --note` takes.
            _ => format!(
                "Note {}: {:?}",
                n.note_id
                    .map_or_else(|| n.commitment.clone(), |id| id.to_string()),
                n.status
            ),
        };
        let line = match agents.iter().find(|a| a.note == n.commitment) {
            Some(a) => mark_first_line(
                line,
                &format!(
                    " [budget note of agent {}: token {}, policy {}]",
                    a.name,
                    if a.disabled { "disabled" } else { "active" },
                    a.policy_version
                        .map_or("missing".to_string(), |v| format!("v{v}"))
                ),
            ),
            None => line,
        };
        out.push(line);
    }
    match quotes {
        View::Off(why) => out.push(format!("Quotes: off ({why})")),
        View::Lines(lines) => {
            for l in lines {
                out.push(format!(
                    "Quote {}: {} source tokens ({}). Fund with testnet USDC from the Circle faucet.",
                    l.label,
                    l.text,
                    quotes::LABEL
                ));
            }
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_900_000_000;
    const DAY: u64 = 86_400;
    const SECRET: &str = "00aabbccddeeff00112233445566778899aabbccddeeff00112233445566778";

    fn note(id: u32, status: NoteStatus, expiry: Option<u64>) -> NoteEntry {
        NoteEntry {
            chain_label: "near-testnet".into(),
            secret: SECRET.into(),
            commitment: format!("0xc0ffee{id}"),
            denomination: "1000000".into(),
            payout: "alice.test".into(),
            status,
            note_id: Some(id),
            expiry_ns: expiry,
            deposit_amount: Some("1250000".into()),
            tx_hash: None,
            created_at: NOW,
        }
    }

    fn pending_without_id() -> NoteEntry {
        let mut n = note(4, NoteStatus::Pending, None);
        n.note_id = None;
        n
    }

    fn render(operator: Result<&Value, &RpcError>, notes: &[NoteEntry], q: &View) -> String {
        super::render(
            "Tor: ready",
            operator,
            notes,
            &Tripwire::default(),
            q,
            NOW,
            None,
        )
    }

    #[test]
    fn notes_show_their_balance_expiry_and_the_warnings_that_follow_from_it() {
        let notes = [
            note(1, NoteStatus::Accepted, Some(NOW + 30 * DAY)),
            note(2, NoteStatus::Accepted, Some(NOW + 10 * DAY)),
            note(3, NoteStatus::Accepted, Some(NOW + 4 * DAY)),
            pending_without_id(),
            note(5, NoteStatus::Exiting, Some(NOW + 4 * DAY)),
        ];
        let ok = json!({"vault": "vault.test", "degraded": false, "recovery_mode": false});
        let text = render(Ok(&ok), &notes, &View::Lines(vec![]));
        assert!(text.starts_with("Tor: ready\n"), "{text}");
        assert!(text.contains(
            "Operator: vault.test (heartbeat none yet, degraded: false, recovery mode: false)"
        ));
        assert!(text.contains("Vault: matches the pins"));
        let line = |id: u32| {
            let at = text
                .find(&format!("Note {id}:"))
                .unwrap_or_else(|| panic!("{text}"));
            let rest = &text[at..];
            rest.split("\nNote ").next().unwrap().to_string()
        };
        let one = line(1);
        assert!(one.contains("1.250000 USDC deposited"), "{one}");
        assert!(one.contains(&format!(
            "expires {}",
            crate::calendar::stamp(NOW + 30 * DAY)
        )));
        assert!(one.contains("(30 days left)"));
        assert!(!one.contains("warning") && !one.contains("notice"), "{one}");
        let two = line(2);
        assert!(two.contains("(10 days left)") && two.contains("notice: 14 days or less left"));
        let three = line(3);
        assert!(three.contains("warning: no new lease on this note (5 days or less left)"));
        assert!(
            text.contains("Note 0xc0ffee4: pending, run recover"),
            "{text}"
        );
        assert!(line(5).contains("Exiting"), "{}", line(5));
        // Nothing in the status comes from a secret.
        assert!(!text.contains(SECRET));
    }

    #[test]
    fn a_budget_note_is_marked_with_its_agent_and_the_others_are_not() {
        use crate::agents::{AgentEntry, Transport};
        let notes = [
            note(1, NoteStatus::Accepted, Some(NOW + 30 * DAY)),
            note(3, NoteStatus::Accepted, Some(NOW + 4 * DAY)),
        ];
        let agent = |policy| AgentEntry {
            name: "cursor".into(),
            token_hash: "00".into(),
            note: "0xc0ffee3".into(),
            owner_account: "alice.test".into(),
            policy_version: policy,
            policy_seen: 0,
            transport: Transport::Tcp,
            uid: None,
            disabled: false,
            created_at: NOW,
        };
        let text = |a: &AgentEntry| {
            super::render_with(
                "Tor: ready",
                Ok(&json!({"vault": "v"})),
                &notes,
                std::slice::from_ref(a),
                &Tripwire::default(),
                &View::Lines(vec![]),
                NOW,
                None,
            )
        };
        let text_none = text(&agent(None));
        let one = text_none
            .lines()
            .find(|l| l.starts_with("Note 1:"))
            .unwrap();
        assert!(!one.contains("budget note"), "{one}");
        let three = text_none
            .lines()
            .find(|l| l.starts_with("Note 3:"))
            .unwrap();
        assert!(
            three.contains("[budget note of agent cursor: token active, policy missing]"),
            "{three}"
        );
        // The mark sits on the first line; the note's own warning stays below it.
        assert!(
            text_none.contains("policy missing]\n  warning: no new lease"),
            "{text_none}"
        );
        let mut disabled = agent(Some(2));
        disabled.disabled = true;
        assert!(text(&disabled).contains("token disabled, policy v2]"));
    }

    #[test]
    fn no_notes_and_no_operator_are_said_plainly() {
        let down = RpcError::Transport("refused".into());
        let text = render(Err(&down), &[], &View::Off("not a testnet build"));
        assert!(text.contains("Notes: none"));
        assert!(text.contains("Operator: unreachable ("), "{text}");
        assert!(text.contains("Quotes: off (not a testnet build)"));
    }

    #[test]
    fn a_quote_is_labelled_quote_only() {
        let lines = View::Lines(vec![quotes::Line {
            label: "ETH",
            text: "0.0003 ETH".into(),
            at: NOW,
        }]);
        let text = render(Ok(&json!({})), &[], &lines);
        assert!(
            text.contains("Quote ETH: 0.0003 ETH source tokens (quote only)"),
            "{text}"
        );
    }

    #[test]
    fn an_operator_emergency_tells_the_user_how_to_exit() {
        let emergency = json!({"vault": "v", "emergency_notice": "the operator is stopping"});
        let text = render(Ok(&emergency), &[], &View::Lines(vec![]));
        assert!(
            text.contains("Operator notice: the operator is stopping"),
            "{text}"
        );
        assert!(text.contains("New leases are stopped."));
        assert!(text.contains("run `nymirad exit --note ID` for each open note"));
        // Without a notice the advice is not shown.
        let calm = render(Ok(&json!({"vault": "v"})), &[], &View::Lines(vec![]));
        assert!(!calm.contains("nymirad exit"), "{calm}");
    }
}
