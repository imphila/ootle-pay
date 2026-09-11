//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// Recurring subscription payments. Plan price and merchant are public (merchants publish their
/// own pricing anyway), and subscriptions are keyed by the subscriber's own signing key, read via
/// `CallerContext::transaction_signer_public_key()` - ordinary identity tracking, no anonymity
/// engineering (Ootle's L2 can't offer a genuine cryptographic guarantee for "who signed this
/// call" the way L1 confidential transactions hide amounts, so this contract doesn't pretend to).
#[template]
mod subscription_template {
    use std::collections::HashMap;

    use super::*;

    pub struct SubscriptionManager {
        registry: ComponentManager,
        next_plan_id: u32,
        plans: HashMap<u32, Plan>,
        subscriptions: HashMap<RistrettoPublicKeyBytes, Subscription>,
        badge_manager: ResourceManager,
    }

    pub struct Plan {
        pub merchant: RistrettoPublicKeyBytes,
        pub price: Amount,
        pub resource: ResourceAddress,
        pub current_period: u32,
        pub revenue: Vault,
    }

    pub struct Subscription {
        pub plan_id: u32,
        pub paid_until_period: u32,
    }

    impl SubscriptionManager {
        pub fn new(registry: ComponentAddress) -> Component<Self> {
            let badge_resource = ResourceBuilder::non_fungible()
                .with_token_symbol("SUB-BADGE")
                .mintable(rule!(allow_all), OWNER)
                .build();

            let access_rules = ComponentAccessRules::new()
                .method("create_plan", rule!(allow_all))
                .method("advance_period", rule!(allow_all))
                .method("subscribe", rule!(allow_all))
                .method("renew", rule!(allow_all))
                .method("is_active", rule!(allow_all))
                .method("claim_plan_revenue", rule!(allow_all));

            Component::new(Self {
                registry: ComponentManager::get(registry),
                next_plan_id: 0,
                plans: HashMap::new(),
                subscriptions: HashMap::new(),
                badge_manager: badge_resource.into(),
            })
            .with_access_rules(access_rules)
            .create()
        }

        /// Registers a new subscription plan for `merchant` (who must already be registered in the
        /// merchant registry) at a fixed public `price`, denominated in `resource`. `name` is not
        /// stored - it only ever lives in the `PlanCreated` event, which is how subscribers (and
        /// the merchant's own dashboard) discover it; the contract itself has no use for it.
        ///
        /// Callable by: anyone (the registry check is what actually gates this).
        pub fn create_plan(
            &mut self,
            merchant: RistrettoPublicKeyBytes,
            name: String,
            price: Amount,
            resource: ResourceAddress,
        ) -> u32 {
            let is_registered: bool = self.registry.call("is_registered", args![merchant]);
            assert!(is_registered, "Merchant is not registered");
            assert!(price > Amount::ZERO, "Price must be greater than zero");

            let plan_id = self.next_plan_id;
            self.next_plan_id += 1;
            self.plans.insert(plan_id, Plan {
                merchant,
                price,
                resource,
                current_period: 0,
                revenue: Vault::new_empty(resource),
            });

            emit_event("PlanCreated", metadata![
                "plan_id" => plan_id.to_string(),
                "merchant" => merchant.to_string(),
                "name" => name,
                "price" => price.to_string(),
            ]);
            plan_id
        }

        /// Advances a plan's billing period by one. Permissionless: there's no on-chain wall-clock, so
        /// whoever runs the merchant's billing calendar (typically the merchant, but anyone may call
        /// this) drives the period counter forward once per billing cycle.
        pub fn advance_period(&mut self, plan_id: u32) {
            let plan = self.plans.get_mut(&plan_id).expect("Unknown plan");
            plan.current_period += 1;
            emit_event("PeriodAdvanced", metadata![
                "plan_id" => plan_id.to_string(),
                "period" => plan.current_period.to_string(),
            ]);
        }

        /// Subscribes the caller to `plan_id` for the first time. `payment` must be exactly the
        /// plan's price in its resource. Returns a membership badge NFT for the caller to deposit
        /// into any account they choose; use `renew` (not this method again) for subsequent periods.
        ///
        /// Callable by: anyone.
        ///
        /// # Panics
        /// Panics if the caller is already subscribed to any plan.
        pub fn subscribe(&mut self, plan_id: u32, payment: Bucket) -> Bucket {
            let subscriber = CallerContext::transaction_signer_public_key();
            assert!(
                !self.subscriptions.contains_key(&subscriber),
                "This account is already subscribed"
            );
            let paid_until_period = self.pay_into_plan(plan_id, payment);
            self.subscriptions.insert(subscriber, Subscription {
                plan_id,
                paid_until_period,
            });

            emit_event("SubscriptionRenewed", metadata![
                "plan_id" => plan_id.to_string(),
                "subscriber" => subscriber.to_string(),
                "paid_until_period" => paid_until_period.to_string(),
            ]);

            let badge_id = NonFungibleId::from_string(subscriber.to_string());
            self.badge_manager
                .mint_non_fungible(badge_id, &metadata!["plan_id" => plan_id.to_string()], &())
        }

        /// Extends the caller's existing subscription by one more billing period. `payment` must be
        /// exactly the plan's price in its resource.
        ///
        /// Callable by: anyone.
        ///
        /// # Panics
        /// Panics if the caller has no existing subscription to `plan_id`.
        pub fn renew(&mut self, plan_id: u32, payment: Bucket) {
            let subscriber = CallerContext::transaction_signer_public_key();
            let paid_until_period = self.pay_into_plan(plan_id, payment);
            let entry = self.subscriptions.get_mut(&subscriber).expect("No existing subscription");
            assert_eq!(entry.plan_id, plan_id, "This account is subscribed to a different plan");
            entry.paid_until_period = paid_until_period;

            emit_event("SubscriptionRenewed", metadata![
                "plan_id" => plan_id.to_string(),
                "subscriber" => subscriber.to_string(),
                "paid_until_period" => paid_until_period.to_string(),
            ]);
        }

        /// Validates and deposits `payment` into `plan_id`'s revenue vault, returning the period it
        /// pays up to (the plan's current period plus one). Used by both `subscribe` and `renew` -
        /// checking the merchant's registration here means a merchant who has called `request_exit`
        /// (or been fully exited/rejected) can no longer collect on plans they created while still
        /// registered, whether that's a first subscription or a renewal.
        fn pay_into_plan(&mut self, plan_id: u32, payment: Bucket) -> u32 {
            let merchant = self.plans.get(&plan_id).expect("Unknown plan").merchant;
            let is_registered: bool = self.registry.call("is_registered", args![merchant]);
            assert!(is_registered, "Merchant is no longer registered");

            let plan = self.plans.get_mut(&plan_id).expect("Unknown plan");
            assert_eq!(payment.resource_address(), plan.resource, "Wrong payment resource");
            assert_eq!(payment.amount(), plan.price, "Payment must equal the plan price exactly");
            let paid_until_period = plan.current_period + 1;
            plan.revenue.deposit(payment);
            paid_until_period
        }

        pub fn is_active(&self, plan_id: u32, subscriber: RistrettoPublicKeyBytes) -> bool {
            let Some(plan) = self.plans.get(&plan_id) else {
                return false;
            };
            self.subscriptions
                .get(&subscriber)
                .map(|s| s.plan_id == plan_id && s.paid_until_period > plan.current_period)
                .unwrap_or(false)
        }

        /// Withdraws all revenue collected so far for `plan_id`.
        ///
        /// Callable by: the plan's merchant.
        pub fn claim_plan_revenue(&mut self, plan_id: u32) -> Bucket {
            let plan = self.plans.get_mut(&plan_id).expect("Unknown plan");
            let caller = CallerContext::transaction_signer_public_key();
            assert_eq!(caller, plan.merchant, "Only the plan's merchant can claim its revenue");
            plan.revenue.withdraw_all()
        }
    }
}
