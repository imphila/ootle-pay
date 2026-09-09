//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// One-off, fully anonymous payments to a registered merchant.
///
/// The actual hidden-amount, hidden-recipient value movement happens as a native `StealthTransfer`
/// instruction alongside this contract's call in the same atomic transaction (client SDKs have
/// first-class, well-tested support for building that instruction, but no generic way to hand a
/// `StealthTransferStatement` to an arbitrary contract method as a plain argument - so this
/// contract does not try to receive one). This contract's only job is the part that *is* meant to
/// be visible: checking the merchant is registered, collecting a small per-tier platform fee, and
/// emitting an event that names only the merchant and the fee - never the private amount or the
/// payer's identity. Because both instructions share one transaction, if this call panics (unknown
/// merchant, fee too low) the whole transaction - including the stealth transfer - is rejected.
#[template]
mod private_pay_template {
    use super::*;

    pub struct PrivatePay {
        registry: ComponentManager,
        fee_vault: Vault,
    }

    /// Minimum platform fee required from a payment to a merchant of each tier (Basic pays the most
    /// per-transaction; Premium merchants, having staked the most, pay the least).
    const BASIC_MIN_FEE: u64 = 3;
    const STANDARD_MIN_FEE: u64 = 2;
    const PREMIUM_MIN_FEE: u64 = 1;

    impl PrivatePay {
        pub fn new(fee_resource: ResourceAddress, registry: ComponentAddress) -> Component<Self> {
            let access_rules = ComponentAccessRules::new()
                .method("pay", rule!(allow_all))
                .method("fee_pot_balance", rule!(allow_all));

            Component::new(Self {
                registry: ComponentManager::get(registry),
                fee_vault: Vault::new_empty(fee_resource),
            })
            .with_access_rules(access_rules)
            .create()
        }

        /// Gates and collects the platform fee for an anonymous payment to `merchant`. Call this in
        /// the same transaction as (and before) a native `StealthTransfer` instruction moving the
        /// actual private payment - if this panics, the whole transaction (including that transfer)
        /// is rejected. `fee` must be at least the minimum for the merchant's tier.
        ///
        /// Callable by: anyone.
        pub fn pay(&mut self, fee: Bucket, merchant: RistrettoPublicKeyBytes) {
            let is_registered: bool = self.registry.call("is_registered", args![merchant]);
            assert!(is_registered, "Merchant is not registered");

            let tier: String = self.registry.call("get_tier", args![merchant]);
            let min_fee = Amount::from(Self::min_fee_for_tier(&tier));
            assert!(
                fee.amount() >= min_fee,
                "Platform fee is below the minimum for this merchant's tier"
            );

            let fee_amount = fee.amount();
            self.fee_vault.deposit(fee);

            emit_event("PaymentProcessed", metadata![
                "merchant" => merchant.to_string(),
                "fee" => fee_amount.to_string(),
            ]);
        }

        pub fn fee_pot_balance(&self) -> Amount {
            self.fee_vault.balance()
        }

        /// Withdraws the entire accumulated fee pot.
        ///
        /// Callable by: the component owner.
        pub fn withdraw_fees(&mut self) -> Bucket {
            self.fee_vault.withdraw_all()
        }

        fn min_fee_for_tier(tier: &str) -> u64 {
            match tier {
                "Premium" => PREMIUM_MIN_FEE,
                "Standard" => STANDARD_MIN_FEE,
                _ => BASIC_MIN_FEE,
            }
        }
    }
}
