//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_crypto::ristretto::RistrettoSecretKey;
use tari_ootle_transaction::args;
use tari_template_lib::types::{Amount, ComponentAddress, constants::TARI_TOKEN, crypto::RistrettoPublicKeyBytes};
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason};

const TEMPLATE_PATHS: &[&str] = &["tests/templates/merchant_registry"];
const TEMPLATE_NAME: &str = "MerchantRegistry";
const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");
const MIN_STAKE: u64 = 1000;

fn setup() -> (TemplateTest, ComponentAddress) {
    let mut test = TemplateTest::new(CRATE_PATH, TEMPLATE_PATHS);
    let template = test.get_template_address(TEMPLATE_NAME);

    test.execute_expect_success(
        test.transaction()
            .call_function(template, "new", args![TARI_TOKEN, Amount::from(MIN_STAKE)])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    let (registry, _) = test
        .read_only_state_store()
        .get_components_by_template_address(template)
        .unwrap()
        .remove(0);

    (test, registry)
}

fn register(
    test: &mut TemplateTest,
    registry: ComponentAddress,
    account: ComponentAddress,
    secret: &RistrettoSecretKey,
    stake: u64,
) {
    test.execute_expect_success(
        test.transaction()
            .call_method(account, "withdraw", args![TARI_TOKEN, Amount::from(stake)])
            .put_last_instruction_output_on_workspace("stake")
            .call_method(registry, "register", args![Workspace("stake")])
            .put_last_instruction_output_on_workspace("badge")
            .call_method(account, "deposit", args![Workspace("badge")])
            .build_and_seal(secret),
        vec![],
    );
}

fn add_stake(
    test: &mut TemplateTest,
    registry: ComponentAddress,
    account: ComponentAddress,
    secret: &RistrettoSecretKey,
    stake: u64,
) {
    test.execute_expect_success(
        test.transaction()
            .call_method(account, "withdraw", args![TARI_TOKEN, Amount::from(stake)])
            .put_last_instruction_output_on_workspace("stake")
            .call_method(registry, "add_stake", args![Workspace("stake")])
            .build_and_seal(secret),
        vec![],
    );
}

#[test]
fn register_mints_a_badge_and_assigns_basic_tier() {
    let (mut test, registry) = setup();
    let (merchant, proof, merchant_secret) = test.create_funded_account();
    let merchant_pk: RistrettoPublicKeyBytes = proof.to_public_key().unwrap();

    register(&mut test, registry, merchant, &merchant_secret, MIN_STAKE);

    assert!(test.call_method::<bool>(registry, "is_registered", args![merchant_pk], vec![]));
    assert_eq!(
        test.call_method::<String>(registry, "get_tier", args![merchant_pk], vec![]),
        "Basic"
    );
    let (staked, tier, active) =
        test.call_method::<(Amount, String, bool)>(registry, "get_merchant_info", args![merchant_pk], vec![]);
    assert_eq!(staked, Amount::from(MIN_STAKE));
    assert_eq!(tier, "Basic");
    assert!(active);
}

#[test]
fn stake_below_minimum_is_rejected() {
    let (mut test, registry) = setup();
    let (merchant, _proof, merchant_secret) = test.create_funded_account();

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_method(merchant, "withdraw", args![TARI_TOKEN, Amount::from(MIN_STAKE - 1)])
            .put_last_instruction_output_on_workspace("stake")
            .call_method(registry, "register", args![Workspace("stake")])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    assert_reject_reason(reason, "Stake below minimum");
}

#[test]
fn adding_stake_upgrades_tier() {
    let (mut test, registry) = setup();
    let (merchant, proof, merchant_secret) = test.create_funded_account();
    let merchant_pk: RistrettoPublicKeyBytes = proof.to_public_key().unwrap();

    register(&mut test, registry, merchant, &merchant_secret, MIN_STAKE);
    assert_eq!(
        test.call_method::<String>(registry, "get_tier", args![merchant_pk], vec![]),
        "Basic"
    );

    add_stake(&mut test, registry, merchant, &merchant_secret, MIN_STAKE * 4);
    assert_eq!(
        test.call_method::<String>(registry, "get_tier", args![merchant_pk], vec![]),
        "Standard"
    );

    add_stake(&mut test, registry, merchant, &merchant_secret, MIN_STAKE * 15);
    assert_eq!(
        test.call_method::<String>(registry, "get_tier", args![merchant_pk], vec![]),
        "Premium"
    );
}

#[test]
fn request_exit_then_finalize_returns_the_stake() {
    let (mut test, registry) = setup();
    let (merchant, proof, merchant_secret) = test.create_funded_account();
    let merchant_pk: RistrettoPublicKeyBytes = proof.to_public_key().unwrap();

    register(&mut test, registry, merchant, &merchant_secret, MIN_STAKE);

    test.execute_expect_success(
        test.transaction()
            .call_method(registry, "request_exit", args![])
            .build_and_seal(&merchant_secret),
        vec![],
    );
    assert!(!test.call_method::<bool>(registry, "is_registered", args![merchant_pk], vec![]));

    let balance_before = test.call_method::<Amount>(merchant, "balance", args![TARI_TOKEN], vec![]);
    test.execute_expect_success(
        test.transaction()
            .call_method(registry, "finalize_exit", args![merchant_pk])
            .put_last_instruction_output_on_workspace("returned_stake")
            .call_method(merchant, "deposit", args![Workspace("returned_stake")])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    let balance_after = test.call_method::<Amount>(merchant, "balance", args![TARI_TOKEN], vec![]);
    assert_eq!(balance_after, balance_before + Amount::from(MIN_STAKE));
}

#[test]
fn slash_reduces_stake_and_can_downgrade_tier() {
    let (mut test, registry) = setup();
    let (merchant, proof, merchant_secret) = test.create_funded_account();
    let merchant_pk: RistrettoPublicKeyBytes = proof.to_public_key().unwrap();

    register(&mut test, registry, merchant, &merchant_secret, MIN_STAKE * 5);
    assert_eq!(
        test.call_method::<String>(registry, "get_tier", args![merchant_pk], vec![]),
        "Standard"
    );

    test.execute_expect_success(
        test.transaction()
            .call_method(registry, "slash", args![merchant_pk, Amount::from(MIN_STAKE * 4)])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    let (staked, tier, _active) =
        test.call_method::<(Amount, String, bool)>(registry, "get_merchant_info", args![merchant_pk], vec![]);
    assert_eq!(staked, Amount::from(MIN_STAKE));
    assert_eq!(tier, "Basic");
}
