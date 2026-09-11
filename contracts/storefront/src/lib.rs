//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// One-off product purchases with ordinary, fully visible order records: amount, product, and
/// buyer are all part of the on-chain `OrderPlaced` event, giving a merchant real order/revenue
/// tracking (which product sold, to whom, how much, running totals) with no anonymity
/// engineering - Ootle's L2 can't offer a genuine cryptographic identity guarantee for "who
/// signed this call" the way L1 confidential transactions hide amounts, so this contract doesn't
/// pretend to.
#[template]
mod storefront_template {
    use std::collections::HashMap;

    use super::*;

    pub struct Storefront {
        registry: ComponentManager,
        next_product_id: u32,
        products: HashMap<u32, Product>,
    }

    pub struct Product {
        pub merchant: RistrettoPublicKeyBytes,
        pub price: Amount,
        pub resource: ResourceAddress,
        pub order_count: u32,
        pub revenue: Vault,
        pub orders: HashMap<u32, Order>,
    }

    pub struct Order {
        pub buyer: RistrettoPublicKeyBytes,
        pub amount: Amount,
        pub refunded: Amount,
        pub status: RefundStatus,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RefundStatus {
        None,
        Requested,
        Denied,
    }

    fn refund_status_name(status: RefundStatus) -> &'static str {
        match status {
            RefundStatus::None => "None",
            RefundStatus::Requested => "Requested",
            RefundStatus::Denied => "Denied",
        }
    }

    impl Storefront {
        pub fn new(registry: ComponentAddress) -> Component<Self> {
            let access_rules = ComponentAccessRules::new()
                .method("create_product", rule!(allow_all))
                .method("buy", rule!(allow_all))
                .method("get_product_info", rule!(allow_all))
                .method("claim_revenue", rule!(allow_all))
                .method("request_refund", rule!(allow_all))
                .method("approve_refund", rule!(allow_all))
                .method("deny_refund", rule!(allow_all))
                .method("get_order_info", rule!(allow_all));

            Component::new(Self {
                registry: ComponentManager::get(registry),
                next_product_id: 0,
                products: HashMap::new(),
            })
            .with_access_rules(access_rules)
            .create()
        }

        /// Registers a new product for `merchant` (who must already be registered in the merchant
        /// registry) at a fixed public `price`, denominated in `resource`. `name` is not stored -
        /// it only ever lives in the `ProductCreated` event, which is how buyers (and the
        /// merchant's own dashboard) discover it; the contract itself has no use for it.
        ///
        /// Callable by: anyone (the registry check is what actually gates this).
        pub fn create_product(
            &mut self,
            merchant: RistrettoPublicKeyBytes,
            name: String,
            price: Amount,
            resource: ResourceAddress,
        ) -> u32 {
            let is_registered: bool = self.registry.call("is_registered", args![merchant]);
            assert!(is_registered, "Merchant is not registered");
            assert!(price > Amount::ZERO, "Price must be greater than zero");

            let product_id = self.next_product_id;
            self.next_product_id += 1;
            self.products.insert(product_id, Product {
                merchant,
                price,
                resource,
                order_count: 0,
                revenue: Vault::new_empty(resource),
                orders: HashMap::new(),
            });

            emit_event("ProductCreated", metadata![
                "product_id" => product_id.to_string(),
                "merchant" => merchant.to_string(),
                "name" => name,
                "price" => price.to_string(),
            ]);
            product_id
        }

        /// Buys `product_id`. `payment` must be exactly the product's price in its resource.
        /// Returns this order's number (i.e. `order_count` after this purchase).
        ///
        /// Callable by: anyone, as long as the product's merchant is still registered - a merchant
        /// who has called `request_exit` (or been fully exited/rejected) can no longer sell, even on
        /// products they listed while still registered.
        /// Records the buyer's public key as part of the order.
        pub fn buy(&mut self, product_id: u32, payment: Bucket) -> u32 {
            let buyer = CallerContext::transaction_signer_public_key();
            let merchant = self.products.get(&product_id).expect("Unknown product").merchant;
            let is_registered: bool = self.registry.call("is_registered", args![merchant]);
            assert!(is_registered, "Merchant is no longer registered");

            let product = self.products.get_mut(&product_id).expect("Unknown product");
            assert_eq!(payment.resource_address(), product.resource, "Wrong payment resource");
            assert_eq!(payment.amount(), product.price, "Payment must equal the product price exactly");

            let amount = payment.amount();
            product.revenue.deposit(payment);
            product.order_count += 1;
            let order_number = product.order_count;
            product.orders.insert(order_number, Order {
                buyer,
                amount,
                refunded: Amount::ZERO,
                status: RefundStatus::None,
            });

            emit_event("OrderPlaced", metadata![
                "product_id" => product_id.to_string(),
                "merchant" => product.merchant.to_string(),
                "buyer" => buyer.to_string(),
                "amount" => product.price.to_string(),
                "order_number" => order_number.to_string(),
            ]);
            order_number
        }

        /// Returns `(price, order_count, revenue collected so far)` for `product_id`.
        pub fn get_product_info(&self, product_id: u32) -> (Amount, u32, Amount) {
            let product = self.products.get(&product_id).expect("Unknown product");
            (product.price, product.order_count, product.revenue.balance())
        }

        /// Withdraws all revenue collected so far for `product_id`.
        ///
        /// Callable by: the product's merchant.
        pub fn claim_revenue(&mut self, product_id: u32) -> Bucket {
            let product = self.products.get_mut(&product_id).expect("Unknown product");
            let caller = CallerContext::transaction_signer_public_key();
            assert_eq!(caller, product.merchant, "Only the product's merchant can claim its revenue");
            product.revenue.withdraw_all()
        }

        /// Requests a refund for `order_id`, recording `reason` on-chain. This is the buyer's move
        /// when they want their money back - the merchant then either `approve_refund`s or
        /// `deny_refund`s it, and a denial is the buyer's cue to escalate to the platform arbiter via
        /// `merchant_registry::resolve_dispute`, not a dead end.
        ///
        /// Callable by: the order's buyer.
        pub fn request_refund(&mut self, product_id: u32, order_id: u32, reason: String) {
            let product = self.products.get_mut(&product_id).expect("Unknown product");
            let order = product.orders.get_mut(&order_id).expect("Unknown order");
            let caller = CallerContext::transaction_signer_public_key();
            assert_eq!(caller, order.buyer, "Only the order's buyer can request a refund");
            assert!(order.refunded < order.amount, "Order has already been fully refunded");

            order.status = RefundStatus::Requested;
            emit_event("RefundRequested", metadata![
                "product_id" => product_id.to_string(),
                "order_id" => order_id.to_string(),
                "buyer" => order.buyer.to_string(),
                "reason" => reason,
            ]);
        }

        /// Approves a refund for `order_id`, paying `amount` back out of the product's revenue
        /// vault. Can be called with or without a prior `request_refund` - a merchant may proactively
        /// refund an order the buyer never disputed - and `amount` may be less than the order's full
        /// price for a partial refund.
        ///
        /// Callable by: the product's merchant.
        ///
        /// # Panics
        /// Panics if `amount` exceeds the order's remaining refundable amount (price minus whatever
        /// was already refunded). If the revenue vault doesn't hold enough - e.g. the merchant
        /// already `claim_revenue`d it - this also fails; the buyer's recourse at that point is to
        /// dispute via the registry's stake-backed `resolve_dispute`.
        pub fn approve_refund(&mut self, product_id: u32, order_id: u32, amount: Amount) -> Bucket {
            let product = self.products.get_mut(&product_id).expect("Unknown product");
            let caller = CallerContext::transaction_signer_public_key();
            assert_eq!(caller, product.merchant, "Only the product's merchant can approve a refund");

            let order = product.orders.get_mut(&order_id).expect("Unknown order");
            let remaining = order.amount - order.refunded;
            assert!(amount > Amount::ZERO, "Refund amount must be greater than zero");
            assert!(amount <= remaining, "Refund exceeds the order's remaining refundable amount");

            order.refunded = order.refunded + amount;
            order.status = RefundStatus::None;
            let refund_bucket = product.revenue.withdraw(amount);

            emit_event("OrderRefunded", metadata![
                "product_id" => product_id.to_string(),
                "order_id" => order_id.to_string(),
                "buyer" => order.buyer.to_string(),
                "amount" => amount.to_string(),
                "total_refunded" => order.refunded.to_string(),
            ]);
            refund_bucket
        }

        /// Denies a pending refund request, recording `reason` on-chain - the buyer's evidence trail
        /// for escalating to the platform arbiter, since the merchant's own denial reason is now
        /// public too.
        ///
        /// Callable by: the product's merchant.
        ///
        /// # Panics
        /// Panics if there is no pending (`Requested`) refund for `order_id`.
        pub fn deny_refund(&mut self, product_id: u32, order_id: u32, reason: String) {
            let product = self.products.get_mut(&product_id).expect("Unknown product");
            let caller = CallerContext::transaction_signer_public_key();
            assert_eq!(caller, product.merchant, "Only the product's merchant can deny a refund");

            let order = product.orders.get_mut(&order_id).expect("Unknown order");
            assert_eq!(order.status, RefundStatus::Requested, "No pending refund request for this order");
            order.status = RefundStatus::Denied;

            emit_event("RefundDenied", metadata![
                "product_id" => product_id.to_string(),
                "order_id" => order_id.to_string(),
                "buyer" => order.buyer.to_string(),
                "reason" => reason,
            ]);
        }

        /// Returns `(buyer, amount, refunded so far, refund status)` for `order_id` of `product_id`.
        pub fn get_order_info(&self, product_id: u32, order_id: u32) -> (RistrettoPublicKeyBytes, Amount, Amount, String) {
            let product = self.products.get(&product_id).expect("Unknown product");
            let order = product.orders.get(&order_id).expect("Unknown order");
            (order.buyer, order.amount, order.refunded, refund_status_name(order.status).to_string())
        }
    }
}
