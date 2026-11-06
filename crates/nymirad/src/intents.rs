//! Direct Intents funding: local pending notes, pinned vault checks and receipt acceptance.
use crate::{
    backup,
    deposit::Accepted,
    mode::Mode,
    oneclick::{validate_quote, validate_two_step, FundingClient},
    pins::{Pins, Tripwire},
    rpc::{complete_receipts, Rpc},
    secrets::{new_secret, NoteEntry, NoteStatus, NoteStore},
};
use nymira_operator::clock::Clock;
use nymira_operator::tx::{NearTx, TxRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};

pub const DESTINATION: &str =
    "nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1";
pub const PLACEHOLDER: &str = "nymira.near";
pub const DUMMY: &str = "0x1111111111111111111111111111111111111111";
pub const ROUTE_FALLBACK: &str = "Route screening can delay or block delivery. For a future deposit, fund with NEAR USDC directly; do not pay twice while this attempt is unresolved.";
pub const PRIVACY: &str = "On this path 1Click and the Intents explorer pair your source wallet with this deposit. The proofs are designed to keep the vault and the operator from tying your spending to this deposit, but while few notes are open, and without Tor, your spending can still be matched to it by time and network address. Wait a while between depositing and your first chat.";
/// The two-step path adds this to [`PRIVACY`].
pub const PRIVACY_TWO_STEP: &str = "Your NEAR account appears on chain next to the deposit.";
pub const TWO_STEP_NOTICE: &str = "Two steps: 1Click first pays NEAR USDC to your own NEAR account, a plain transfer that is refunded if it fails. Then nymirad sends that USDC from your account to the vault. The second step needs a little NEAR for gas on your account and an account registered for USDC.";
/// yoctoNEAR (0.02 NEAR) the account must hold before the two-step path is offered.
pub const GAS_NEAR_MIN: u128 = 20_000_000_000_000_000_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    AddressReady,
    PaymentPending,
    ReceiptPending,
    /// Two-step only: the transfer to the vault was sent; `destination` is that transaction.
    Forwarding,
    Booked,
    Accepted,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Destination {
    pub hash: String,
    pub signer: String,
    #[serde(default)]
    pub amount: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Booking {
    Dust {
        amount: String,
        reason: String,
    },
    Unclaimed {
        amount: String,
        reason: String,
    },
    PayoutPending {
        account: String,
        amount: String,
        reason: String,
    },
}

impl Booking {
    pub fn report(&self) -> String {
        match self {
            Self::Dust { amount, reason } => format!("Dust: {amount} USDC base units ({reason}) went to the treasury balance; this amount cannot be claimed."),
            Self::Unclaimed { amount, .. } => format!("Unclaimed: {amount} USDC base units. The vault could not read the message or found no valid payout account, so the amount went to the treasury balance. No note was confirmed."),
            Self::PayoutPending { account, amount, reason } => format!(
                "Receipt booked payout: {amount} USDC base units for {account} ({reason})."
            ),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub commitment: String,
    pub quote: Value,
    pub request: Value,
    pub receipt: Option<String>,
    #[serde(default)]
    pub phase: Phase,
    #[serde(default)]
    pub destination: Option<Destination>,
    #[serde(default)]
    pub bookings: Vec<Booking>,
    #[serde(default)]
    pub notice: Option<String>,
    /// Two-step path: the user's own NEAR account that receives the USDC first.
    #[serde(default)]
    pub via: Option<String>,
}

pub fn request(
    source: &str,
    amount: u128,
    recipient: &str,
    message: &str,
    refund: &str,
    dry: bool,
    now: u64,
) -> Value {
    let deadline = chrono::DateTime::<chrono::Utc>::from_timestamp((now + 3600) as i64, 0)
        .expect("current time fits")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    json!({"dry": dry, "swapType": "EXACT_OUTPUT", "slippageTolerance": 100,
        "originAsset": source, "depositType": "ORIGIN_CHAIN", "destinationAsset": DESTINATION,
        "amount": amount.to_string(), "refundTo": refund, "refundType": "ORIGIN_CHAIN",
        "recipient": recipient, "recipientType": "DESTINATION_CHAIN", "customRecipientMsg": message, "deadline": deadline})
}

pub struct Funding {
    pub mode: Mode,
    pub home: PathBuf,
    pub rpc: Rpc,
    pub archival: Option<Rpc>,
    pub pins: Pins,
    pub notes: NoteStore,
    pub backup: backup::Target,
    pub clock: Arc<dyn Clock>,
}

fn number(v: &Value) -> Result<u128, String> {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "invalid vault amount".into())
}
impl Funding {
    pub async fn check(&self, denomination: u128) -> Result<Value, String> {
        if !self.mode.intents_enabled() {
            return Err("Intents has no testnet".into());
        }
        Tripwire::default()
            .check_against(&self.pins, &self.rpc)
            .await
            .map_err(|d| format!("vault pins differ: {}", d.join(", ")))?;
        let config = self
            .rpc
            .view(&self.pins.vault, "config", json!({}))
            .await
            .map_err(|e| e.to_string())?;
        if !config["denominations"].as_array().is_some_and(|a| {
            a.iter()
                .any(|n| n.as_str() == Some(&denomination.to_string()))
        }) {
            return Err("not a vault denomination".into());
        }
        let total = denomination
            .checked_add(number(&config["note_fee"])?)
            .ok_or("amount overflow")?;
        if number(&config["current_tvl"])?
            .checked_add(total)
            .is_none_or(|v| v > number(&config["tvl_cap"]).unwrap_or(0))
        {
            return Err("the vault TVL cap has no room for this deposit".into());
        }
        Ok(config)
    }

    pub async fn payout(&self, payout: &str, config: &Value) -> Result<bool, String> {
        let valid = payout.len() >= 2
            && payout.len() <= 64
            && payout.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                    })
                    && !part.starts_with(['-', '_'])
                    && !part.ends_with(['-', '_'])
            });
        let separated = payout
            .as_bytes()
            .windows(2)
            .all(|w| !(matches!(w[0], b'.' | b'_' | b'-') && matches!(w[1], b'.' | b'_' | b'-')));
        if !valid
            || !separated
            || [
                self.pins.vault.as_str(),
                config["token"].as_str().unwrap_or(""),
                "intents.near",
            ]
            .contains(&payout)
        {
            return Err("invalid payout account".into());
        }
        let registered = self
            .rpc
            .view(
                config["token"].as_str().ok_or("no vault token")?,
                "storage_balance_of",
                json!({"account_id": payout}),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(!registered.is_null())
    }

    pub async fn prepare(
        &self,
        client: &FundingClient,
        denomination: u128,
        source: &str,
        refund: &str,
        payout: &str,
        backup_secret: &str,
    ) -> Result<Attempt, String> {
        self.prepare_for(
            client,
            denomination,
            source,
            refund,
            payout,
            backup_secret,
            None,
        )
        .await
    }

    /// The account needs NEAR for the second step's gas and a USDC registration, for itself and
    /// the vault. Run before any source is listed.
    pub async fn two_step_ready(&self, tx: &NearTx, config: &Value) -> Result<(), String> {
        if tx.balance().await.map_err(|e| e.to_string())? < GAS_NEAR_MIN {
            return Err("the two-step path needs NEAR for gas on your account and it holds too little; it is not offered".into());
        }
        for account in [tx.signer_id(), self.pins.vault.as_str()] {
            let registered = self
                .rpc
                .view(
                    config["token"].as_str().ok_or("no vault token")?,
                    "storage_balance_of",
                    json!({"account_id": account}),
                )
                .await
                .map_err(|e| e.to_string())?;
            if registered.is_null() {
                return Err(format!(
                    "{account} is not registered for USDC; the two-step path is not offered"
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_two_step(
        &self,
        client: &FundingClient,
        tx: &NearTx,
        denomination: u128,
        source: &str,
        refund: &str,
        payout: &str,
        backup_secret: &str,
    ) -> Result<Attempt, String> {
        let config = self.check(denomination).await?;
        self.two_step_ready(tx, &config).await?;
        self.prepare_for(
            client,
            denomination,
            source,
            refund,
            payout,
            backup_secret,
            Some(tx.signer_id()),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_for(
        &self,
        client: &FundingClient,
        denomination: u128,
        source: &str,
        refund: &str,
        payout: &str,
        backup_secret: &str,
        via: Option<&str>,
    ) -> Result<Attempt, String> {
        let config = self.check(denomination).await?;
        self.payout(payout, &config).await?;
        if refund.trim().is_empty() {
            return Err("a source wallet refund address is required".into());
        }
        let total = denomination
            .checked_add(number(&config["note_fee"])?)
            .ok_or("amount overflow")?;
        if !self
            .sources(client, denomination)
            .await?
            .iter()
            .any(|s| s.asset == source)
        {
            return Err(
                "the source has no valid dry quote or its minimum exceeds this payment".into(),
            );
        }
        let (secret, commitment) = new_secret(self.mode.chain_label());
        let commitment = format!("0x{}", hex::encode(commitment.0));
        self.notes
            .add(NoteEntry {
                chain_label: self.mode.chain_label().into(),
                secret: hex::encode(secret.0),
                commitment: commitment.clone(),
                denomination: denomination.to_string(),
                payout: payout.into(),
                status: NoteStatus::Pending,
                note_id: None,
                expiry_ns: None,
                deposit_amount: None,
                tx_hash: None,
                created_at: self.clock.now_secs(),
            })
            .map_err(|e| e.to_string())?;
        let message = json!({"commitment": commitment, "withdraw_to": payout}).to_string();
        let mut request = request(
            source,
            total,
            via.unwrap_or(&self.pins.vault),
            &message,
            refund,
            false,
            self.clock.now_secs(),
        );
        if via.is_some() {
            // A plain, refundable transfer to the user's own account: no message for the token.
            request
                .as_object_mut()
                .expect("a request is an object")
                .remove("customRecipientMsg");
        }
        let quote = client
            .quote(&request)
            .await
            .map_err(|e| format!("{}. {ROUTE_FALLBACK}", e.escape_default()))?;
        validate_quote(&quote, &request, self.clock.now_secs())?;
        if let Some(account) = via {
            validate_two_step(&quote, account, &self.pins.vault)?;
        }
        let attempt = Attempt {
            commitment,
            quote,
            request,
            receipt: None,
            phase: Phase::AddressReady,
            destination: None,
            bookings: vec![],
            notice: None,
            via: via.map(String::from),
        };
        self.notes
            .save_funding(&attempt)
            .map_err(|e| e.to_string())?;
        self.back_up()?;
        // Open the file just written, using the user's recovery credential, before showing an address.
        for place in &self.backup.places {
            let restored = backup::load(&[place.join(backup::NOTES_FILE)], backup_secret)
                .map_err(|e| format!("backup restore check: {e}"))?;
            if !restored
                .notes
                .iter()
                .any(|n| n.commitment == attempt.commitment && n.status == NoteStatus::Pending)
            {
                return Err("backup restore did not recover the pending note".into());
            }
            if !restored
                .funding
                .iter()
                .any(|a| a.commitment == attempt.commitment)
            {
                return Err("backup restore has no funding quote".into());
            }
        }
        self.check(denomination).await?;
        validate_quote(&attempt.quote, &attempt.request, self.clock.now_secs())?;
        if let Some(account) = via {
            validate_two_step(&attempt.quote, account, &self.pins.vault)?;
        }
        Ok(attempt)
    }

    pub fn back_up(&self) -> Result<(), String> {
        let mut state = backup::StateBackup {
            version: backup::VERSION,
            taken_at: self.clock.now_secs(),
            notes: self.notes.list().map_err(|e| e.to_string())?,
            ..Default::default()
        };
        state.funding = self.notes.funding_attempts().map_err(|e| e.to_string())?;
        state.add_file(&self.home, "config.json");
        backup::write_backups(
            &self.backup,
            &state.to_bytes().map_err(|e| e.to_string())?,
            backup::NOTES_FILE,
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn bookings(
        &self,
        events: &[Value],
        attempt: &Attempt,
        note: &NoteEntry,
        accepted_id: Option<u64>,
    ) -> Result<Vec<Booking>, String> {
        let delivered = attempt
            .destination
            .as_ref()
            .and_then(|d| d.amount.as_deref())
            .map(|a| Value::String(a.into()))
            .unwrap_or_else(|| attempt.request["amount"].clone());
        let expected = number(&delivered).ok().and_then(|total| {
            if accepted_id.is_some() {
                total
                    .checked_sub(note.denomination.parse::<u128>().ok()?)?
                    .checked_sub(number(&self.pins.config["note_fee"]).ok()?)
            } else {
                Some(total)
            }
        });
        let mut bookings = vec![];
        for event in events {
            let d = &event["data"];
            let Some(kind) = event["event"]
                .as_str()
                .filter(|e| matches!(*e, "payout_pending" | "dust" | "unclaimed"))
            else {
                continue;
            };
            if kind == "payout_pending" && d["account"].as_str() != Some(&note.payout) {
                continue;
            }
            let Some(amount) = d["amount"]
                .as_str()
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let Ok(value) = amount.parse::<u128>() else {
                continue;
            };
            if value == 0 {
                continue;
            }
            match &d["commitment"] {
                Value::String(c) if c == &attempt.commitment => {}
                Value::Null if Some(value) == expected => {}
                _ => continue,
            }
            if d["note_id"]
                .as_u64()
                .is_some_and(|id| accepted_id.is_some_and(|ours| id != ours))
            {
                continue;
            }
            let Some(reason) = d["reason"].as_str() else {
                continue;
            };
            let booking = match (kind, reason) {
                ("payout_pending", "below_note_fee" | "transfer_failed" | "intents_excess") => {
                    Booking::PayoutPending {
                        account: note.payout.clone(),
                        amount: amount.into(),
                        reason: reason.into(),
                    }
                }
                ("dust", "below_note_fee") if value < number(&self.pins.config["note_fee"])? => {
                    Booking::Dust {
                        amount: amount.into(),
                        reason: reason.into(),
                    }
                }
                ("unclaimed", "intents_bad_msg") if accepted_id.is_none() => Booking::Unclaimed {
                    amount: amount.into(),
                    reason: reason.into(),
                },
                _ => continue,
            };
            bookings.push(booking);
        }
        if bookings.len() > 1 {
            return Err("ambiguous vault delivery events; the note is not accepted".into());
        }
        Ok(bookings)
    }

    async fn destination_outcome(
        &self,
        destination: &Destination,
    ) -> Result<(Value, &Rpc), String> {
        let params = json!({"tx_hash": destination.hash, "sender_account_id": destination.signer, "wait_until": "FINAL"});
        let regular = self
            .rpc
            .call("EXPERIMENTAL_tx_status", params.clone())
            .await;
        match regular {
            Ok(ref value)
                if !value.is_null() && (complete_receipts(value) || self.archival.is_none()) =>
            {
                return Ok((value.clone(), &self.rpc))
            }
            Err(crate::rpc::RpcError::TorDown(e)) => return Err(format!("Tor is down: {e}")),
            _ => {}
        }
        let archive = self.archival.as_ref().ok_or_else(||
            "destination receipt unavailable; set archival_rpc_url and resume with recover --intents COMMITMENT".to_string())?;
        let outcome = archive
            .call("EXPERIMENTAL_tx_status", params)
            .await
            .map_err(|e| e.to_string())?;
        if outcome.is_null() {
            return Err(
                "destination receipt unavailable from both RPC endpoints; no note was confirmed"
                    .into(),
            );
        }
        Ok((outcome, archive))
    }

    async fn pending_report(&self, rpc: &Rpc, note: &NoteEntry) -> Option<String> {
        match rpc.view(&self.pins.vault, "pending_payout", json!({"account_id": note.payout})).await {
            Ok(value) => match number(&value) {
                Ok(0) => Some(format!("No payout is currently pending for {}; there is nothing to claim.", note.payout)),
                Ok(amount) => Some(format!("Current pending payout for {}: {amount} USDC base units (account total across deposits). Run nymirad claim-payout --intents {}. This balance alone does not confirm a note.", note.payout, note.commitment)),
                Err(e) => Some(format!("Pending payout check not verified: {e}")),
            },
            Err(e) => Some(format!("Pending payout check not verified: {}", e.to_string().escape_default())),
        }
    }

    pub async fn progress(
        &self,
        client: &FundingClient,
        attempt: &mut Attempt,
    ) -> Result<Option<Accepted>, String> {
        if attempt.via.is_some() {
            return Err("this attempt uses the two-step path and needs your funding key".into());
        }
        if !self.delivered(client, attempt).await? {
            return Ok(None);
        }
        self.settle(attempt).await
    }

    /// Two-step path: waits for 1Click's USDC in the user's account, sends it on to the vault
    /// and confirms the note from the `deposit` event of that second transaction.
    pub async fn progress_two_step(
        &self,
        client: &FundingClient,
        tx: &NearTx,
        attempt: &mut Attempt,
    ) -> Result<Option<Accepted>, String> {
        let account = attempt.via.clone().ok_or("not a two-step attempt")?;
        if account != tx.signer_id() {
            return Err(format!(
                "the funding key signs for {}, not {account}",
                tx.signer_id()
            ));
        }
        if attempt.phase != Phase::Forwarding
            && (!self.delivered(client, attempt).await? || !self.forward(tx, attempt).await?)
        {
            return Ok(None);
        }
        self.settle(attempt).await
    }

    /// Sends the USDC from the user's account to the vault once it has arrived. `false` while
    /// the first delivery or its USDC is not final yet.
    async fn forward(&self, tx: &NearTx, attempt: &mut Attempt) -> Result<bool, String> {
        let account = tx.signer_id().to_string();
        let first = attempt.destination.clone().ok_or("destination missing")?;
        if !complete_receipts(&self.destination_outcome(&first).await?.0) {
            return Ok(false);
        }
        let token = self.pins.config["token"].as_str().ok_or("no vault token")?;
        let total = number(&attempt.request["amount"])?;
        let have = number(
            &self
                .rpc
                .view(token, "ft_balance_of", json!({"account_id": account}))
                .await
                .map_err(|e| e.to_string())?,
        )?;
        if have < total {
            attempt.notice = Some(format!("Waiting for the USDC to arrive in {account}."));
            return Ok(false);
        }
        let note = self
            .notes
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|n| n.commitment == attempt.commitment)
            .ok_or("pending note missing")?;
        self.check(
            note.denomination
                .parse()
                .map_err(|_| "invalid stored denomination")?,
        )
        .await?;
        let mut msg = json!({"commitment": attempt.commitment});
        if note.payout != account {
            msg["withdraw_to"] = note.payout.clone().into();
        }
        let args = json!({"receiver_id": self.pins.vault, "amount": total.to_string(), "msg": msg.to_string()});
        let row = tx
            .submit(TxRequest {
                kind: "deposit",
                method: "ft_transfer_call",
                args: args.to_string().into_bytes(),
                gas: crate::deposit::GAS_TRANSFER_CALL,
                idem_key: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        attempt.destination = Some(Destination {
            hash: row.hash.clone().ok_or("the transfer has no hash")?,
            signer: account,
            amount: None,
        });
        attempt.phase = Phase::Forwarding;
        attempt.notice = None;
        self.notes
            .save_funding(attempt)
            .map_err(|e| e.to_string())?;
        self.back_up()?;
        tx.drive(row.id).await.map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// True once 1Click reports the first delivery and `destination` is saved.
    async fn delivered(
        &self,
        client: &FundingClient,
        attempt: &mut Attempt,
    ) -> Result<bool, String> {
        if attempt.destination.is_none() {
            let q = &attempt.quote["quote"];
            let status = client
                .status(
                    q["depositAddress"]
                        .as_str()
                        .ok_or("quote has no deposit address")?,
                    q["depositMemo"].as_str(),
                )
                .await?;
            if status["status"] != "SUCCESS" {
                match status["status"].as_str() {
                    Some("REFUNDED" | "FAILED") => {
                        return Err(format!(
                            "1Click {}: {}. The note stays pending until a vault receipt is checked. {ROUTE_FALLBACK}",
                            status["status"], status["swapDetails"]["refundReason"].as_str().unwrap_or("check the source-wallet refund status").escape_default()
                        ))
                    }
                    Some(
                        "KNOWN_DEPOSIT_TX" | "PENDING_DEPOSIT" | "INCOMPLETE_DEPOSIT"
                        | "PROCESSING",
                    ) => {
                        if attempt.phase != Phase::PaymentPending {
                            attempt.phase = Phase::PaymentPending;
                            attempt.notice = Some(format!("1Click has not completed delivery. {ROUTE_FALLBACK}"));
                            self.notes.save_funding(attempt).map_err(|e| e.to_string())?;
                            self.back_up()?;
                        }
                        return Ok(false);
                    },
                    _ => return Err("unknown 1Click status".into()),
                }
            }
            let hashes = status["swapDetails"]["nearTxHashes"]
                .as_array()
                .ok_or("SUCCESS has no NEAR destination transaction")?;
            if hashes.len() != 1 {
                return Err("ambiguous NEAR destination transaction list".into());
            }
            let hash = hashes[0].as_str().ok_or("invalid NEAR transaction hash")?;
            // The live signer lookup and batched transfer shape are gated by the mainnet probe (#50).
            let signer = "intents.near";
            #[cfg(feature = "mock")]
            let signer = status["swapDetails"]["destinationSigner"]
                .as_str()
                .unwrap_or(signer);
            let amount = match &status["swapDetails"]["amountOut"] {
                Value::Null => None,
                value => {
                    let amount = number(value).map_err(|_| "invalid settled destination amount")?;
                    if amount == 0 {
                        return Err("invalid settled destination amount".into());
                    }
                    Some(amount.to_string())
                }
            };
            attempt.destination = Some(Destination {
                hash: hash.into(),
                signer: signer.into(),
                amount,
            });
            attempt.phase = Phase::ReceiptPending;
            attempt.notice = None;
            self.notes
                .save_funding(attempt)
                .map_err(|e| e.to_string())?;
            self.back_up()?;
        }
        Ok(true)
    }

    async fn settle(&self, attempt: &mut Attempt) -> Result<Option<Accepted>, String> {
        let destination = attempt.destination.clone().ok_or("destination missing")?;
        let hash = destination.hash.as_str();
        let note = self
            .notes
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|n| n.commitment == attempt.commitment)
            .ok_or("pending note missing")?;
        let (outcome, reading_rpc) = match self.destination_outcome(&destination).await {
            Ok(found) => found,
            Err(error) => {
                let diagnostic = self
                    .pending_report(&self.rpc, &note)
                    .await
                    .map(|p| format!("\n{p}"))
                    .unwrap_or_default();
                return Err(format!("{error}{diagnostic}"));
            }
        };
        if !complete_receipts(&outcome) {
            return Ok(None);
        }
        let events = crate::deposit::events_in(&outcome, &self.pins.vault);
        let matching: Vec<_> = events
            .iter()
            .filter(|e| {
                e["event"] == "deposit"
                    && e["data"]["commitment"].as_str() == Some(&attempt.commitment)
            })
            .collect();
        let pending_report = self.pending_report(reading_rpc, &note).await;
        let accepted_id = matching.first().and_then(|e| e["data"]["note_id"].as_u64());
        let booking_events = crate::deposit::successful_events_in(&outcome, &self.pins.vault);
        attempt.bookings = self.bookings(&booking_events, attempt, &note, accepted_id)?;
        if matching.is_empty() && !attempt.bookings.is_empty() {
            attempt.phase = Phase::Booked;
            let mut reports: Vec<_> = attempt.bookings.iter().map(Booking::report).collect();
            reports.extend(pending_report);
            if let Err(error) = self.notes.save_funding(attempt) {
                reports.push(format!("funding journal after delivery: {error}"));
            }
            if let Err(error) = self.back_up() {
                reports.push(format!("backup after delivery: {error}"));
            }
            return Err(reports.join("\n"));
        }
        if let (Some(account), true) = (&attempt.via, matching.is_empty()) {
            // The vault refunded the second step: the USDC is back in the user's account.
            attempt.phase = Phase::ReceiptPending;
            self.notes
                .save_funding(attempt)
                .map_err(|e| e.to_string())?;
            let held = self
                .rpc
                .view(
                    self.pins.config["token"].as_str().unwrap_or_default(),
                    "ft_balance_of",
                    json!({"account_id": account}),
                )
                .await
                .map(|v| v.as_str().unwrap_or("unknown").to_string())
                .unwrap_or_else(|_| "unknown".into());
            return Err(format!("The second step did not deposit into the vault and the USDC was refunded. It is still in {account} (balance {held} base units). Run recover --intents {} to send the deposit again.", attempt.commitment));
        }
        if matching.len() != 1 {
            return Err(format!(
                "SUCCESS has no unique matching vault deposit receipt; the note is not accepted{}",
                pending_report.map(|p| format!("\n{p}")).unwrap_or_default()
            ));
        }
        let data = &matching[0]["data"];
        let note_id: u32 = data["note_id"]
            .as_u64()
            .and_then(|v| v.try_into().ok())
            .ok_or("invalid note ID")?;
        let expiry = data["expiry"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or("invalid expiry")?;
        if data["denomination"].as_str() != Some(&note.denomination)
            || data["withdraw_to"].as_str() != Some(&note.payout)
        {
            return Err("deposit receipt amount or payout differs".into());
        }
        let records = reading_rpc
            .view(
                &self.pins.vault,
                "note_records",
                json!({"from": note_id, "limit": 1}),
            )
            .await
            .map_err(|e| e.to_string())?;
        let rec = &records[0];
        if rec["note_id"] != note_id
            || rec["deposit_amount"].as_str() != Some(&note.denomination)
            || rec["expiry"] != expiry
        {
            return Err("deposit receipt and vault note ID, amount or expiry differ".into());
        }
        let receipt = outcome["receipts_outcome"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r["outcome"]["executor_id"] == self.pins.vault
                    && r["outcome"]["logs"].as_array().is_some_and(|logs| {
                        logs.iter().any(|l| {
                            l.as_str()
                                .and_then(|l| l.strip_prefix("EVENT_JSON:"))
                                .and_then(|l| serde_json::from_str::<Value>(l).ok())
                                .is_some_and(|event| &event == matching[0])
                        })
                    })
            })
            .and_then(|r| r["id"].as_str())
            .ok_or("deposit receipt ID missing")?;
        let receipt_status = &outcome["receipts_outcome"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"].as_str() == Some(receipt))
            .ok_or("deposit receipt missing")?["outcome"]["status"];
        if receipt_status.get("SuccessValue").is_none()
            && receipt_status.get("SuccessReceiptId").is_none()
        {
            return Err("deposit receipt failed; the note is not accepted".into());
        }
        attempt.receipt = Some(receipt.into());
        let mut blocked = false;
        self.notes
            .update(&attempt.commitment, |n| {
                if !matches!(n.status, NoteStatus::Pending | NoteStatus::Accepted) {
                    blocked = true;
                    return;
                }
                n.status = NoteStatus::Accepted;
                n.note_id = Some(note_id);
                n.expiry_ns = Some(expiry);
                n.deposit_amount = Some(note.denomination.clone());
                n.tx_hash = Some(hash.into());
            })
            .map_err(|e| e.to_string())?;
        if blocked {
            return Err(
                "the note has already closed or entered an exit; its status is unchanged".into(),
            );
        }
        attempt.phase = Phase::Accepted;
        let journal_error = self.notes.save_funding(attempt).err();
        let mut warnings: Vec<String> = attempt.bookings.iter().map(|b| b.report()).collect();
        if let Some(error) = journal_error {
            warnings.push(format!("funding journal after acceptance: {error}"));
        }
        warnings.extend(pending_report);
        if expiry <= self.clock.now_secs() {
            warnings.push("The note has expired. It cannot open a lease; use an exit to recover the remaining balance.".into());
        }
        if let Err(e) = self.back_up() {
            warnings.push(format!("backup after acceptance: {e}"));
        }
        let calendar = self
            .home
            .join("calendar")
            .join(format!("reminder-{}.ics", crate::net::identity()));
        if let Err(e) = std::fs::create_dir_all(calendar.parent().unwrap()).and_then(|()| {
            std::fs::write(
                &calendar,
                crate::calendar::private_expiry_event(expiry, self.clock.now_secs()),
            )
        }) {
            warnings.push(format!("calendar: {e}"));
        }
        Ok(Some(Accepted {
            note_id,
            expiry_secs: expiry,
            deposit_amount: note
                .denomination
                .parse()
                .map_err(|_| "invalid stored denomination")?,
            tx_hash: hash.into(),
            calendar,
            warnings,
        }))
    }
}

pub const SOURCES_FILE: &str = "intents-sources.json";
pub const SOURCE_TTL: u64 = 180;
#[derive(Clone, Copy)]
pub struct Source {
    pub name: &'static str,
    pub asset: &'static str,
    pub chain: &'static str,
}
pub const SOURCES: &[Source] = &[
    Source {
        name: "ethereum-usdc",
        asset: "nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near",
        chain: "eth",
    },
    Source {
        name: "base-usdc",
        asset: "nep141:base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near",
        chain: "base",
    },
    Source {
        name: "bsc-usdc",
        asset: "nep245:v2_1.omni.hot.tg:56_2w93GqMcEmQFDru84j3HZZWt557r",
        chain: "bsc",
    },
];
pub fn minimum_payment(chain: &str) -> u128 {
    match chain {
        "bsc" | "polygon" | "ton" | "optimism" | "avalanche" | "stellar" | "monad" | "xlayer"
        | "adi" => 1_000_000_000,
        "tron" => 100_000_000,
        _ => 0,
    }
}
pub fn placeholder_message() -> String {
    json!({"commitment": format!("0x{}", "01".repeat(32)), "withdraw_to": PLACEHOLDER}).to_string()
}
#[derive(Serialize, Deserialize)]
struct CachedSource {
    at: u64,
    amount: u128,
    response: Value,
}
impl Funding {
    pub async fn sources(
        &self,
        client: &FundingClient,
        denomination: u128,
    ) -> Result<Vec<Source>, String> {
        let config = self.check(denomination).await?;
        let amount = denomination
            .checked_add(number(&config["note_fee"])?)
            .ok_or("amount overflow")?;
        let mut cache: std::collections::BTreeMap<String, CachedSource> =
            std::fs::read(self.home.join(SOURCES_FILE))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
        let mut offered = vec![];
        for source in SOURCES {
            if minimum_payment(source.chain) > amount {
                continue;
            }
            let cached = cache.get(source.asset).filter(|c| {
                c.amount == amount && self.clock.now_secs().saturating_sub(c.at) < SOURCE_TTL
            });
            if cached.is_some_and(|c| c.response.is_null()) {
                continue;
            }
            let response = match cached {
                Some(c) => c.response.clone(),
                None => {
                    let request = request(
                        source.asset,
                        amount,
                        PLACEHOLDER,
                        &placeholder_message(),
                        DUMMY,
                        true,
                        self.clock.now_secs(),
                    );
                    match client.quote(&request).await {
                        Ok(response) => response,
                        Err(_) => {
                            cache.insert(
                                source.asset.into(),
                                CachedSource {
                                    at: self.clock.now_secs(),
                                    amount,
                                    response: Value::Null,
                                },
                            );
                            continue;
                        }
                    }
                }
            };
            let deadline = response["quoteRequest"]["deadline"].clone();
            let mut expected = request(
                source.asset,
                amount,
                PLACEHOLDER,
                &placeholder_message(),
                DUMMY,
                true,
                self.clock.now_secs(),
            );
            expected["deadline"] = deadline;
            if client
                .verify(&response)
                .and_then(|()| validate_quote(&response, &expected, self.clock.now_secs()))
                .is_err()
            {
                cache.remove(source.asset);
                continue;
            }
            let at = cached
                .map(|c| c.at)
                .unwrap_or_else(|| self.clock.now_secs());
            cache.insert(
                source.asset.into(),
                CachedSource {
                    at,
                    amount,
                    response,
                },
            );
            offered.push(*source);
        }
        std::fs::write(
            self.home.join(SOURCES_FILE),
            serde_json::to_vec(&cache).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(offered)
    }
}
