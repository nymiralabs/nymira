//! Receiver for `ft_transfer_call` tests: records the call and returns a
//! configurable unused amount. Test double of the shared test harness.

use near_sdk::json_types::U128;
use near_sdk::{env, near, AccountId, PromiseOrValue};

#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub struct LastCall {
    pub predecessor: AccountId,
    pub sender_id: AccountId,
    pub amount: U128,
    pub msg: String,
}

#[near(contract_state)]
#[derive(Default)]
pub struct Receiver {
    unused: u128,
    last: Option<LastCall>,
}

#[near]
impl Receiver {
    /// Demo fallback: only this fixture account's full-access signer may deliver a fake swap.
    #[payable]
    pub fn ft_withdraw(
        &mut self,
        token: AccountId,
        receiver_id: AccountId,
        amount: U128,
        msg: String,
    ) -> near_sdk::Promise {
        near_sdk::assert_one_yocto();
        near_sdk::require!(
            env::predecessor_account_id() == env::current_account_id(),
            "fixture signer only"
        );
        near_sdk::Promise::new(token).function_call(
            "ft_transfer_call",
            near_sdk::serde_json::json!({"receiver_id": receiver_id, "amount": amount, "msg": msg})
                .to_string()
                .into_bytes(),
            near_sdk::NearToken::from_yoctonear(1),
            near_sdk::Gas::from_tgas(150),
        )
    }

    pub fn set_unused(&mut self, amount: U128) {
        self.unused = amount.0;
    }

    pub fn last_call(&self) -> Option<LastCall> {
        self.last.clone()
    }

    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        self.last = Some(LastCall {
            predecessor: env::predecessor_account_id(),
            sender_id,
            amount,
            msg,
        });
        PromiseOrValue::Value(U128(self.unused))
    }
}
