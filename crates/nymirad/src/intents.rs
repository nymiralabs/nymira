//! Direct Intents funding: local pending notes, pinned vault checks and receipt acceptance.
use crate::{
    backup,
    deposit::Accepted,
    mode::Mode,
    oneclick::{validate_quote, FundingClient},
    pins::{Pins, Tripwire},
    rpc::Rpc,
    secrets::{new_secret, NoteEntry, NoteStatus, NoteStore},
};
use nymira_operator::clock::Clock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};

pub const DESTINATION: &str =
    "nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1";
pub const PLACEHOLDER: &str = "nymira.near";
pub const DUMMY: &str = "0x1111111111111111111111111111111111111111";
pub const PRIVACY: &str = "On this path 1Click and the Intents explorer pair your source wallet with this deposit. The proofs are designed to keep the vault and the operator from tying your spending to this deposit, but while few notes are open, and without Tor, your spending can still be matched to it by time and network address. Wait a while between depositing and your first chat.";

#[derive(Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub commitment: String,
    pub quote: Value,
    pub request: Value,
    pub receipt: Option<String>,
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
        let request = request(
            source,
            total,
            &self.pins.vault,
            &message,
            refund,
            false,
            self.clock.now_secs(),
        );
        let quote = client.quote(&request).await?;
        validate_quote(&quote, &request, self.clock.now_secs())?;
        let attempt = Attempt {
            commitment,
            quote,
            request,
            receipt: None,
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

    pub async fn progress(
        &self,
        client: &FundingClient,
        attempt: &mut Attempt,
    ) -> Result<Option<Accepted>, String> {
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
                        "1Click {}; note stays pending until a vault receipt is checked",
                        status["status"]
                    ))
                }
                Some(
                    "KNOWN_DEPOSIT_TX" | "PENDING_DEPOSIT" | "INCOMPLETE_DEPOSIT" | "PROCESSING",
                ) => return Ok(None),
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
        let outcome = self
            .rpc
            .call(
                "EXPERIMENTAL_tx_status",
                json!({"tx_hash": hash, "sender_account_id": signer, "wait_until": "FINAL"}),
            )
            .await
            .map_err(|e| e.to_string())?;
        if outcome["final_execution_status"] != "FINAL" {
            return Ok(None);
        }
        let all: std::collections::BTreeSet<_> = outcome["receipts_outcome"]
            .as_array()
            .ok_or("missing receipts")?
            .iter()
            .filter_map(|r| r["id"].as_str())
            .collect();
        for r in std::iter::once(&outcome["transaction_outcome"])
            .chain(outcome["receipts_outcome"].as_array().unwrap().iter())
        {
            if r["outcome"]["receipt_ids"].as_array().is_some_and(|ids| {
                ids.iter()
                    .any(|id| !all.contains(id.as_str().unwrap_or("")))
            }) {
                return Ok(None);
            }
        }
        let events = crate::deposit::events_in(&outcome, &self.pins.vault);
        let matching: Vec<_> = events
            .iter()
            .filter(|e| {
                e["event"] == "deposit"
                    && e["data"]["commitment"].as_str() == Some(&attempt.commitment)
            })
            .collect();
        if matching.len() != 1 {
            return Err(
                "SUCCESS has no unique matching vault deposit receipt; the note is not accepted"
                    .into(),
            );
        }
        let data = &matching[0]["data"];
        let note = self
            .notes
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|n| n.commitment == attempt.commitment)
            .ok_or("pending note missing")?;
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
        let records = self
            .rpc
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
                        logs.iter()
                            .any(|l| l.as_str().is_some_and(|l| l.contains(&attempt.commitment)))
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
        self.notes
            .save_funding(attempt)
            .map_err(|e| e.to_string())?;
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
        let mut warnings = vec![];
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
