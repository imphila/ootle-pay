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
    }

    impl Storefront {
        pub fn new(registry: ComponentAddress) -> Component<Self> {
            let access_rules = ComponentAccessRules::new()
                .method("create_product", rule!(allow_all))
                .method("buy", rule!(allow_all))
                .method("get_product_info", rule!(allow_all))
                .method("claim_revenue", rule!(allow_all));

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
        /// Callable by: anyone. Records the buyer's public key as part of the order.
        pub fn buy(&mut self, product_id: u32, payment: Bucket) -> u32 {
            let buyer = CallerContext::transaction_signer_public_key();
            let product = self.products.get_mut(&product_id).expect("Unknown product");
            assert_eq!(payment.resource_address(), product.resource, "Wrong payment resource");
            assert_eq!(payment.amount(), product.price, "Payment must equal the product price exactly");

            product.revenue.deposit(payment);
            product.order_count += 1;
            let order_number = product.order_count;

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
    }
}
