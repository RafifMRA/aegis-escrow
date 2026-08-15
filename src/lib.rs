#![no_std]

//! Arbitrated escrow vault for the Stellar network (Soroban).
//!
//! One deployed instance of this contract represents a single escrow deal
//! between a `payer` and a `payee`, mediated by a neutral `arbiter`. The
//! payer funds the vault with a fixed amount of a given token; the funds are
//! then released to the payee (by either the payer or the arbiter) once the
//! agreed condition is met, or refunded back to the payer by the arbiter if
//! the deal falls through.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env,
};

/// Number of ledgers in one day, assuming ~5 second ledger close times.
/// Used to size storage TTL extensions so the contract's state does not
/// expire and get archived while an escrow is still active.
const DAY_IN_LEDGERS: u32 = 17280;
/// How far into the future `extend_ttl` pushes the instance's expiration
/// every time it is called.
const INSTANCE_BUMP_AMOUNT: u32 = 30 * DAY_IN_LEDGERS;
/// The remaining-TTL threshold below which `extend_ttl` will actually bump
/// the expiration ledger (avoids paying to extend on every single call).
const INSTANCE_LIFETIME_THRESHOLD: u32 = INSTANCE_BUMP_AMOUNT - DAY_IN_LEDGERS;

/// Lifecycle status of the escrow.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    /// Escrow has been initialized (and may or may not be funded yet); no
    /// final resolution has occurred.
    Pending,
    /// Funds have been released to the payee. Terminal state.
    Completed,
    /// Funds have been returned to the payer. Terminal state.
    Refunded,
}

/// Full on-chain record for this escrow deal.
#[contracttype]
#[derive(Clone)]
pub struct EscrowData {
    pub payer: Address,
    pub payee: Address,
    pub arbiter: Address,
    pub token: Address,
    pub amount: i128,
    /// True once `deposit` has successfully pulled `amount` of `token` from
    /// `payer` into the contract.
    pub funded: bool,
    pub status: Status,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Escrow,
}

/// Typed error codes returned (via panic) by every failure path in this
/// contract, so callers and tests can assert on precise failure reasons
/// instead of opaque panics.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum EscrowError {
    /// `initialize` was called on a contract instance that already has an
    /// escrow set up.
    AlreadyInitialized = 1,
    /// A state-mutating or read function was called before `initialize`.
    NotInitialized = 2,
    /// `amount` supplied to `initialize` was zero or negative.
    InvalidAmount = 3,
    /// `payer`, `payee`, and `arbiter` were not all distinct addresses.
    InvalidParties = 4,
    /// `deposit` was called on an escrow that has already been funded.
    AlreadyFunded = 5,
    /// `release` or `refund` was called before the escrow was funded.
    NotFunded = 6,
    /// `release` or `refund` was called on an escrow that is no longer
    /// `Pending` (i.e. already `Completed` or `Refunded`).
    NotPending = 7,
    /// `release` was called by an address that is neither the payer nor
    /// the arbiter.
    Unauthorized = 8,
}

#[contract]
pub struct EscrowVault;

#[contractimpl]
impl EscrowVault {
    /// Sets up a new escrow deal. May only be called once per contract
    /// instance. Requires the `payer`'s authorization, since they are the
    /// party committing to fund the vault.
    ///
    /// Does not move any funds — call `deposit` afterwards to actually lock
    /// `amount` of `token` into the vault.
    pub fn initialize(
        env: Env,
        payer: Address,
        payee: Address,
        arbiter: Address,
        token: Address,
        amount: i128,
    ) {
        payer.require_auth();

        if env.storage().instance().has(&DataKey::Escrow) {
            panic_with_error!(&env, EscrowError::AlreadyInitialized);
        }
        if amount <= 0 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }
        if payer == payee || payer == arbiter || payee == arbiter {
            panic_with_error!(&env, EscrowError::InvalidParties);
        }

        let escrow = EscrowData {
            payer: payer.clone(),
            payee: payee.clone(),
            arbiter: arbiter.clone(),
            token: token.clone(),
            amount,
            funded: false,
            status: Status::Pending,
        };
        env.storage().instance().set(&DataKey::Escrow, &escrow);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

        env.events()
            .publish((symbol_short!("init"), payer, payee, arbiter), (token, amount));
    }

    /// Locks `amount` of the escrow's token into the vault by pulling it
    /// from `payer`. Requires the `payer`'s authorization. May only be
    /// called once, and only while the escrow is `Pending` and not yet
    /// funded.
    pub fn deposit(env: Env) {
        let mut escrow = Self::load(&env);

        escrow.payer.require_auth();

        if escrow.status != Status::Pending {
            panic_with_error!(&env, EscrowError::NotPending);
        }
        if escrow.funded {
            panic_with_error!(&env, EscrowError::AlreadyFunded);
        }

        token::Client::new(&env, &escrow.token).transfer(
            &escrow.payer,
            &env.current_contract_address(),
            &escrow.amount,
        );

        escrow.funded = true;
        env.storage().instance().set(&DataKey::Escrow, &escrow);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

        env.events()
            .publish((symbol_short!("deposit"), escrow.payer.clone()), escrow.amount);
    }

    /// Releases the locked funds to the `payee`. Callable by either the
    /// `payer` or the `arbiter`; `caller` must be one of those two
    /// addresses and must supply a valid authorization for itself. Requires
    /// the escrow to be `Pending` and funded.
    pub fn release(env: Env, caller: Address) {
        let mut escrow = Self::load(&env);

        caller.require_auth();
        if caller != escrow.payer && caller != escrow.arbiter {
            panic_with_error!(&env, EscrowError::Unauthorized);
        }

        if escrow.status != Status::Pending {
            panic_with_error!(&env, EscrowError::NotPending);
        }
        if !escrow.funded {
            panic_with_error!(&env, EscrowError::NotFunded);
        }

        token::Client::new(&env, &escrow.token).transfer(
            &env.current_contract_address(),
            &escrow.payee,
            &escrow.amount,
        );

        escrow.status = Status::Completed;
        env.storage().instance().set(&DataKey::Escrow, &escrow);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

        env.events().publish(
            (symbol_short!("release"), caller, escrow.payee.clone()),
            escrow.amount,
        );
    }

    /// Returns the locked funds to the `payer`. Callable only by the
    /// `arbiter`. Requires the escrow to be `Pending` and funded.
    pub fn refund(env: Env) {
        let mut escrow = Self::load(&env);

        escrow.arbiter.require_auth();

        if escrow.status != Status::Pending {
            panic_with_error!(&env, EscrowError::NotPending);
        }
        if !escrow.funded {
            panic_with_error!(&env, EscrowError::NotFunded);
        }

        token::Client::new(&env, &escrow.token).transfer(
            &env.current_contract_address(),
            &escrow.payer,
            &escrow.amount,
        );

        escrow.status = Status::Refunded;
        env.storage().instance().set(&DataKey::Escrow, &escrow);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

        env.events().publish(
            (symbol_short!("refund"), escrow.arbiter.clone(), escrow.payer.clone()),
            escrow.amount,
        );
    }

    /// Returns the current lifecycle status of the escrow.
    pub fn get_status(env: Env) -> Status {
        Self::load(&env).status
    }

    /// Returns the full escrow record (roles, token, amount, funded flag,
    /// and status).
    pub fn get_escrow(env: Env) -> EscrowData {
        Self::load(&env)
    }

    /// Loads the escrow record from instance storage, panicking with
    /// `EscrowError::NotInitialized` if `initialize` has not been called
    /// yet.
    fn load(env: &Env) -> EscrowData {
        env.storage()
            .instance()
            .get(&DataKey::Escrow)
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::NotInitialized))
    }
}

#[cfg(test)]
mod test;
