//! Mock USDC: a NEP-141 and NEP-145 token with the registration rules the vault
//! relies on, plus switches for the failures a real token can show (pause,
//! blocklist, a panic in `ft_transfer`, junk return bytes). Test double of the
//! shared test harness; not for deployment.

use near_sdk::json_types::U128;
use near_sdk::store::{LookupMap, LookupSet};
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, Gas, NearToken, Promise, PromiseOrValue,
};

/// 0.00125 NEAR, the `storage_balance_bounds` minimum of the real USDC.
pub const MIN_STORAGE_DEPOSIT: u128 = 1_250_000_000_000_000_000_000;
const GAS_FOR_RESOLVE_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_FOR_FT_TRANSFER_CALL: Gas = Gas::from_tgas(30);
const GAS_MARGIN: Gas = Gas::from_tgas(5);

#[near(serializers = [json])]
pub struct StorageBalance {
    pub total: U128,
    pub available: U128,
}

#[near(serializers = [json])]
pub struct StorageBalanceBounds {
    pub min: U128,
    pub max: Option<U128>,
}

#[near(serializers = [json])]
pub struct FungibleTokenMetadata {
    pub spec: String,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
}

#[near(contract_state)]
pub struct MockUsdc {
    balances: LookupMap<AccountId, u128>,
    registered: LookupSet<AccountId>,
    total_supply: u128,
    paused: bool,
    blocked: Option<AccountId>,
    panic_in_transfer: bool,
    junk_return: bool,
}

impl Default for MockUsdc {
    fn default() -> Self {
        Self {
            balances: LookupMap::new(b"b"),
            registered: LookupSet::new(b"r"),
            total_supply: 0,
            paused: false,
            blocked: None,
            panic_in_transfer: false,
            junk_return: false,
        }
    }
}

impl MockUsdc {
    fn balance(&self, account: &AccountId) -> u128 {
        self.balances.get(account).copied().unwrap_or(0)
    }

    fn check_transfer_allowed(&self, sender: &AccountId, receiver: &AccountId) {
        require!(!self.paused, "token is paused");
        require!(!self.panic_in_transfer, "ft_transfer panics (test switch)");
        if let Some(blocked) = &self.blocked {
            require!(
                blocked != sender && blocked != receiver,
                "account is blocklisted"
            );
        }
    }

    fn move_tokens(&mut self, from: &AccountId, to: &AccountId, amount: u128) {
        require!(amount > 0, "amount must be positive");
        require!(self.registered.contains(to), "receiver is not registered");
        let from_balance = self.balance(from);
        require!(from_balance >= amount, "not enough balance");
        self.balances.insert(from.clone(), from_balance - amount);
        let to_balance = self.balance(to);
        self.balances.insert(to.clone(), to_balance + amount);
    }
}

#[near]
impl MockUsdc {
    // ---- test helpers and switches (no access control: test double) ----

    /// Credits `amount` to `account_id` without registration or checks.
    pub fn mint(&mut self, account_id: AccountId, amount: U128) {
        let balance = self.balance(&account_id);
        self.balances.insert(account_id, balance + amount.0);
        self.total_supply += amount.0;
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Blocklists one account (as sender and as receiver), or clears it.
    pub fn set_blocked(&mut self, account_id: Option<AccountId>) {
        self.blocked = account_id;
    }

    pub fn set_panic_in_transfer(&mut self, on: bool) {
        self.panic_in_transfer = on;
    }

    /// A successful `ft_transfer` returns junk bytes instead of nothing.
    pub fn set_junk_return(&mut self, on: bool) {
        self.junk_return = on;
    }

    // ---- NEP-141 ----

    pub fn ft_total_supply(&self) -> U128 {
        U128(self.total_supply)
    }

    pub fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.balance(&account_id))
    }

    pub fn ft_metadata(&self) -> FungibleTokenMetadata {
        FungibleTokenMetadata {
            spec: "ft-1.0.0".into(),
            name: "Mock USDC".into(),
            symbol: "USDC".into(),
            decimals: 6,
        }
    }

    #[payable]
    pub fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        assert_one_yocto();
        let _ = memo;
        let sender = env::predecessor_account_id();
        self.check_transfer_allowed(&sender, &receiver_id);
        self.move_tokens(&sender, &receiver_id, amount.0);
        if self.junk_return {
            env::value_return(b"\xde\xad\xbe\xefjunk");
        }
    }

    #[payable]
    pub fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        assert_one_yocto();
        let _ = memo;
        require!(
            env::prepaid_gas() > GAS_FOR_RESOLVE_TRANSFER.saturating_add(GAS_FOR_FT_TRANSFER_CALL),
            "more gas is required"
        );
        let sender = env::predecessor_account_id();
        self.check_transfer_allowed(&sender, &receiver_id);
        self.move_tokens(&sender, &receiver_id, amount.0);
        let payload = near_sdk::serde_json::json!({
            "sender_id": sender,
            "amount": amount,
            "msg": msg,
        })
        .to_string()
        .into_bytes();
        // Leave the resolve call its share and a margin for building the promises.
        let on_transfer_gas = env::prepaid_gas()
            .saturating_sub(env::used_gas())
            .saturating_sub(GAS_FOR_RESOLVE_TRANSFER)
            .saturating_sub(GAS_MARGIN);
        PromiseOrValue::Promise(
            Promise::new(receiver_id.clone())
                .function_call(
                    "ft_on_transfer".to_string(),
                    payload,
                    NearToken::from_yoctonear(0),
                    on_transfer_gas,
                )
                .then(
                    Self::ext(env::current_account_id())
                        .with_static_gas(GAS_FOR_RESOLVE_TRANSFER)
                        .ft_resolve_transfer(sender, receiver_id, amount),
                ),
        )
    }

    #[private]
    pub fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> U128 {
        let unused = match env::promise_result_checked(0, 64) {
            Ok(bytes) => near_sdk::serde_json::from_slice::<U128>(&bytes)
                .map(|value| value.0.min(amount.0))
                .unwrap_or(amount.0),
            Err(_) => amount.0,
        };
        let refund = unused.min(self.balance(&receiver_id));
        if refund > 0 {
            let receiver_balance = self.balance(&receiver_id);
            self.balances.insert(receiver_id, receiver_balance - refund);
            let sender_balance = self.balance(&sender_id);
            self.balances.insert(sender_id, sender_balance + refund);
        }
        U128(amount.0 - refund)
    }

    // ---- NEP-145 ----

    /// Registers `account_id` (default: the caller). The minimum is kept and any
    /// excess is refunded, whether or not `registration_only` is set; for an
    /// account that is already registered the whole deposit is refunded.
    #[payable]
    pub fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        let _ = registration_only;
        let amount = env::attached_deposit().as_yoctonear();
        let account = account_id.unwrap_or_else(env::predecessor_account_id);
        let refund = if self.registered.contains(&account) {
            amount
        } else {
            require!(
                amount >= MIN_STORAGE_DEPOSIT,
                "deposit is below the storage minimum"
            );
            self.registered.insert(account);
            amount - MIN_STORAGE_DEPOSIT
        };
        if refund > 0 {
            Promise::new(env::predecessor_account_id())
                .transfer(NearToken::from_yoctonear(refund))
                .detach();
        }
        StorageBalance {
            total: U128(MIN_STORAGE_DEPOSIT),
            available: U128(0),
        }
    }

    pub fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.registered
            .contains(&account_id)
            .then(|| StorageBalance {
                total: U128(MIN_STORAGE_DEPOSIT),
                available: U128(0),
            })
    }

    pub fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        StorageBalanceBounds {
            min: U128(MIN_STORAGE_DEPOSIT),
            max: Some(U128(MIN_STORAGE_DEPOSIT)),
        }
    }
}
