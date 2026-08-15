#![cfg(test)]

//! Exhaustive test suite for the arbitrated escrow vault.
//!
//! Covers the two successful lifecycles (release and refund), every
//! authorization boundary (unauthorized deposit, unauthorized release
//! caller, unauthorized refund), and every double-spend / out-of-order
//! edge case (double initialize, double deposit, double release, double
//! refund, release-before-deposit, release-after-refund,
//! refund-after-release, and invalid `initialize` inputs).

use crate::{EscrowVault, EscrowVaultClient, Status};
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

const DEPOSIT_AMOUNT: i128 = 1_000;
const STARTING_BALANCE: i128 = 10_000;

fn create_token_contract<'a>(
    env: &Env,
    admin: &Address,
) -> (TokenClient<'a>, StellarAssetClient<'a>, Address) {
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let address = sac.address();
    (
        TokenClient::new(env, &address),
        StellarAssetClient::new(env, &address),
        address,
    )
}

struct TestCtx<'a> {
    env: Env,
    contract_id: Address,
    client: EscrowVaultClient<'a>,
    token: TokenClient<'a>,
    token_address: Address,
    payer: Address,
    payee: Address,
    arbiter: Address,
    outsider: Address,
}

/// Spins up a fresh contract instance, a fresh test token, funds `payer`
/// with `STARTING_BALANCE`, but does NOT call `initialize` — callers decide
/// whether/how to initialize.
fn setup() -> TestCtx<'static> {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowVault);
    let client = EscrowVaultClient::new(&env, &contract_id);

    let token_admin = Address::generate(&env);
    let (token, token_admin_client, token_address) = create_token_contract(&env, &token_admin);

    let payer = Address::generate(&env);
    let payee = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let outsider = Address::generate(&env);

    token_admin_client.mint(&payer, &STARTING_BALANCE);

    TestCtx {
        env,
        contract_id,
        client,
        token,
        token_address,
        payer,
        payee,
        arbiter,
        outsider,
    }
}

/// Same as `setup`, but also initializes and funds the escrow, leaving it
/// `Pending` with `funded == true`, ready for `release`/`refund` tests.
fn setup_funded() -> TestCtx<'static> {
    let ctx = setup();
    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.arbiter,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );
    ctx.client.deposit();
    ctx
}

// ---------------------------------------------------------------------
// Successful lifecycles
// ---------------------------------------------------------------------

#[test]
fn test_full_lifecycle_release_by_payer() {
    let ctx = setup_funded();

    ctx.client.release(&ctx.payer);

    assert_eq!(ctx.token.balance(&ctx.payee), DEPOSIT_AMOUNT);
    assert_eq!(ctx.token.balance(&ctx.contract_id), 0);
    assert_eq!(ctx.client.get_status(), Status::Completed);
}

#[test]
fn test_full_lifecycle_release_by_arbiter() {
    let ctx = setup_funded();

    ctx.client.release(&ctx.arbiter);

    assert_eq!(ctx.token.balance(&ctx.payee), DEPOSIT_AMOUNT);
    assert_eq!(ctx.client.get_status(), Status::Completed);
}

#[test]
fn test_full_lifecycle_refund_by_arbiter() {
    let ctx = setup_funded();
    let payer_balance_before_refund = ctx.token.balance(&ctx.payer);

    ctx.client.refund();

    assert_eq!(
        ctx.token.balance(&ctx.payer),
        payer_balance_before_refund + DEPOSIT_AMOUNT
    );
    assert_eq!(ctx.token.balance(&ctx.contract_id), 0);
    assert_eq!(ctx.client.get_status(), Status::Refunded);
}

#[test]
fn test_initialize_sets_pending_unfunded_state() {
    let ctx = setup();

    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.arbiter,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );

    let escrow = ctx.client.get_escrow();
    assert_eq!(escrow.payer, ctx.payer);
    assert_eq!(escrow.payee, ctx.payee);
    assert_eq!(escrow.arbiter, ctx.arbiter);
    assert_eq!(escrow.token, ctx.token_address);
    assert_eq!(escrow.amount, DEPOSIT_AMOUNT);
    assert!(!escrow.funded);
    assert_eq!(escrow.status, Status::Pending);
}

// ---------------------------------------------------------------------
// `initialize` input validation
// ---------------------------------------------------------------------

#[test]
#[should_panic]
fn test_initialize_zero_amount_panics() {
    let ctx = setup();
    ctx.client
        .initialize(&ctx.payer, &ctx.payee, &ctx.arbiter, &ctx.token_address, &0);
}

#[test]
#[should_panic]
fn test_initialize_negative_amount_panics() {
    let ctx = setup();
    ctx.client
        .initialize(&ctx.payer, &ctx.payee, &ctx.arbiter, &ctx.token_address, &-1);
}

#[test]
#[should_panic]
fn test_initialize_duplicate_parties_panics() {
    let ctx = setup();
    // arbiter == payer is a conflict of interest and must be rejected.
    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.payer,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );
}

#[test]
#[should_panic]
fn test_double_initialize_panics() {
    let ctx = setup();
    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.arbiter,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );
    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.arbiter,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );
}

#[test]
#[should_panic]
fn test_calls_before_initialize_panic() {
    let ctx = setup();
    // No `initialize` call at all — any state read/mutation must panic.
    ctx.client.deposit();
}

// ---------------------------------------------------------------------
// `deposit` edge cases
// ---------------------------------------------------------------------

#[test]
#[should_panic]
fn test_deposit_unauthorized_panics() {
    let ctx = setup();
    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.arbiter,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );

    // Strip the mocked auths so `escrow.payer.require_auth()` inside
    // `deposit` has nothing authorizing it.
    ctx.env.set_auths(&[]);
    ctx.client.deposit();
}

#[test]
#[should_panic]
fn test_double_deposit_panics() {
    let ctx = setup_funded();
    ctx.client.deposit();
}

// ---------------------------------------------------------------------
// `release` edge cases
// ---------------------------------------------------------------------

#[test]
#[should_panic]
fn test_release_before_deposit_panics() {
    let ctx = setup();
    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.arbiter,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );
    ctx.client.release(&ctx.payer);
}

#[test]
#[should_panic]
fn test_release_by_outsider_panics() {
    let ctx = setup_funded();
    // `outsider` authenticates fine (auths are mocked) but is neither the
    // payer nor the arbiter, so the business-logic authorization check
    // inside `release` must reject it regardless.
    ctx.client.release(&ctx.outsider);
}

#[test]
#[should_panic]
fn test_release_by_payee_panics() {
    let ctx = setup_funded();
    // The payee itself has no authority to trigger a release.
    ctx.client.release(&ctx.payee);
}

#[test]
#[should_panic]
fn test_release_without_caller_auth_panics() {
    let ctx = setup_funded();
    ctx.env.set_auths(&[]);
    ctx.client.release(&ctx.payer);
}

#[test]
#[should_panic]
fn test_double_release_panics() {
    let ctx = setup_funded();
    ctx.client.release(&ctx.payer);
    ctx.client.release(&ctx.arbiter);
}

#[test]
#[should_panic]
fn test_release_after_refund_panics() {
    let ctx = setup_funded();
    ctx.client.refund();
    ctx.client.release(&ctx.payer);
}

// ---------------------------------------------------------------------
// `refund` edge cases
// ---------------------------------------------------------------------

#[test]
#[should_panic]
fn test_refund_before_deposit_panics() {
    let ctx = setup();
    ctx.client.initialize(
        &ctx.payer,
        &ctx.payee,
        &ctx.arbiter,
        &ctx.token_address,
        &DEPOSIT_AMOUNT,
    );
    ctx.client.refund();
}

#[test]
#[should_panic]
fn test_refund_without_arbiter_auth_panics() {
    let ctx = setup_funded();
    // Strip mocked auths so `escrow.arbiter.require_auth()` inside
    // `refund` has nothing authorizing it — simulates the payer (or
    // anyone else) trying to force a refund without the arbiter's consent.
    ctx.env.set_auths(&[]);
    ctx.client.refund();
}

#[test]
#[should_panic]
fn test_double_refund_panics() {
    let ctx = setup_funded();
    ctx.client.refund();
    ctx.client.refund();
}

#[test]
#[should_panic]
fn test_refund_after_release_panics() {
    let ctx = setup_funded();
    ctx.client.release(&ctx.payer);
    ctx.client.refund();
}
