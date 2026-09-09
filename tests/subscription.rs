//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_crypto::ristretto::RistrettoSecretKey;
use tari_ootle_transaction::args;
use tari_template_lib::types::{Amount, ComponentAddress, constants::TARI_TOKEN, crypto::RistrettoPublicKeyBytes};
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason};

const REGISTRY_TEMPLATE_NAME: &str = "MerchantRegistry";
const SUBSCRIPTION_TEMPLATE_NAME: &str = "SubscriptionManager";
const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");
const MIN_STAKE: u64 = 1000;
const PRICE: u64 = 50;

fn setup() -> (TemplateTest, ComponentAddress, ComponentAddress) {
    let mut test = TemplateTest::new(CRATE_PATH, vec![
        "tests/templates/subscription",
        "tests/templates/merchant_registry",
    ]);
    let registry_template = test.get_template_address(REGISTRY_TEMPLATE_NAME);
    let subscription_template = test.get_template_address(SUBSCRIPTION_TEMPLATE_NAME);

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

    test.execute_expect_success(
        test.transaction()
            .call_function(subscription_template, "new", args![registry])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    let (subs, _) = test
        .read_only_state_store()
        .get_components_by_template_address(subscription_template)
        .unwrap()
        .remove(0);

    (test, registry, subs)
}

fn register_merchant(
    test: &mut TemplateTest,
    registry: ComponentAddress,
    account: ComponentAddress,
    secret: &RistrettoSecretKey,
) {
    test.execute_expect_success(
        test.transaction()
            .call_method(account, "withdraw", args![TARI_TOKEN, Amount::from(MIN_STAKE)])
            .put_last_instruction_output_on_workspace("stake")
            .call_method(registry, "register", args![Workspace("stake")])
            .put_last_instruction_output_on_workspace("badge")
            .call_method(account, "deposit", args![Workspace("badge")])
            .build_and_seal(secret),
        vec![],
    );
}

fn create_plan(test: &mut TemplateTest, subs: ComponentAddress, merchant_pk: RistrettoPublicKeyBytes) -> u32 {
    test.call_method::<u32>(subs, "create_plan", args![merchant_pk, Amount::from(PRICE), TARI_TOKEN], vec![])
}

/// First-time subscription: pays for period 1 and deposits the returned membership badge into the
/// payer's own account. The subscriber's identity is simply whoever signs - no secret involved.
fn subscribe(test: &mut TemplateTest, subs: ComponentAddress, plan_id: u32, payer_account: ComponentAddress, payer_secret: &RistrettoSecretKey) {
    test.execute_expect_success(
        test.transaction()
            .call_method(payer_account, "withdraw", args![TARI_TOKEN, Amount::from(PRICE)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(subs, "subscribe", args![plan_id, Workspace("payment")])
            .put_last_instruction_output_on_workspace("badge")
            .call_method(payer_account, "deposit", args![Workspace("badge")])
            .build_and_seal(payer_secret),
        vec![],
    );
}

/// Renews an existing subscription for one more period. No bucket is returned.
fn renew(test: &mut TemplateTest, subs: ComponentAddress, plan_id: u32, payer_account: ComponentAddress, payer_secret: &RistrettoSecretKey) {
    test.execute_expect_success(
        test.transaction()
            .call_method(payer_account, "withdraw", args![TARI_TOKEN, Amount::from(PRICE)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(subs, "renew", args![plan_id, Workspace("payment")])
            .build_and_seal(payer_secret),
        vec![],
    );
}

#[test]
fn subscribe_with_wrong_amount_is_rejected() {
    let (mut test, registry, subs) = setup();
    let (merchant, merchant_proof, merchant_secret) = test.create_funded_account();
    let merchant_pk = merchant_proof.to_public_key().unwrap();
    register_merchant(&mut test, registry, merchant, &merchant_secret);
    let plan_id = create_plan(&mut test, subs, merchant_pk);

    let (payer, _proof, payer_secret) = test.create_funded_account();

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_method(payer, "withdraw", args![TARI_TOKEN, Amount::from(PRICE - 1)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(subs, "subscribe", args![plan_id, Workspace("payment")])
            .build_and_seal(&payer_secret),
        vec![],
    );
    assert_reject_reason(reason, "Payment must equal the plan price exactly");
}

#[test]
fn subscribe_then_active_until_period_advances() {
    let (mut test, registry, subs) = setup();
    let (merchant, merchant_proof, merchant_secret) = test.create_funded_account();
    let merchant_pk = merchant_proof.to_public_key().unwrap();
    register_merchant(&mut test, registry, merchant, &merchant_secret);
    let plan_id = create_plan(&mut test, subs, merchant_pk);

    let (payer, payer_proof, payer_secret) = test.create_funded_account();
    let payer_pk = payer_proof.to_public_key().unwrap();

    subscribe(&mut test, subs, plan_id, payer, &payer_secret);
    assert!(test.call_method::<bool>(subs, "is_active", args![plan_id, payer_pk], vec![]));

    test.execute_expect_success(
        test.transaction()
            .call_method(subs, "advance_period", args![plan_id])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    assert!(!test.call_method::<bool>(subs, "is_active", args![plan_id, payer_pk], vec![]));

    renew(&mut test, subs, plan_id, payer, &payer_secret);
    assert!(test.call_method::<bool>(subs, "is_active", args![plan_id, payer_pk], vec![]));
}

#[test]
fn same_account_renews_across_periods_and_other_accounts_stay_unaffected() {
    let (mut test, registry, subs) = setup();
    let (merchant, merchant_proof, merchant_secret) = test.create_funded_account();
    let merchant_pk = merchant_proof.to_public_key().unwrap();
    register_merchant(&mut test, registry, merchant, &merchant_secret);
    let plan_id = create_plan(&mut test, subs, merchant_pk);

    let (payer_a, proof_a, secret_a) = test.create_funded_account();
    let payer_a_pk = proof_a.to_public_key().unwrap();
    let (payer_b, proof_b, _secret_b) = test.create_funded_account();
    let payer_b_pk = proof_b.to_public_key().unwrap();
    assert_ne!(payer_a, payer_b, "sanity check: these must be different accounts");

    subscribe(&mut test, subs, plan_id, payer_a, &secret_a);
    assert!(test.call_method::<bool>(subs, "is_active", args![plan_id, payer_a_pk], vec![]));
    // B never subscribed, so B is not active even though the plan has an active subscriber.
    assert!(!test.call_method::<bool>(subs, "is_active", args![plan_id, payer_b_pk], vec![]));

    test.execute_expect_success(
        test.transaction()
            .call_method(subs, "advance_period", args![plan_id])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    assert!(!test.call_method::<bool>(subs, "is_active", args![plan_id, payer_a_pk], vec![]));

    // Only A's own account can renew A's subscription.
    renew(&mut test, subs, plan_id, payer_a, &secret_a);
    assert!(test.call_method::<bool>(subs, "is_active", args![plan_id, payer_a_pk], vec![]));
}

#[test]
fn renewing_from_a_different_account_is_rejected() {
    let (mut test, registry, subs) = setup();
    let (merchant, merchant_proof, merchant_secret) = test.create_funded_account();
    let merchant_pk = merchant_proof.to_public_key().unwrap();
    register_merchant(&mut test, registry, merchant, &merchant_secret);
    let plan_id = create_plan(&mut test, subs, merchant_pk);

    let (payer_a, _proof_a, secret_a) = test.create_funded_account();
    subscribe(&mut test, subs, plan_id, payer_a, &secret_a);

    let (payer_b, _proof_b, secret_b) = test.create_funded_account();
    let reason = test.execute_expect_failure(
        test.transaction()
            .call_method(payer_b, "withdraw", args![TARI_TOKEN, Amount::from(PRICE)])
            .put_last_instruction_output_on_workspace("payment")
            .call_method(subs, "renew", args![plan_id, Workspace("payment")])
            .build_and_seal(&secret_b),
        vec![],
    );
    assert_reject_reason(reason, "No existing subscription");
}
