//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// Transparent, stake-gated registry of third-party merchants allowed to use the payment API.
///
/// Merchants are deliberately NOT anonymous here: they stake a bond to register, can be slashed for
/// misbehaviour, and their tier (derived from stake) gates how much they pay in fees on `private_pay`
/// and whether `subscription` will create plans for them. Only the *payers* (customers) need privacy,
/// which is handled entirely by the other two templates - this registry never touches a payer's key.
#[template]
mod merchant_registry_template {
    use std::collections::HashMap;

    use super::*;

    pub struct MerchantRegistry {
        stake_resource: ResourceAddress,
        min_stake: Amount,
        stakes: Vault,
        /// Confiscated stake lands here rather than being burned, since `stake_resource` is supplied
        /// by the deployer and this registry has no say over whether it is burnable.
        treasury: Vault,
        merchants: HashMap<RistrettoPublicKeyBytes, Merchant>,
        badge_manager: ResourceManager,
    }

    pub struct Merchant {
        pub staked: Amount,
        pub tier: Tier,
        pub active: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Tier {
        Basic,
        Standard,
        Premium,
    }

    fn tier_name(tier: Tier) -> &'static str {
        match tier {
            Tier::Basic => "Basic",
            Tier::Standard => "Standard",
            Tier::Premium => "Premium",
        }
    }

    /// Tier thresholds, expressed as a multiple of `min_stake`: >= 1x is Basic, >= 5x is Standard,
    /// >= 20x is Premium.
    const STANDARD_MULTIPLE: u64 = 5;
    const PREMIUM_MULTIPLE: u64 = 20;

    impl MerchantRegistry {
        pub fn new(stake_resource: ResourceAddress, min_stake: Amount) -> Component<Self> {
            assert!(min_stake > Amount::ZERO, "min_stake must be greater than zero");

            let badge_resource = ResourceBuilder::non_fungible()
                .with_token_symbol("API-BADGE")
                .mintable(rule!(allow_all), OWNER)
                .build();

            let access_rules = ComponentAccessRules::new()
                .method("register", rule!(allow_all))
                .method("add_stake", rule!(allow_all))
                .method("request_exit", rule!(allow_all))
                .method("cancel_exit_request", rule!(allow_all))
                .method("is_registered", rule!(allow_all))
                .method("get_tier", rule!(allow_all))
                .method("get_merchant_info", rule!(allow_all))
                .method("get_min_stake", rule!(allow_all));

            Component::new(Self {
                stake_resource,
                min_stake,
                stakes: Vault::new_empty(stake_resource),
                treasury: Vault::new_empty(stake_resource),
                merchants: HashMap::new(),
                badge_manager: badge_resource.into(),
            })
            .with_access_rules(access_rules)
            .create()
        }

        /// Registers the caller as a merchant, staking `stake` (must be >= `min_stake` of
        /// `stake_resource`), and returns their API badge NFT for them to deposit wherever they like.
        ///
        /// Callable by: anyone (once, per signing key).
        pub fn register(&mut self, stake: Bucket) -> Bucket {
            assert_eq!(stake.resource_address(), self.stake_resource, "Wrong stake resource");
            assert!(stake.amount() >= self.min_stake, "Stake below minimum");

            let merchant = CallerContext::transaction_signer_public_key();
            assert!(!self.merchants.contains_key(&merchant), "Already registered");

            let staked = stake.amount();
            self.stakes.deposit(stake);
            let tier = Self::tier_for_stake(self.min_stake, staked);
            self.merchants.insert(merchant, Merchant {
                staked,
                tier,
                active: true,
            });

            let badge_id = NonFungibleId::from_string(merchant.to_string());
            let badge = self
                .badge_manager
                .mint_non_fungible(badge_id, &metadata!["tier" => tier_name(tier)], &());

            emit_event("MerchantRegistered", metadata![
                "merchant" => merchant.to_string(),
                "tier" => tier_name(tier),
            ]);

            badge
        }

        /// Adds more stake to the caller's existing registration, possibly upgrading their tier.
        ///
        /// Callable by: a registered merchant (the caller must already be registered).
        pub fn add_stake(&mut self, stake: Bucket) {
            assert_eq!(stake.resource_address(), self.stake_resource, "Wrong stake resource");

            let merchant = CallerContext::transaction_signer_public_key();
            let entry = self.merchants.get_mut(&merchant).expect("Not registered");
            assert!(entry.active, "Merchant has requested exit");

            entry.staked = entry.staked + stake.amount();
            entry.tier = Self::tier_for_stake(self.min_stake, entry.staked);
            self.stakes.deposit(stake);

            emit_event("MerchantStakeIncreased", metadata![
                "merchant" => merchant.to_string(),
                "staked" => entry.staked.to_string(),
                "tier" => tier_name(entry.tier),
            ]);
        }

        /// Marks the caller's registration inactive. No new payments should be routed to an inactive
        /// merchant; the owner can then release their stake with `finalize_exit`.
        ///
        /// Callable by: a registered merchant.
        pub fn request_exit(&mut self) {
            let merchant = CallerContext::transaction_signer_public_key();
            let entry = self.merchants.get_mut(&merchant).expect("Not registered");
            entry.active = false;
            emit_event("MerchantExitRequested", metadata!["merchant" => merchant.to_string()]);
        }

        /// Reverses the caller's own pending exit request, resuming active status - only works
        /// before the owner has acted on it with `finalize_exit` or `reject_exit`.
        ///
        /// Callable by: a registered merchant.
        pub fn cancel_exit_request(&mut self) {
            let merchant = CallerContext::transaction_signer_public_key();
            let entry = self.merchants.get_mut(&merchant).expect("Not registered");
            assert!(!entry.active, "No pending exit request to cancel");
            entry.active = true;
            emit_event("MerchantExitCancelled", metadata!["merchant" => merchant.to_string()]);
        }

        /// Returns an inactive merchant's stake in full and removes their registration.
        ///
        /// Callable by: the registry owner.
        pub fn finalize_exit(&mut self, merchant: RistrettoPublicKeyBytes) -> Bucket {
            let entry = self.merchants.get(&merchant).expect("Not registered");
            assert!(!entry.active, "Merchant has not requested exit");
            let amount = entry.staked;
            self.merchants.remove(&merchant);
            emit_event("MerchantExitFinalized", metadata![
                "merchant" => merchant.to_string(),
                "returned" => amount.to_string(),
            ]);
            self.stakes.withdraw(amount)
        }

        /// Rejects a merchant's pending exit request, confiscating their entire remaining stake into
        /// the registry's treasury and removing their registration outright. `reason` is recorded
        /// on-chain so the confiscation is never just an opaque decision - e.g. because a dispute
        /// investigation found against the merchant and finalizing their exit would let them walk
        /// away with their bond intact.
        ///
        /// Callable by: the registry owner.
        pub fn reject_exit(&mut self, merchant: RistrettoPublicKeyBytes, reason: String) {
            let entry = self.merchants.get(&merchant).expect("Not registered");
            assert!(!entry.active, "Merchant has not requested exit");
            let amount = entry.staked;
            self.merchants.remove(&merchant);
            let confiscated = self.stakes.withdraw(amount);
            self.treasury.deposit(confiscated);

            emit_event("MerchantExitRejected", metadata![
                "merchant" => merchant.to_string(),
                "confiscated" => amount.to_string(),
                "reason" => reason,
            ]);
        }

        /// Confiscates `amount` of a merchant's stake (e.g. after a dispute finds against them) into
        /// the registry's treasury, recomputing (and possibly downgrading) their tier.
        ///
        /// Callable by: the registry owner.
        pub fn slash(&mut self, merchant: RistrettoPublicKeyBytes, amount: Amount) {
            let entry = self.merchants.get_mut(&merchant).expect("Not registered");
            assert!(amount > Amount::ZERO, "Slash amount must be greater than zero");
            assert!(amount <= entry.staked, "Cannot slash more than the merchant has staked");

            entry.staked = entry.staked - amount;
            entry.tier = Self::tier_for_stake(self.min_stake, entry.staked);
            let slashed = self.stakes.withdraw(amount);
            self.treasury.deposit(slashed);

            emit_event("MerchantSlashed", metadata![
                "merchant" => merchant.to_string(),
                "slashed" => amount.to_string(),
                "remaining_stake" => entry.staked.to_string(),
                "tier" => tier_name(entry.tier),
            ]);
        }

        pub fn treasury_balance(&self) -> Amount {
            self.treasury.balance()
        }

        /// Withdraws the entire treasury (accumulated from slashing).
        ///
        /// Callable by: the registry owner.
        pub fn withdraw_treasury(&mut self) -> Bucket {
            self.treasury.withdraw_all()
        }

        /// The deployer-chosen minimum stake (1x this is Basic, 5x Standard, 20x Premium) - lets
        /// callers show the real tier thresholds instead of hardcoding them.
        pub fn get_min_stake(&self) -> Amount {
            self.min_stake
        }

        pub fn is_registered(&self, merchant: RistrettoPublicKeyBytes) -> bool {
            self.merchants.get(&merchant).map(|m| m.active).unwrap_or(false)
        }

        /// Returns the merchant's tier as a string ("Basic" / "Standard" / "Premium"). Panics if the
        /// merchant is not registered.
        pub fn get_tier(&self, merchant: RistrettoPublicKeyBytes) -> String {
            tier_name(self.merchants.get(&merchant).expect("Not registered").tier).to_string()
        }

        pub fn get_merchant_info(&self, merchant: RistrettoPublicKeyBytes) -> (Amount, String, bool) {
            let entry = self.merchants.get(&merchant).expect("Not registered");
            (entry.staked, tier_name(entry.tier).to_string(), entry.active)
        }

        fn tier_for_stake(min_stake: Amount, staked: Amount) -> Tier {
            if staked >= min_stake * Amount::from(PREMIUM_MULTIPLE) {
                Tier::Premium
            } else if staked >= min_stake * Amount::from(STANDARD_MULTIPLE) {
                Tier::Standard
            } else {
                Tier::Basic
            }
        }
    }
}
