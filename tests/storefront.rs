//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_crypto::ristretto::RistrettoSecretKey;
use tari_ootle_transaction::args;
use tari_template_lib::types::{Amount, ComponentAddress, constants::TARI_TOKEN, crypto::RistrettoPublicKeyBytes};
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason};

const REGISTRY_TEMPLATE_NAME: &str = "MerchantRegistry";
const STOREFRONT_TEMPLATE_NAME: &str = "Storefront";
const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");
const MIN_STAKE: u64 = 1000;
const PRICE: u64 = 20;

struct Setup {
    test: TemplateTest,
    registry: ComponentAddress,
    storefront: ComponentAddress,
}

fn setup() -> Setup {
    let mut test = TemplateTest::new(CRATE_PATH, vec!["tests/templates/storefront", "tests/templates/merchant_registry"]);

    let registry_template = test.get_template_address(REGISTRY_TEMPLATE_NAME);
    test.execute_expect_success(
        test.transaction()
            .call_function(registry_template, "new", args![TARI_TOKEN, Amount::from(MIN_STAKE)])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    let (registry, _) = test
        .read_only_state_store()
        .get_components_by_template_address(registry_template)
        .unwrap()
        .remove(0);

    let pay_template = test.get_template_address(STOREFRONT_TEMPLATE_NAME);
    test.execute_expect_success(
        test.transaction()
            .call_function(pay_template, "new", args![registry])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    let (storefront, _) = test
        .read_only_state_store()
        .get_components_by_template_address(pay_template)
        .unwrap()
        .remove(0);

    Setup {
        test,
        registry,
        storefront,
    }
}

fn register_merchant(setup: &mut Setup) -> (ComponentAddress, RistrettoSecretKey, RistrettoPublicKeyBytes) {
    let (merchant, proof, merchant_secret) = setup.test.create_funded_account();
    let merchant_pk = proof.to_public_key().unwrap();
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(merchant, "withdraw", args![TARI_TOKEN, Amount::from(MIN_STAKE)])
            .put_last_instruction_output_on_workspace("stake")
            .call_method(setup.registry, "register", args![Workspace("stake")])
            .put_last_instruction_output_on_workspace("badge")
            .call_method(merchant, "deposit", args![Workspace("badge")])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    (merchant, merchant_secret, merchant_pk)
}

fn create_product(setup: &mut Setup, merchant_pk: RistrettoPublicKeyBytes) -> u32 {
    setup.test.call_method::<u32>(
        setup.storefront,
        "create_product",
        args![merchant_pk, "Test Product".to_string(), Amount::from(PRICE), TARI_TOKEN],
        vec![],
    )
}

/// Buys `product_id` from a fresh, throwaway funded account, paying `amount`. Never records that
/// account's identity anywhere in the contract.
fn buy(setup: &mut Setup, product_id: u32, amount: u64) {
    let (payer, _proof, payer_secret) = setup.test.create_funded_account();
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(payer, "withdraw", args![TARI_TOKEN, Amount::from(amount)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(setup.storefront, "buy", args![product_id, Workspace("payment")])
            .build_and_seal(&payer_secret),
        vec![],
    );
}

/// Buys `product_id` from a fresh, funded account, returning that account/secret/pk so a refund
/// flow can be driven from the same buyer afterwards.
fn buy_and_return_buyer(
    setup: &mut Setup,
    product_id: u32,
    amount: u64,
) -> (ComponentAddress, RistrettoSecretKey, RistrettoPublicKeyBytes) {
    let (payer, proof, payer_secret) = setup.test.create_funded_account();
    let payer_pk = proof.to_public_key().unwrap();
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(payer, "withdraw", args![TARI_TOKEN, Amount::from(amount)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(setup.storefront, "buy", args![product_id, Workspace("payment")])
            .build_and_seal(&payer_secret),
        vec![],
    );
    (payer, payer_secret, payer_pk)
}

fn buy_expect_failure(setup: &mut Setup, product_id: u32, amount: u64) -> String {
    let (payer, _proof, payer_secret) = setup.test.create_funded_account();
    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(payer, "withdraw", args![TARI_TOKEN, Amount::from(amount)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(setup.storefront, "buy", args![product_id, Workspace("payment")])
            .build_and_seal(&payer_secret),
        vec![],
    );
    format!("{reason:?}")
}

#[test]
fn creating_a_product_for_an_unregistered_merchant_is_rejected() {
    let mut setup = setup();
    let not_a_merchant = setup.test.to_public_key_bytes();

    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(
                setup.storefront,
                "create_product",
                args![not_a_merchant, "Test Product".to_string(), Amount::from(PRICE), TARI_TOKEN],
            )
            .build_and_seal(setup.test.secret_key()),
        vec![],
    );
    assert_reject_reason(reason, "Merchant is not registered");
}

#[test]
fn registered_merchants_product_can_be_created_and_bought() {
    let mut setup = setup();
    let (_, _, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);

    buy(&mut setup, product_id, PRICE);

    let (price, order_count, revenue) =
        setup
            .test
            .call_method::<(Amount, u32, Amount)>(setup.storefront, "get_product_info", args![product_id], vec![]);
    assert_eq!(price, Amount::from(PRICE));
    assert_eq!(order_count, 1);
    assert_eq!(revenue, Amount::from(PRICE));
}

#[test]
fn wrong_payment_amount_is_rejected() {
    let mut setup = setup();
    let (_, _, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);

    let reason = buy_expect_failure(&mut setup, product_id, PRICE - 1);
    assert!(
        reason.contains("Payment must equal the product price exactly"),
        "unexpected reject reason: {reason}"
    );
}

#[test]
fn buying_twice_from_different_accounts_accumulates_order_count_and_revenue_and_records_each_buyer() {
    let mut setup = setup();
    let (_, _, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);

    // Two purchases, from two different accounts - each order should record its own buyer.
    buy(&mut setup, product_id, PRICE);
    let (payer_b, _proof_b, payer_b_secret) = setup.test.create_funded_account();
    let payer_b_pk = _proof_b.to_public_key().unwrap();
    let result = setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(payer_b, "withdraw", args![TARI_TOKEN, Amount::from(PRICE)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(setup.storefront, "buy", args![product_id, Workspace("payment")])
            .build_and_seal(&payer_b_secret),
        vec![],
    );

    let (_, order_count, revenue) =
        setup
            .test
            .call_method::<(Amount, u32, Amount)>(setup.storefront, "get_product_info", args![product_id], vec![]);
    assert_eq!(order_count, 2);
    assert_eq!(revenue, Amount::from(PRICE * 2));

    // The second purchase's own event correctly records the second (different) buyer.
    let event = result
        .finalize
        .events
        .iter()
        .find(|e| e.topic() == "Storefront.OrderPlaced")
        .expect("OrderPlaced event not found");
    assert_eq!(event.get_payload("product_id").unwrap(), product_id.to_string());
    assert_eq!(event.get_payload("merchant").unwrap(), merchant_pk.to_string());
    assert_eq!(event.get_payload("buyer").unwrap(), payer_b_pk.to_string());
    assert_eq!(event.get_payload("amount").unwrap(), PRICE.to_string());
    assert_eq!(event.get_payload("order_number").unwrap(), "2");
}

#[test]
fn buying_after_the_merchant_requests_exit_is_rejected() {
    let mut setup = setup();
    let (_merchant_account, merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);

    // Sanity check: the product is sellable before the merchant requests exit.
    buy(&mut setup, product_id, PRICE);

    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.registry, "request_exit", args![])
            .build_and_seal(&merchant_secret),
        vec![],
    );

    let reason = buy_expect_failure(&mut setup, product_id, PRICE);
    assert!(
        reason.contains("Merchant is no longer registered"),
        "unexpected reject reason: {reason}"
    );
}

#[test]
fn claim_revenue_by_a_non_merchant_is_rejected() {
    let mut setup = setup();
    let (_, _, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    buy(&mut setup, product_id, PRICE);

    let (impostor, _proof, impostor_secret) = setup.test.create_funded_account();
    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "claim_revenue", args![product_id])
            .put_last_instruction_output_on_workspace("revenue")
            .call_method(impostor, "deposit", args![Workspace("revenue")])
            .build_and_seal(&impostor_secret),
        vec![],
    );
    assert_reject_reason(reason, "Only the product's merchant can claim its revenue");
}

#[test]
fn merchant_can_claim_revenue_and_a_second_claim_drains_nothing_more() {
    let mut setup = setup();
    let (merchant_account, merchant_proof, merchant_secret) = setup.test.create_funded_account();
    let merchant_pk = merchant_proof.to_public_key().unwrap();
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(merchant_account, "withdraw", args![TARI_TOKEN, Amount::from(MIN_STAKE)])
            .put_last_instruction_output_on_workspace("stake")
            .call_method(setup.registry, "register", args![Workspace("stake")])
            .put_last_instruction_output_on_workspace("badge")
            .call_method(merchant_account, "deposit", args![Workspace("badge")])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    let product_id = create_product(&mut setup, merchant_pk);
    buy(&mut setup, product_id, PRICE);

    let balance_before = setup
        .test
        .call_method::<Amount>(merchant_account, "balance", args![TARI_TOKEN], vec![]);

    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "claim_revenue", args![product_id])
            .put_last_instruction_output_on_workspace("revenue")
            .call_method(merchant_account, "deposit", args![Workspace("revenue")])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    let balance_after = setup
        .test
        .call_method::<Amount>(merchant_account, "balance", args![TARI_TOKEN], vec![]);
    assert_eq!(balance_after, balance_before + Amount::from(PRICE));

    let (_, _, revenue_after_claim) =
        setup
            .test
            .call_method::<(Amount, u32, Amount)>(setup.storefront, "get_product_info", args![product_id], vec![]);
    assert_eq!(revenue_after_claim, Amount::ZERO);

    // A second claim withdraws an empty (zero-balance) bucket rather than failing.
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "claim_revenue", args![product_id])
            .put_last_instruction_output_on_workspace("revenue")
            .call_method(merchant_account, "deposit", args![Workspace("revenue")])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    let balance_after_second_claim = setup
        .test
        .call_method::<Amount>(merchant_account, "balance", args![TARI_TOKEN], vec![]);
    assert_eq!(balance_after_second_claim, balance_after);
}

#[test]
fn buyer_can_request_a_refund_and_merchant_can_approve_it() {
    let mut setup = setup();
    let (_, merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (buyer, buyer_secret, _) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "request_refund", args![
                product_id,
                order_id,
                "Wrong size".to_string()
            ])
            .build_and_seal(&buyer_secret),
        vec![],
    );

    let balance_before = setup.test.call_method::<Amount>(buyer, "balance", args![TARI_TOKEN], vec![]);
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "approve_refund", args![product_id, order_id, Amount::from(PRICE)])
            .put_last_instruction_output_on_workspace("refund")
            .call_method(buyer, "deposit", args![Workspace("refund")])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    let balance_after = setup.test.call_method::<Amount>(buyer, "balance", args![TARI_TOKEN], vec![]);
    assert_eq!(balance_after, balance_before + Amount::from(PRICE));

    let (_, amount, refunded, status) = setup.test.call_method::<(RistrettoPublicKeyBytes, Amount, Amount, String)>(
        setup.storefront,
        "get_order_info",
        args![product_id, order_id],
        vec![],
    );
    assert_eq!(amount, Amount::from(PRICE));
    assert_eq!(refunded, Amount::from(PRICE));
    assert_eq!(status, "None");

    let (_, _, revenue_after) =
        setup
            .test
            .call_method::<(Amount, u32, Amount)>(setup.storefront, "get_product_info", args![product_id], vec![]);
    assert_eq!(revenue_after, Amount::ZERO);
}

#[test]
fn merchant_can_deny_a_refund_request_with_a_reason() {
    let mut setup = setup();
    let (_, merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (_, buyer_secret, buyer_pk) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "request_refund", args![
                product_id,
                order_id,
                "Item never arrived".to_string()
            ])
            .build_and_seal(&buyer_secret),
        vec![],
    );

    let result = setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "deny_refund", args![
                product_id,
                order_id,
                "Tracking shows delivered".to_string()
            ])
            .build_and_seal(&merchant_secret),
        vec![],
    );

    let event = result
        .finalize
        .events
        .iter()
        .find(|e| e.topic() == "Storefront.RefundDenied")
        .expect("RefundDenied event not found");
    assert_eq!(event.get_payload("buyer").unwrap(), buyer_pk.to_string());
    assert_eq!(event.get_payload("reason").unwrap(), "Tracking shows delivered");

    let (_, _, _, status) = setup.test.call_method::<(RistrettoPublicKeyBytes, Amount, Amount, String)>(
        setup.storefront,
        "get_order_info",
        args![product_id, order_id],
        vec![],
    );
    assert_eq!(status, "Denied");
}

#[test]
fn merchant_can_approve_a_refund_without_a_prior_request() {
    let mut setup = setup();
    let (_, merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (buyer, _buyer_secret, _) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    // No request_refund call at all - the merchant proactively refunds.
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "approve_refund", args![product_id, order_id, Amount::from(PRICE)])
            .put_last_instruction_output_on_workspace("refund")
            .call_method(buyer, "deposit", args![Workspace("refund")])
            .build_and_seal(&merchant_secret),
        vec![],
    );

    let (_, _, refunded, _) = setup.test.call_method::<(RistrettoPublicKeyBytes, Amount, Amount, String)>(
        setup.storefront,
        "get_order_info",
        args![product_id, order_id],
        vec![],
    );
    assert_eq!(refunded, Amount::from(PRICE));
}

#[test]
fn approving_more_than_the_remaining_refundable_amount_is_rejected() {
    let mut setup = setup();
    let (_, merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (_, _buyer_secret, _) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "approve_refund", args![
                product_id,
                order_id,
                Amount::from(PRICE + 1)
            ])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    assert_reject_reason(reason, "Refund exceeds the order's remaining refundable amount");
}

#[test]
fn only_the_buyer_can_request_a_refund() {
    let mut setup = setup();
    let (_, _merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (_, _buyer_secret, _) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    let (_impostor, _proof, impostor_secret) = setup.test.create_funded_account();
    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "request_refund", args![
                product_id,
                order_id,
                "not mine".to_string()
            ])
            .build_and_seal(&impostor_secret),
        vec![],
    );
    assert_reject_reason(reason, "Only the order's buyer can request a refund");
}

#[test]
fn only_the_merchant_can_approve_or_deny_a_refund() {
    let mut setup = setup();
    let (_, _merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (_, _buyer_secret, _) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    let (_impostor, _proof, impostor_secret) = setup.test.create_funded_account();
    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "approve_refund", args![product_id, order_id, Amount::from(PRICE)])
            .build_and_seal(&impostor_secret),
        vec![],
    );
    assert_reject_reason(reason, "Only the product's merchant can approve a refund");

    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "deny_refund", args![product_id, order_id, "nope".to_string()])
            .build_and_seal(&impostor_secret),
        vec![],
    );
    assert_reject_reason(reason, "Only the product's merchant can deny a refund");
}

#[test]
fn denying_without_a_pending_refund_request_is_rejected() {
    let mut setup = setup();
    let (_, merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (_, _buyer_secret, _) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "deny_refund", args![product_id, order_id, "nothing pending".to_string()])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    assert_reject_reason(reason, "No pending refund request for this order");
}

#[test]
fn partial_refunds_accumulate_and_a_second_full_refund_is_capped() {
    let mut setup = setup();
    let (_, merchant_secret, merchant_pk) = register_merchant(&mut setup);
    let product_id = create_product(&mut setup, merchant_pk);
    let (buyer, _buyer_secret, _) = buy_and_return_buyer(&mut setup, product_id, PRICE);
    let order_id = 1u32;

    // Refund half.
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "approve_refund", args![
                product_id,
                order_id,
                Amount::from(PRICE / 2)
            ])
            .put_last_instruction_output_on_workspace("refund")
            .call_method(buyer, "deposit", args![Workspace("refund")])
            .build_and_seal(&merchant_secret),
        vec![],
    );

    // Trying to refund the full original price again overruns what's left.
    let reason = setup.test.execute_expect_failure(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "approve_refund", args![product_id, order_id, Amount::from(PRICE)])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    assert_reject_reason(reason, "Refund exceeds the order's remaining refundable amount");

    // But refunding exactly what's left works.
    setup.test.execute_expect_success(
        setup
            .test
            .transaction()
            .call_method(setup.storefront, "approve_refund", args![
                product_id,
                order_id,
                Amount::from(PRICE - PRICE / 2)
            ])
            .put_last_instruction_output_on_workspace("refund")
            .call_method(buyer, "deposit", args![Workspace("refund")])
            .build_and_seal(&merchant_secret),
        vec![],
    );

    let (_, amount, refunded, _) = setup.test.call_method::<(RistrettoPublicKeyBytes, Amount, Amount, String)>(
        setup.storefront,
        "get_order_info",
        args![product_id, order_id],
        vec![],
    );
    assert_eq!(refunded, amount);
}
