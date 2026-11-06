//! The agent API beside chat: `GET /v1/nymira/balance` and `POST /v1/nymira/quote`, and the
//! request headers (`X-Nymira-Max-Cost-Usd`, `X-Nymira-Max-Lease-Usd`, `X-Nymira-Approval-Id`,
//! `Idempotency-Key`).
//!
//! Every field these routes return is a number, a flag or a word from a fixed list. Nothing the
//! operator or the model wrote is copied into them, so a reply from `nymirad` cannot carry
//! instructions. Both routes answer for the token's own budget note and for no other.

use super::*;

/// The per-call cost cap, in USD with at most 6 decimals.
pub const COST_CAP_HEADER: &str = "x-nymira-max-cost-usd";
pub const BALANCE_PATH: &str = "/v1/nymira/balance";
pub const QUOTE_PATH: &str = "/v1/nymira/quote";

/// "0.0123" to micro-USD. Digits only, one optional point, at most 6 decimals.
pub fn parse_usd(text: &str) -> Option<u128> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || whole.len() > 9 || frac.len() > 6 || !digits(whole) || !digits(frac) {
        return None;
    }
    if text.ends_with('.') {
        return None;
    }
    let micro = format!("{frac:0<6}").parse::<u128>().ok()?;
    Some(whole.parse::<u128>().ok()? * 1_000_000 + micro)
}

/// The options an HTTP request carries: the lease class, the approval ID and the cost cap.
#[allow(clippy::result_large_err)]
pub(super) fn options(headers: &HeaderMap, scope: Scope) -> Result<ChatOptions, ApiError> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let max_cost_micro = match headers.get(COST_CAP_HEADER) {
        None => None,
        Some(v) => Some(v.to_str().ok().and_then(parse_usd).ok_or_else(|| {
            ApiError::invalid_request(
                "x-nymira-max-cost-usd is a USD amount with at most 6 decimals, such as 0.002",
            )
        })?),
    };
    Ok(ChatOptions {
        max_lease_usd: text("x-nymira-max-lease-usd").and_then(|v| v.parse().ok()),
        scope,
        approval_id: text(crate::approvals::HEADER).map(String::from),
        max_cost_micro,
    })
}

impl Gateway {
    /// The agent's note in words: `open`, or why no new request is served on it.
    fn note_word(&self, note: &str, entry: Option<&NoteEntry>) -> &'static str {
        let revoking = self
            .revoking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(note);
        match entry.map(|e| e.status) {
            _ if revoking => "revoking",
            Some(NoteStatus::Exiting) => "exiting",
            Some(NoteStatus::Closed) => "closed",
            Some(NoteStatus::Accepted) => {
                let short = self.guard.enabled
                    && entry
                        .and_then(|e| e.expiry_ns)
                        .is_some_and(|e| crate::expiry::lease_blocked(e, self.clock.now_secs()));
                if short {
                    "expiring"
                } else {
                    "open"
                }
            }
            _ => "unfunded",
        }
    }

    /// The balance route's answer for the agent in `opts.scope`. The balance is the deposit less
    /// the charges of settled leases; a lease still open is not counted until it settles.
    pub fn balance_json(&self, opts: &ChatOptions) -> Value {
        let Scope::Agent { name, note, policy } = &opts.scope else {
            return json!({});
        };
        let entry = self
            .notes
            .list()
            .ok()
            .and_then(|n| n.into_iter().find(|n| &n.commitment == note));
        let balance = entry
            .as_ref()
            .and_then(|e| self.note_balance(e))
            .unwrap_or(0);
        let max_lease = opts.scope.lease_request(opts.max_lease_usd);
        let bound = lease::choose_bound(&self.bound_classes(), max_lease).unwrap_or(0);
        let status = self.note_word(note, entry.as_ref());
        let spendable = if status == "open" && balance >= bound {
            balance
        } else {
            0
        };
        json!({
            "balance_micro": balance,
            "spendable_micro": spendable,
            "lease_bound_micro": bound,
            "spent_today_micro": self.agent_spend_today(name, note),
            "note": {
                "status": status,
                "closing": status != "open",
                "expires_at": entry.and_then(|e| e.expiry_ns).map(|ns| ns / 1_000_000_000),
            },
            "charge_warning": self.charge_warnings.active(note),
            "limits": super::revoke::policy_json(policy),
        })
    }

    /// The quote route: the decision chat would take for this request, with no counter moved,
    /// no lease opened, nothing sent and no approval held. `decision` is `allow`,
    /// `needs_approval` or `deny`; `reason` is a code from the agent API's table.
    pub fn quote_json(&self, request: &Value, opts: &ChatOptions) -> Value {
        let mut opts = opts.clone();
        opts.max_lease_usd = opts.scope.lease_request(opts.max_lease_usd);
        let denied = |e: ApiError| {
            let e = e.typed();
            let mut out = json!({
                "decision": "deny",
                "reason": e.code,
                "http_status": e.status.as_u16(),
            });
            if let Some(detail) = &e.detail {
                out["detail"] = json!(detail);
            }
            if let Some(secs) = e.retry_after_secs {
                out["retry_after_secs"] = json!(secs);
            }
            out
        };
        let pre = match self.preflight(&opts, request, true) {
            Ok(pre) => pre,
            Err(e) => return denied(e),
        };
        let Some(a) = &pre.admitted else {
            return json!({ "decision": "deny", "reason": "invalid_request", "http_status": 403 });
        };
        let figures = json!({
            "model": pre.model,
            "input_tokens": a.input_tokens,
            "max_tokens": pre.caller_max.map_or(a.cap, |c| c.min(a.cap)),
            "estimated_cost_micro": a.worst,
        });
        let mut out = if pre.needs_approval {
            json!({ "decision": "needs_approval", "reason": "approval_required" })
        } else {
            match self.quote_blocker(&opts) {
                Some(e) => denied(e),
                None => json!({ "decision": "allow", "reason": null }),
            }
        };
        if let (Some(out), Some(figures)) = (out.as_object_mut(), figures.as_object()) {
            out.extend(figures.clone());
        }
        out
    }

    /// What chat would hit after the policy checks and before the lease: an emergency notice, a
    /// dead Tor, a balance under the lease bound. All of it is read locally.
    fn quote_blocker(&self, opts: &ChatOptions) -> Option<ApiError> {
        if let Some(why) = self.emergency_refusal() {
            return Some(why);
        }
        if let Err(e) = self.transport.ensure_ready() {
            return Some(net_error("operator_unreachable", e));
        }
        let Scope::Agent { note, .. } = &opts.scope else {
            return None;
        };
        let bound = lease::choose_bound(&self.bound_classes(), opts.max_lease_usd).unwrap_or(0);
        let balance = self
            .notes
            .list()
            .ok()
            .and_then(|n| n.into_iter().find(|n| &n.commitment == note))
            .and_then(|e| self.note_balance(&e))
            .unwrap_or(0);
        (balance < bound).then(ApiError::budget_exhausted)
    }
}

// ---- HTTP ----

pub async fn balance_handler(
    State(gw): State<Arc<Gateway>>,
    axum::Extension(scope): axum::Extension<Scope>,
    headers: HeaderMap,
) -> Response {
    if !scope.is_agent() {
        return forbidden();
    }
    match options(&headers, scope) {
        Ok(opts) => axum::Json(gw.balance_json(&opts)).into_response(),
        Err(e) => e.typed().into_response(),
    }
}

pub async fn quote_handler(
    State(gw): State<Arc<Gateway>>,
    axum::Extension(scope): axum::Extension<Scope>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !scope.is_agent() {
        return forbidden();
    }
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return ApiError::invalid_request("the body is not JSON").into_response();
    };
    match options(&headers, scope) {
        Ok(opts) => axum::Json(gw.quote_json(&request, &opts)).into_response(),
        Err(e) => e.typed().into_response(),
    }
}

fn forbidden() -> Response {
    let mut e = ApiError::invalid_request("this route is for agent tokens");
    e.status = StatusCode::FORBIDDEN;
    e.code = "agent_route_only".into();
    e.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cost_cap_is_a_decimal_with_at_most_six_places() {
        assert_eq!(parse_usd("0.002"), Some(2_000));
        assert_eq!(parse_usd("1"), Some(1_000_000));
        assert_eq!(parse_usd("0.000001"), Some(1));
        assert_eq!(parse_usd("12.5"), Some(12_500_000));
        assert_eq!(parse_usd("0"), Some(0));
        for bad in [
            "",
            ".5",
            "1.",
            "0.0000001",
            "-1",
            "1e-3",
            "0x10",
            " 1",
            "1,5",
            "+1",
        ] {
            assert_eq!(parse_usd(bad), None, "{bad:?}");
        }
    }
}
