//   Copyright 2026
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_transaction::args;
use tari_template_lib::types::{Amount, ComponentAddress, constants::TARI_TOKEN, crypto::RistrettoPublicKeyBytes};
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason};

const REGISTRY_TEMPLATE_NAME: &str = "MerchantRegistry";
const PRIVATE_PAY_TEMPLATE_NAME: &str = "PrivatePay";
const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");
const MIN_STAKE: u64 = 1000;

/// Basic-tier minimum platform fee, must match `private_pay`'s `BASIC_MIN_FEE`.
const BASIC_MIN_FEE: u64 = 3;

struct Setup {
    test: TemplateTest,
    registry: ComponentAddress,
    private_pay: ComponentAddress,
}

fn setup() -> Setup {
    let mut test = TemplateTest::new(CRATE_PATH, vec!["tests/templates/private_pay", "tests/templates/merchant_registry"]);

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

    let pay_template = test.get_template_address(PRIVATE_PAY_TEMPLATE_NAME);
    test.execute_expect_success(
        test.transaction()
            .call_function(pay_template, "new", args![TARI_TOKEN, registry])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    let (private_pay, _) = test
        .read_only_state_store()
        .get_components_by_template_address(pay_template)
        .unwrap()
        .remove(0);

    Setup {
        test,
        registry,
        private_pay,
    }
}

fn register_merchant(setup: &mut Setup) -> RistrettoPublicKeyBytes {
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
    merchant_pk
}

/// Builds (but does not execute) a transaction that funds a fresh payer account and pays
/// `merchant` a platform fee of `fee_amount`. In a real deployment this `pay` call would share a
/// transaction with a sibling native `StealthTransfer` instruction moving the actual hidden
/// payment - not exercised here since that instruction is independent of, and never inspected by,
/// this contract.
fn pay_transaction(setup: &mut Setup, merchant_pk: RistrettoPublicKeyBytes, fee_amount: u64) -> (tari_ootle_transaction::Transaction, tari_crypto::ristretto::RistrettoSecretKey) {
    let (payer, _proof, payer_secret) = setup.test.create_funded_account();
    let transaction = setup
        .test
        .transaction()
        .call_method(payer, "withdraw", args![TARI_TOKEN, Amount::from(fee_amount)])
        .put_last_instruction_output_on_workspace("fee")
        .call_method(setup.private_pay, "pay", args![Workspace("fee"), merchant_pk])
        .build_and_seal(&payer_secret);
    (transaction, payer_secret)
}

#[test]
fn valid_payment_collects_the_fee_and_hides_everything_else() {
    let mut setup = setup();
    let merchant_pk = register_merchant(&mut setup);

    let (transaction, _) = pay_transaction(&mut setup, merchant_pk, BASIC_MIN_FEE);
    setup.test.execute_expect_success(transaction, vec![]);

    assert_eq!(
        setup
            .test
            .call_method::<Amount>(setup.private_pay, "fee_pot_balance", args![], vec![]),
        Amount::from(BASIC_MIN_FEE)
    );
}

#[test]
fn fee_below_the_tier_minimum_is_rejected() {
    let mut setup = setup();
    let merchant_pk = register_merchant(&mut setup);

    let (transaction, _) = pay_transaction(&mut setup, merchant_pk, BASIC_MIN_FEE - 1);
    let reason = setup.test.execute_expect_failure(transaction, vec![]);
    assert_reject_reason(reason, "Platform fee is below the minimum for this merchant's tier");
}

#[test]
fn payment_to_an_unregistered_merchant_is_rejected() {
    let mut setup = setup();
    let not_a_merchant = setup.test.to_public_key_bytes();

    let (transaction, _) = pay_transaction(&mut setup, not_a_merchant, BASIC_MIN_FEE);
    let reason = setup.test.execute_expect_failure(transaction, vec![]);
    assert_reject_reason(reason, "Merchant is not registered");
}
