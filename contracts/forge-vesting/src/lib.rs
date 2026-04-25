#![no_std]

//! # forge-vesting
//!
//! Token vesting contract with configurable cliff and linear release schedule.
//!
//! ## Overview
//! - Deploy with a token, beneficiary, total amount, cliff period, and vesting duration
//! - After the cliff, tokens unlock linearly every second
//! - Beneficiary can call `claim()` at any time to withdraw unlocked tokens
//! - Admin can cancel vesting and reclaim unvested tokens

use forge_errors::CommonError;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Env, Symbol,
};

// ── Storage Keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Config,
    Claimed,
    VestedAtCancel,
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub struct VestingConfig {
    /// Token contract address
    pub token: Address,
    /// Beneficiary who receives vested tokens
    pub beneficiary: Address,
    /// Admin who can cancel vesting
    pub admin: Address,
    /// Total tokens to vest
    pub total_amount: i128,
    /// Timestamp when vesting starts
    pub start_time: u64,
    /// Seconds before any tokens unlock
    pub cliff_seconds: u64,
    /// Total vesting duration in seconds
    pub duration_seconds: u64,
    /// Whether vesting has been cancelled
    pub cancelled: bool,
    /// Whether vesting is currently paused
    pub paused: bool,
    /// Ledger timestamp when vesting was paused (None if not paused)
    pub paused_at: Option<u64>,
}

#[contracttype]
#[derive(Clone)]
pub struct VestingStatus {
    pub total_amount: i128,
    pub claimed: i128,
    pub vested: i128,
    pub claimable: i128,
    pub cliff_reached: bool,
    pub fully_vested: bool,
    pub paused: bool,
}

/// Vesting schedule configuration (excludes admin and cancellation state).
///
/// Returned by [`get_vesting_schedule`](crate::ForgeVesting::get_vesting_schedule)
/// to expose the original vesting parameters without sensitive or mutable fields.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VestingSchedule {
    /// Token contract address
    pub token: Address,
    /// Beneficiary who receives vested tokens
    pub beneficiary: Address,
    /// Total tokens to vest
    pub total_amount: i128,
    /// Seconds before any tokens unlock
    pub cliff_seconds: u64,
    /// Total vesting duration in seconds
    pub duration_seconds: u64,
    /// Timestamp when vesting starts
    pub start_time: u64,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum VestingError {
    #[from(CommonError)]
    Common(CommonError),
    CliffNotReached = 4,
    NothingToClaim = 5,
    Cancelled = 6,
    SameAdmin = 8,
    SameBeneficiary = 11,
    BeneficiaryAsAdmin = 12,
    Paused = 9,
    NotPaused = 10,
    VestingComplete = 13,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct ForgeVesting;

#[contractimpl]
impl ForgeVesting {
    /// Initialize a new vesting schedule.
    ///
    /// Sets up the vesting configuration and records the current ledger timestamp
    /// as the start time. Must be called exactly once; subsequent calls return
    /// [`VestingError::AlreadyInitialized`]. Requires authorization from `admin`.
    ///
    /// # Parameters
    /// - `token` — Address of the Soroban token contract whose tokens are being vested.
    /// - `beneficiary` — Address that will receive tokens as they vest.
    /// - `admin` — Address authorized to cancel the vesting schedule.
    /// - `total_amount` — Total number of tokens (in the token's smallest unit) to vest.
    ///   Must be greater than zero.
    /// - `cliff_seconds` — Number of seconds after `start_time` before any tokens unlock.
    ///   Must be ≤ `duration_seconds`.
    /// - `duration_seconds` — Total length of the vesting schedule in seconds. Must be > 0.
    ///
    /// # Returns
    /// `Ok(())` on success, or a [`VestingError`] variant on failure.
    ///
    /// # Errors
    /// - [`VestingError::AlreadyInitialized`] — Contract has already been initialized.
    /// - [`VestingError::InvalidConfig`] — `total_amount` ≤ 0, `duration_seconds` == 0,
    ///   or `cliff_seconds` > `duration_seconds`.
    ///
    /// # Example
    /// ```rust,ignore
    /// // Vest 1 000 000 tokens over 1000 s with a 100 s cliff.
    /// client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
    /// ```
    pub fn initialize(
        env: Env,
        token: Address,
        beneficiary: Address,
        admin: Address,
        total_amount: i128,
        cliff_seconds: u64,
        duration_seconds: u64,
    ) -> Result<(), VestingError> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(VestingError::Common(CommonError::AlreadyInitialized));
        }
        if total_amount <= 0 || duration_seconds == 0 || cliff_seconds > duration_seconds {
            return Err(VestingError::Common(CommonError::InvalidConfig));
        }
        if admin == beneficiary {
            return Err(VestingError::BeneficiaryAsAdmin);
        }

        admin.require_auth();

        let config = VestingConfig {
            token,
            beneficiary,
            admin,
            total_amount,
            start_time: env.ledger().timestamp(),
            cliff_seconds,
            duration_seconds,
            cancelled: false,
            paused: false,
            paused_at: None,
        };

        env.storage().instance().set(&DataKey::Config, &config);
        env.storage().instance().set(&DataKey::Claimed, &0_i128);

        env.events().publish(
            (Symbol::new(&env, "vesting_initialized"),),
            (
                config.total_amount,
                config.cliff_seconds,
                config.duration_seconds,
            ),
        );

        Ok(())
    }

    /// Claim all currently vested and unclaimed tokens.
    ///
    /// Computes the amount vested up to the current ledger timestamp, subtracts
    /// previously claimed tokens, and transfers the remainder to the beneficiary.
    /// Requires authorization from the beneficiary.
    ///
    /// # Returns
    /// `Ok(amount)` — the number of tokens transferred on this call.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    /// - [`VestingError::Cancelled`] — The vesting schedule was cancelled by the admin.
    /// - [`VestingError::CliffNotReached`] — Current time is before `start_time + cliff_seconds`.
    /// - [`VestingError::NothingToClaim`] — All vested tokens have already been claimed.
    ///
    /// # Example
    /// ```rust,ignore
    /// // After the cliff has passed:
    /// let claimed = client.claim(); // returns tokens vested so far
    /// ```
    pub fn claim(env: Env) -> Result<i128, VestingError> {
        let config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        if config.cancelled {
            return Err(VestingError::Cancelled);
        }

        if config.paused {
            return Err(VestingError::Common(CommonError::Unauthorized));
        }

        config.beneficiary.require_auth();

        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(config.start_time);

        if elapsed < config.cliff_seconds {
            return Err(VestingError::CliffNotReached);
        }

        let vested = Self::compute_vested(&config, now);
        let claimed = Self::get_claimed(&env);
        let claimable = vested - claimed;

        if claimable <= 0 {
            return Err(VestingError::NothingToClaim);
        }

        env.storage()
            .instance()
            .set(&DataKey::Claimed, &(claimed + claimable));

        let token_client = token::Client::new(&env, &config.token);
        token_client.transfer(
            &env.current_contract_address(),
            &config.beneficiary,
            &claimable,
        );

        env.events().publish(
            (Symbol::new(&env, "claimed"),),
            (&config.beneficiary, claimable),
        );

        Ok(claimable)
    }

    /// Cancel the vesting schedule and return unvested tokens to the admin.
    ///
    /// Computes how many tokens have vested (or been claimed) at the current ledger
    /// timestamp and transfers the remainder back to `admin`. Once cancelled, neither
    /// `claim` nor `cancel` can be called again. Requires authorization from `admin`.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    /// - [`VestingError::Cancelled`] — The schedule is already cancelled.
    ///
    /// # Example
    /// ```rust,ignore
    /// // Admin decides to terminate the schedule early:
    /// client.cancel(); // unvested tokens are returned to admin
    /// ```
    pub fn cancel(env: Env) -> Result<(), VestingError> {
        let mut config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        config.admin.require_auth();

        if config.cancelled {
            return Err(VestingError::Cancelled);
        }

        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(config.start_time);

        if elapsed >= config.duration_seconds {
            return Err(VestingError::VestingComplete);
        }

        let vested = Self::compute_vested(&config, now);
        let claimed = Self::get_claimed(&env);

        // Split tokens: vested-but-unclaimed goes to beneficiary, unvested goes to admin
        let to_beneficiary = vested - claimed;
        let to_admin = config.total_amount - vested;

        config.cancelled = true;
        env.storage().instance().set(&DataKey::Config, &config);
        env.storage()
            .instance()
            .set(&DataKey::VestedAtCancel, &vested);
        // Update claimed to vested so get_status().claimable reflects 0 after cancel payout
        env.storage().instance().set(&DataKey::Claimed, &vested);

        let token_client = token::Client::new(&env, &config.token);

        // Transfer vested-but-unclaimed tokens to beneficiary
        if to_beneficiary > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &config.beneficiary,
                &to_beneficiary,
            );
        }

        // Transfer unvested tokens to admin
        if to_admin > 0 {
            token_client.transfer(&env.current_contract_address(), &config.admin, &to_admin);
        }

        env.events().publish(
            (Symbol::new(&env, "vesting_cancelled"),),
            (&config.admin, to_admin, &config.beneficiary, to_beneficiary),
        );

        Ok(())
    }

    /// Atomically claim all vested tokens for the beneficiary and return unvested tokens to the admin.
    ///
    /// Combines `claim()` and `cancel()` into a single transaction, eliminating the race condition
    /// where an admin could cancel before a beneficiary claims. Requires authorization from both
    /// `admin` and `beneficiary`.
    ///
    /// # Returns
    /// `Ok((to_beneficiary, to_admin))` — tokens transferred to each party.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    /// - [`VestingError::Cancelled`] — The schedule is already cancelled.
    /// - [`VestingError::Paused`] — The schedule is currently paused.
    pub fn cancel_and_claim(env: Env) -> Result<(i128, i128), VestingError> {
        let mut config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        if config.cancelled {
            return Err(VestingError::Cancelled);
        }
        if config.paused {
            return Err(VestingError::Common(CommonError::Unauthorized));
        }

        config.admin.require_auth();
        config.beneficiary.require_auth();

        let now = env.ledger().timestamp();
        let vested = Self::compute_vested(&config, now);
        let claimed = Self::get_claimed(&env);
        let to_beneficiary = vested - claimed;
        let to_admin = config.total_amount - vested;

        config.cancelled = true;
        env.storage().instance().set(&DataKey::Config, &config);
        env.storage()
            .instance()
            .set(&DataKey::VestedAtCancel, &vested);
        env.storage()
            .instance()
            .set(&DataKey::Claimed, &(claimed + to_beneficiary));

        let token_client = token::Client::new(&env, &config.token);
        if to_beneficiary > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &config.beneficiary,
                &to_beneficiary,
            );
        }
        if to_admin > 0 {
            token_client.transfer(&env.current_contract_address(), &config.admin, &to_admin);
        }

        env.events().publish(
            (Symbol::new(&env, "claimed"),),
            (&config.beneficiary, to_beneficiary),
        );
        env.events().publish(
            (Symbol::new(&env, "vesting_cancelled"),),
            (&config.admin, to_admin, &config.beneficiary, to_beneficiary),
        );

        Ok((to_beneficiary, to_admin))
    }

    /// Transfer admin rights to a new address.
    ///
    /// Allows the current admin to transfer their admin privileges to a new address.
    /// This is useful when teams change or multisigs are rotated. Requires authorization
    /// from the current admin.
    ///
    /// # Parameters
    /// - `new_admin` — Address that will become the new admin.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    /// - [`VestingError::SameAdmin`] — `new_admin` is the same as the current admin.
    ///
    /// # Example
    /// ```rust,ignore
    /// // Transfer admin rights to a new multisig:
    /// client.transfer_admin(&new_admin_address);
    /// ```
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), VestingError> {
        let mut config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        config.admin.require_auth();

        if config.admin == new_admin {
            return Err(VestingError::SameAdmin);
        }
        if config.beneficiary == new_admin {
            return Err(VestingError::BeneficiaryAsAdmin);
        }

        let old_admin = config.admin;
        config.admin = new_admin.clone();
        env.storage().instance().set(&DataKey::Config, &config);

        env.events().publish(
            (Symbol::new(&env, "admin_transferred"),),
            (&old_admin, &new_admin),
        );

        Ok(())
    }

    /// Transfer beneficiary rights to a new address.
    ///
    /// Allows the current beneficiary to transfer their vesting rights to a new address.
    /// This is useful for wallet migration scenarios or when transferring vesting rights
    /// to another party. Requires authorization from the current beneficiary.
    ///
    /// # Parameters
    /// - `new_beneficiary` — Address that will become the new beneficiary.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    /// - [`VestingError::Cancelled`] — The vesting schedule has been cancelled.
    /// - [`VestingError::SameBeneficiary`] — `new_beneficiary` is the same as the current beneficiary.
    ///
    /// # Example
    /// ```rust,ignore
    /// // Transfer beneficiary rights to a new wallet:
    /// client.change_beneficiary(&new_beneficiary_address);
    /// ```
    pub fn change_beneficiary(env: Env, new_beneficiary: Address) -> Result<(), VestingError> {
        let mut config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        config.beneficiary.require_auth();

        if config.cancelled {
            return Err(VestingError::Cancelled);
        }

        if config.beneficiary == new_beneficiary {
            return Err(VestingError::SameBeneficiary);
        }

        let old_beneficiary = config.beneficiary;
        config.beneficiary = new_beneficiary.clone();
        env.storage().instance().set(&DataKey::Config, &config);

        env.events().publish(
            (Symbol::new(&env, "beneficiary_changed"),),
            (&old_beneficiary, &new_beneficiary),
        );

        Ok(())
    }

    /// Return a snapshot of the current vesting status.
    ///
    /// Reads the ledger timestamp and computes vested, claimed, and claimable
    /// amounts without modifying any state. Safe to call by anyone.
    ///
    /// # Returns
    /// `Ok(`[`VestingStatus`]`)` containing:
    /// - `total_amount` — Total tokens in the schedule.
    /// - `claimed` — Tokens already transferred to the beneficiary.
    /// - `vested` — Tokens unlocked so far (including already claimed).
    /// - `claimable` — Tokens available to claim right now (`vested - claimed`).
    /// - `cliff_reached` — `true` if the cliff timestamp has passed.
    /// - `fully_vested` — `true` if the full duration has elapsed.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    ///
    /// # Example
    /// ```rust,ignore
    /// let status = client.get_status();
    /// if status.cliff_reached {
    ///     println!("Claimable: {}", status.claimable);
    /// }
    /// ```
    pub fn get_status(env: Env) -> Result<VestingStatus, VestingError> {
        let config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(config.start_time);
        let cliff_reached = elapsed >= config.cliff_seconds;
        let vested = if config.cancelled {
            env.storage()
                .instance()
                .get(&DataKey::VestedAtCancel)
                .unwrap_or(0)
        } else {
            Self::compute_vested(&config, now)
        };
        let claimed = Self::get_claimed(&env);
        let claimable = (vested - claimed).max(0);
        let fully_vested = vested >= config.total_amount;

        Ok(VestingStatus {
            total_amount: config.total_amount,
            claimed,
            vested,
            claimable,
            cliff_reached,
            fully_vested,
            paused: config.paused,
        })
    }

    /// Return the full vesting configuration set at initialization.
    ///
    /// Exposes all fields of [`VestingConfig`] including token, beneficiary, admin,
    /// amounts, timing parameters, and cancellation status. Read-only; does not
    /// modify state.
    ///
    /// # Deprecation Notice
    ///
    /// **Prefer [`get_vesting_schedule`] and [`get_status`] for public-facing reads.**
    /// `get_config` exposes the admin address and internal cancellation flag, which
    /// may be a privacy concern in some deployments. Use the alternatives instead:
    /// - [`get_vesting_schedule`] — token, beneficiary, amounts, and timing (no admin)
    /// - [`get_status`] — claimable amount, vested amount, cliff status, and pause state
    ///
    /// `get_config` is retained for admin tooling and backward compatibility.
    ///
    /// # Returns
    /// `Ok(`[`VestingConfig`]`)` with the stored configuration.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    pub fn get_config(env: Env) -> Result<VestingConfig, VestingError> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)
    }

    /// Return the vesting schedule parameters.
    ///
    /// Exposes the original vesting configuration including token, beneficiary,
    /// total amount, cliff, duration, and start time. Unlike [`get_config`],
    /// this excludes admin and cancellation state for a cleaner public interface.
    /// Read-only; does not modify state.
    ///
    /// # Returns
    /// `Ok(`[`VestingSchedule`]`)` containing the vesting schedule parameters.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — `initialize` has not been called.
    ///
    /// # Example
    /// ```text
    /// let schedule = client.get_vesting_schedule();
    /// println!("Total: {}, Cliff: {}s, Duration: {}s",
    ///     schedule.total_amount, schedule.cliff_seconds, schedule.duration_seconds);
    /// ```
    pub fn get_vesting_schedule(env: Env) -> Result<VestingSchedule, VestingError> {
        let config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        Ok(VestingSchedule {
            token: config.token,
            beneficiary: config.beneficiary,
            total_amount: config.total_amount,
            cliff_seconds: config.cliff_seconds,
            duration_seconds: config.duration_seconds,
            start_time: config.start_time,
        })
    }

    // ── Pause / Unpause ───────────────────────────────────────────────────────

    /// Pause the vesting schedule, freezing token accumulation.
    ///
    /// While paused, `claim()` is blocked and `compute_vested` uses `paused_at`
    /// as the effective current time so the vested amount stays frozen.
    /// Requires authorization from `admin`.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — Contract not initialized.
    /// - [`VestingError::Paused`] — Already paused.
    pub fn pause(env: Env) -> Result<(), VestingError> {
        let mut config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        config.admin.require_auth();

        if config.cancelled {
            return Err(VestingError::Cancelled);
        }

        if config.paused {
            return Err(VestingError::Common(CommonError::Unauthorized));
        }

        config.paused = true;
        config.paused_at = Some(env.ledger().timestamp());
        env.storage().instance().set(&DataKey::Config, &config);

        Ok(())
    }

    /// Unpause the vesting schedule, shifting the timeline forward by the pause duration.
    ///
    /// Calculates `delta = now - paused_at` and adds it to both `start_time` and
    /// `end_time` (via `duration_seconds` anchor) so the full remaining schedule
    /// is preserved. Requires authorization from `admin`.
    ///
    /// # Errors
    /// - [`VestingError::NotInitialized`] — Contract not initialized.
    /// - [`VestingError::NotPaused`] — Not currently paused.
    pub fn unpause(env: Env) -> Result<(), VestingError> {
        let mut config: VestingConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)?;

        config.admin.require_auth();

        if config.cancelled {
            return Err(VestingError::Cancelled);
        }

        if !config.paused {
            return Err(VestingError::Common(CommonError::NotInitialized));
        }

        let now = env.ledger().timestamp();
        let paused_at = config.paused_at.unwrap_or(now);
        let delta = now.saturating_sub(paused_at);
        config.start_time = config.start_time.saturating_add(delta);
        config.paused = false;
        config.paused_at = None;
        env.storage().instance().set(&DataKey::Config, &config);

        Ok(())
    }

    // ── Private ───────────────────────────────────────────────────────────────

    /// Get the claimed amount from storage.
    ///
    /// Returns 0 if called before initialization (though this should never happen
    /// in practice since all public methods check for initialization first).
    fn get_claimed(env: &Env) -> i128 {
        env.storage().instance().get(&DataKey::Claimed).unwrap_or(0)
    }

    fn compute_vested(config: &VestingConfig, now: u64) -> i128 {
        if config.cancelled {
            return 0;
        }
        let effective_now = if config.paused {
            config.paused_at.unwrap_or(now)
        } else {
            now
        };
        let elapsed = effective_now.saturating_sub(config.start_time);
        if elapsed < config.cliff_seconds {
            return 0;
        }
        if elapsed >= config.duration_seconds {
            return config.total_amount;
        }
        (config.total_amount * elapsed as i128) / config.duration_seconds as i128
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Env,
    };

    fn setup() -> (Env, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeVesting);
        let token = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        (env, contract_id, token, beneficiary, admin)
    }

    #[test]
    fn test_initialize_success() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        let result = client.try_initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cancel_after_full_vesting_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        // Advance past duration
        env.ledger().with_mut(|l| l.timestamp += 1001);
        let result = client.try_cancel();
        assert_eq!(result, Err(Ok(VestingError::VestingComplete)));
    }

    #[test]
    fn test_claim_after_failed_cancel_succeeds() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);

        // Mock token transfer for claim
        env.mock_all_auths();

        // Advance to full vesting
        env.ledger().with_mut(|l| l.timestamp += 1000);

        // Cancel fails
        let cancel_result = client.try_cancel();
        assert_eq!(cancel_result, Err(Ok(VestingError::VestingComplete)));

        // Beneficiary can still claim
        let claim_result = client.try_claim();
        assert!(claim_result.is_ok());
        assert_eq!(claim_result.unwrap(), Ok(1_000_000));
    }

    #[test]
    fn test_compute_vested_dust_verification() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        // setup_with_token mints 1_000_000; we only vest 1000 here
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1000, &0, &3);

        env.mock_all_auths();

        // Start at t=0
        let start_ts = env.ledger().timestamp();

        // t=1
        env.ledger().with_mut(|l| l.timestamp = start_ts + 1);
        let v1 = client.claim();
        assert_eq!(v1, 333); // (1000 * 1) / 3 = 333

        // t=2
        env.ledger().with_mut(|l| l.timestamp = start_ts + 2);
        let v2 = client.claim();
        assert_eq!(v2, 333); // (1000 * 2) / 3 - 333 = 666 - 333 = 333

        // t=3
        env.ledger().with_mut(|l| l.timestamp = start_ts + 3);
        let v3 = client.claim();
        assert_eq!(v3, 334); // 1000 - 666 = 334

        assert_eq!(v1 + v2 + v3, 1000);
    }

    #[test]
    fn test_double_initialize_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);

        // Initial setup
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        // Attempt re-initialization with DIFFERENT values
        let new_beneficiary = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let result = client.try_initialize(
            &token,
            &new_beneficiary,
            &new_admin,
            &9_999_999,
            &500,
            &5000,
        );

        // Assert it fails with AlreadyInitialized
        assert_eq!(result, Err(Ok(VestingError::AlreadyInitialized)));

        // Verify original state is unchanged
        let config = client.get_config();
        assert_eq!(config.token, token);
        assert_eq!(config.beneficiary, beneficiary);
        assert_eq!(config.admin, admin);
        assert_eq!(config.total_amount, 1_000_000);
        assert_eq!(config.cliff_seconds, 100);
        assert_eq!(config.duration_seconds, 1000);
        assert!(!config.cancelled);
    }

    #[test]
    fn test_claim_before_cliff_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &500, &1000);
        // advance 100s — still before cliff of 500
        env.ledger().with_mut(|l| l.timestamp += 100);
        let result = client.try_claim();
        assert_eq!(result, Err(Ok(VestingError::CliffNotReached)));
    }

    #[test]
    fn test_get_status_before_cliff() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &500, &1000);
        let status = client.get_status();
        assert!(!status.cliff_reached);
        assert_eq!(status.claimable, 0);
        assert_eq!(status.claimed, 0);
    }

    #[test]
    fn test_get_vesting_schedule_returns_init_params() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &2_500_000, &200, &5000);

        let schedule = client.get_vesting_schedule();
        assert_eq!(schedule.token, token);
        assert_eq!(schedule.beneficiary, beneficiary);
        assert_eq!(schedule.total_amount, 2_500_000);
        assert_eq!(schedule.cliff_seconds, 200);
        assert_eq!(schedule.duration_seconds, 5000);
        assert_eq!(schedule.start_time, env.ledger().timestamp());
    }

    #[test]
    fn test_get_vesting_schedule_matches_init_params() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);

        let total = 10_000_000_i128;
        let cliff = 86400_u64; // 1 day
        let duration = 31536000_u64; // 1 year

        client.initialize(&token, &beneficiary, &admin, &total, &cliff, &duration);

        let schedule = client.get_vesting_schedule();
        assert_eq!(schedule.total_amount, total);
        assert_eq!(schedule.cliff_seconds, cliff);
        assert_eq!(schedule.duration_seconds, duration);
    }

    #[test]
    fn test_get_vesting_schedule_fails_when_not_initialized() {
        let (env, contract_id, _, _, _) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        let result = client.try_get_vesting_schedule();
        assert_eq!(result, Err(Ok(VestingError::NotInitialized)));
    }

    #[test]
    fn test_schedule_and_status_provide_full_ui_info_without_get_config() {
        // get_vesting_schedule() + get_status() together expose everything a UI
        // needs: token, beneficiary, amounts, timing, claimable — without
        // leaking the admin address or internal cancellation flag.
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        // Advance past cliff
        env.ledger().with_mut(|l| l.timestamp += 500);

        let schedule = client.get_vesting_schedule();
        assert_eq!(schedule.token, token);
        assert_eq!(schedule.beneficiary, beneficiary);
        assert_eq!(schedule.total_amount, 1_000_000);
        assert_eq!(schedule.cliff_seconds, 100);
        assert_eq!(schedule.duration_seconds, 1000);

        let status = client.get_status();
        assert!(status.cliff_reached);
        assert!(status.claimable > 0);
        assert_eq!(status.claimed, 0);
        assert!(!status.fully_vested);
    }

    #[test]
    fn test_invalid_config_rejected() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        // cliff > duration is invalid
        let result = client.try_initialize(&token, &beneficiary, &admin, &1_000_000, &2000, &1000);
        assert_eq!(result, Err(Ok(VestingError::InvalidConfig)));
    }

    /// Test initialize() with total_amount = 0 returns InvalidConfig
    #[test]
    fn test_initialize_total_amount_zero_returns_invalid_config() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);

        // Try to initialize with total_amount = 0
        let result = client.try_initialize(&token, &beneficiary, &admin, &0, &100, &1000);
        assert_eq!(result, Err(Ok(VestingError::InvalidConfig)));

        // Verify no config is stored after failed call
        let config_result = client.try_get_config();
        assert_eq!(config_result, Err(Ok(VestingError::NotInitialized)));
    }

    /// Test initialize() with total_amount = -1 returns InvalidConfig
    #[test]
    fn test_initialize_total_amount_negative_returns_invalid_config() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);

        // Try to initialize with total_amount = -1
        let result = client.try_initialize(&token, &beneficiary, &admin, &-1, &100, &1000);
        assert_eq!(result, Err(Ok(VestingError::InvalidConfig)));

        // Verify no config is stored after failed call
        let config_result = client.try_get_config();
        assert_eq!(config_result, Err(Ok(VestingError::NotInitialized)));
    }

    /// Test initialize() with duration_seconds = 0 returns InvalidConfig
    #[test]
    fn test_initialize_duration_zero_returns_invalid_config() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);

        // Try to initialize with duration_seconds = 0
        let result = client.try_initialize(&token, &beneficiary, &admin, &1_000_000, &100, &0);
        assert_eq!(result, Err(Ok(VestingError::InvalidConfig)));

        // Verify no config is stored after failed call
        let config_result = client.try_get_config();
        assert_eq!(config_result, Err(Ok(VestingError::NotInitialized)));
    }

    /// Test that subsequent valid initialize() succeeds after failed attempts
    #[test]
    fn test_valid_initialize_succeeds_after_invalid_attempts() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);

        // Attempt 1: total_amount = 0 (should fail)
        let result1 = client.try_initialize(&token, &beneficiary, &admin, &0, &100, &1000);
        assert_eq!(result1, Err(Ok(VestingError::InvalidConfig)));
        assert_eq!(
            client.try_get_config(),
            Err(Ok(VestingError::NotInitialized))
        );

        // Attempt 2: total_amount = -1 (should fail)
        let result2 = client.try_initialize(&token, &beneficiary, &admin, &-1, &100, &1000);
        assert_eq!(result2, Err(Ok(VestingError::InvalidConfig)));
        assert_eq!(
            client.try_get_config(),
            Err(Ok(VestingError::NotInitialized))
        );

        // Attempt 3: duration_seconds = 0 (should fail)
        let result3 = client.try_initialize(&token, &beneficiary, &admin, &1_000_000, &100, &0);
        assert_eq!(result3, Err(Ok(VestingError::InvalidConfig)));
        assert_eq!(
            client.try_get_config(),
            Err(Ok(VestingError::NotInitialized))
        );

        // Attempt 4: cliff > duration (should fail)
        let result4 = client.try_initialize(&token, &beneficiary, &admin, &1_000_000, &2000, &1000);
        assert_eq!(result4, Err(Ok(VestingError::InvalidConfig)));
        assert_eq!(
            client.try_get_config(),
            Err(Ok(VestingError::NotInitialized))
        );

        // Final attempt: valid parameters (should succeed)
        let result5 = client.try_initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        assert!(result5.is_ok());

        // Verify config is properly stored after successful initialization
        let config = client.get_config();
        assert_eq!(config.token, token);
        assert_eq!(config.beneficiary, beneficiary);
        assert_eq!(config.admin, admin);
        assert_eq!(config.total_amount, 1_000_000);
        assert_eq!(config.cliff_seconds, 100);
        assert_eq!(config.duration_seconds, 1000);
        assert!(!config.cancelled);
        assert!(!config.paused);
    }

    #[test]
    fn test_cancel_by_admin() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);
        let result = client.try_cancel();
        assert!(result.is_ok());
    }

    #[test]
    fn test_double_cancel_fails() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);
        client.cancel();
        let result = client.try_cancel();
        assert_eq!(result, Err(Ok(VestingError::Cancelled)));
    }

    #[test]
    fn test_claim_after_cancel_fails() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);
        client.cancel();
        env.ledger().with_mut(|l| l.timestamp += 200);
        let result = client.try_claim();
        assert_eq!(result, Err(Ok(VestingError::Cancelled)));
    }

    #[test]
    fn test_fully_vested_after_duration() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 2000);
        let status = client.get_status();
        assert!(status.fully_vested);
        assert_eq!(status.vested, 1_000_000);
    }

    /// Verifies get_status() reflects correct state across two partial claims.
    ///
    /// Timeline (total=10_000, cliff=0, duration=1000):
    /// - t=200: vested=2_000, claimed=0, claimable=2_000
    /// - claim() at t=200
    /// - t=200: vested=2_000, claimed=2_000, claimable=0
    /// - t=500: vested=5_000, claimed=2_000, claimable=3_000
    /// - claim() at t=500
    /// - t=500: vested=5_000, claimed=5_000, claimable=0
    #[test]
    fn test_get_status_after_partial_claim_then_time_advance() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &10_000, &0, &1000);

        // t=200: 20% vested, nothing claimed yet
        env.ledger().with_mut(|l| l.timestamp = 200);
        let s = client.get_status();
        assert_eq!(s.vested, 2_000);
        assert_eq!(s.claimed, 0);
        assert_eq!(s.claimable, 2_000);

        client.claim();

        // immediately after claim: claimable drains to 0
        let s = client.get_status();
        assert_eq!(s.vested, 2_000);
        assert_eq!(s.claimed, 2_000);
        assert_eq!(s.claimable, 0);

        // t=500: 50% vested, only the new 3_000 is claimable
        env.ledger().with_mut(|l| l.timestamp = 500);
        let s = client.get_status();
        assert_eq!(s.vested, 5_000);
        assert_eq!(s.claimed, 2_000);
        assert_eq!(s.claimable, 3_000);

        client.claim();

        // after second claim: claimed accumulates, claimable is 0 again
        let s = client.get_status();
        assert_eq!(s.claimed, 5_000);
        assert_eq!(s.claimable, 0);
    }

    fn setup_with_token() -> (Env, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeVesting);
        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        {
            soroban_sdk::token::StellarAssetClient::new(&env, &token_id)
                .mint(&contract_id, &1_000_000);
        }
        (env, contract_id, token_id, beneficiary, admin)
    }

    #[test]
    fn test_cancel_before_cliff_beneficiary_gets_zero_admin_gets_all() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &1000);

        // advance 100s — still before cliff of 500s
        env.ledger().with_mut(|l| l.timestamp += 100);
        client.cancel();

        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 0);
        assert_eq!(tc.balance(&admin), 1_000_000);
    }

    #[test]
    fn test_cancel_after_cliff_splits_tokens_correctly() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);

        // advance 400s — past cliff, 40% vested
        env.ledger().with_mut(|l| l.timestamp += 400);
        client.claim();
        client.cancel();

        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        // 400/1000 * 1_000_000 = 400_000 vested → beneficiary
        // remaining 600_000 → admin
        assert_eq!(tc.balance(&beneficiary), 400_000);
        assert_eq!(tc.balance(&admin), 600_000);
    }

    #[test]
    fn test_cancel_without_claim_sends_vested_to_beneficiary() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);

        // advance 400s — past cliff, 40% vested, but NO claim
        env.ledger().with_mut(|l| l.timestamp += 400);
        client.cancel();

        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        // 400/1000 * 1_000_000 = 400_000 vested → beneficiary (even without claim)
        // remaining 600_000 → admin
        assert_eq!(tc.balance(&beneficiary), 400_000);
        assert_eq!(tc.balance(&admin), 600_000);
    }

    #[test]
    fn test_transfer_admin_success() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        let new_admin = Address::generate(&env);
        let result = client.try_transfer_admin(&new_admin);
        assert!(result.is_ok());
        let config = client.get_config();
        assert_eq!(config.admin, new_admin);
    }

    #[test]
    fn test_transfer_admin_allows_new_admin_to_cancel_old_admin_cannot() {
        use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
        use soroban_sdk::IntoVal;

        let (env, contract_id, token_id, beneficiary, admin_a) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin_a, &1_000_000, &100, &1000);

        let admin_b = Address::generate(&env);
        client.transfer_admin(&admin_b);

        env.mock_auths(&[MockAuth {
            address: &admin_a,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "cancel",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(
            client.try_cancel().is_err(),
            "old admin should not be able to cancel after transfer"
        );

        env.mock_auths(&[MockAuth {
            address: &admin_b,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "cancel",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.cancel();

        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 0);
        assert_eq!(tc.balance(&admin_b), 1_000_000);
    }

    #[test]
    fn test_transfer_admin_then_cancel_before_cliff_unvested_goes_to_new_admin() {
        use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
        use soroban_sdk::IntoVal;

        let (env, contract_id, token_id, beneficiary, admin_a) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin_a, &1_000_000, &500, &1000);

        let admin_b = Address::generate(&env);
        client.transfer_admin(&admin_b);

        // Advance to before cliff so nothing is vested yet
        env.ledger().with_mut(|l| l.timestamp = 100);

        env.mock_auths(&[MockAuth {
            address: &admin_b,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "cancel",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.cancel();

        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 0);
        assert_eq!(tc.balance(&admin_a), 0);
        assert_eq!(tc.balance(&admin_b), 1_000_000);
    }

    #[test]
    fn test_transfer_admin_by_non_admin_fails() {
        use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
        use soroban_sdk::IntoVal;
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeVesting);
        let token = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        let non_admin = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &non_admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "transfer_admin",
                args: (&non_admin,).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        let result = client.try_transfer_admin(&non_admin);
        assert!(result.is_err());
    }

    #[test]
    fn test_transfer_admin_to_same_admin_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        let result = client.try_transfer_admin(&admin);
        assert_eq!(result, Err(Ok(VestingError::SameAdmin)));
    }

    #[test]
    fn test_transfer_admin_to_beneficiary_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        let result = client.try_transfer_admin(&beneficiary);
        assert_eq!(result, Err(Ok(VestingError::BeneficiaryAsAdmin)));
    }

    #[test]
    fn test_initialize_with_admin_as_beneficiary_fails() {
        let (env, contract_id, token, _, _) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        let same_address = Address::generate(&env);
        let result = client.try_initialize(
            &token,
            &same_address,
            &same_address,
            &1_000_000,
            &100,
            &1000,
        );
        assert_eq!(result, Err(Ok(VestingError::BeneficiaryAsAdmin)));
    }

    #[test]
    fn test_change_beneficiary_success() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        let new_beneficiary = Address::generate(&env);
        let result = client.try_change_beneficiary(&new_beneficiary);
        assert!(result.is_ok());

        let config = client.get_config();
        assert_eq!(config.beneficiary, new_beneficiary);
    }

    #[test]
    fn test_change_beneficiary_by_non_beneficiary_fails() {
        use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
        use soroban_sdk::IntoVal;

        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeVesting);
        let token = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        let non_beneficiary = Address::generate(&env);
        let new_beneficiary = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &non_beneficiary,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "change_beneficiary",
                args: (&new_beneficiary,).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        let result = client.try_change_beneficiary(&new_beneficiary);
        assert!(result.is_err());
    }

    #[test]
    fn test_change_beneficiary_to_same_beneficiary_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        let result = client.try_change_beneficiary(&beneficiary);
        assert_eq!(result, Err(Ok(VestingError::SameBeneficiary)));
    }

    #[test]
    fn test_change_beneficiary_preserves_claimed_amount() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);

        // Advance past cliff and claim some tokens
        env.ledger().with_mut(|l| l.timestamp += 500);
        let claimed_amount = client.claim();

        // Change beneficiary
        let new_beneficiary = Address::generate(&env);
        client.change_beneficiary(&new_beneficiary);

        // Verify claimed amount is preserved
        let status = client.get_status();
        assert_eq!(status.claimed, claimed_amount);

        // Verify new beneficiary can claim remaining tokens
        env.ledger().with_mut(|l| l.timestamp += 500);
        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        let new_beneficiary_balance_before = tc.balance(&new_beneficiary);
        client.claim();
        let new_beneficiary_balance_after = tc.balance(&new_beneficiary);
        assert!(new_beneficiary_balance_after > new_beneficiary_balance_before);
    }

    #[test]
    fn test_change_beneficiary_not_initialized_fails() {
        let (env, contract_id, _, _, _) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        let new_beneficiary = Address::generate(&env);
        let result = client.try_change_beneficiary(&new_beneficiary);
        assert_eq!(result, Err(Ok(VestingError::NotInitialized)));
    }

    #[test]
    fn test_change_beneficiary_cancelled_fails() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);
        client.cancel();
        let new_beneficiary = Address::generate(&env);
        let result = client.try_change_beneficiary(&new_beneficiary);
        assert_eq!(result, Err(Ok(VestingError::Cancelled)));
    }

    /// Verifies the fully vested state end-to-end:
    /// - `get_status().fully_vested` is true after the full duration elapses.
    /// - `claimable` equals `total_amount - already_claimed` (handles partial prior claims).
    /// - `claim()` transfers exactly the remaining balance to the beneficiary.
    /// - A subsequent `claim()` fails with `NothingToClaim` — no tokens remain.
    #[test]
    fn test_fully_vested_claim_remaining_tokens() {
        const TOTAL: i128 = 10_000;
        const CLIFF: u64 = 0;
        const DURATION: u64 = 31_536_000; // 365 days

        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeVesting);
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &TOTAL);

        let client = ForgeVestingClient::new(&env, &contract_id);
        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &CLIFF, &DURATION);

        // Partial claim at 50% through vesting
        env.ledger().with_mut(|l| l.timestamp = DURATION / 2);
        let partial = client.claim();
        assert!(partial > 0);

        // Advance past full duration
        env.ledger().with_mut(|l| l.timestamp = DURATION + 1);

        // Status checks
        let status = client.get_status();
        assert!(status.fully_vested);
        assert_eq!(status.claimable, TOTAL - partial);

        // Claim remaining and verify token balance
        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        let before = tc.balance(&beneficiary);
        let remaining = client.claim();
        assert_eq!(remaining, TOTAL - partial);
        assert_eq!(tc.balance(&beneficiary), before + remaining);

        // Second claim must fail — nothing left
        assert_eq!(client.try_claim(), Err(Ok(VestingError::NothingToClaim)));
    }

    // ── Cliff == Duration (instant-vest) edge case ────────────────────────────

    /// Helper: sets up a vesting schedule where cliff == duration so all tokens
    /// vest at once at the cliff moment.
    fn setup_cliff_equals_duration() -> (Env, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeVesting);
        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &1_000_000);
        (env, contract_id, token_id, beneficiary, admin)
    }

    #[test]
    fn test_cliff_equals_duration_initialize_succeeds() {
        // duration_seconds == cliff_seconds must be accepted (cliff <= duration)
        let (env, contract_id, token_id, beneficiary, admin) = setup_cliff_equals_duration();
        let client = ForgeVestingClient::new(&env, &contract_id);
        let result = client.try_initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &500);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cliff_equals_duration_before_cliff_claimable_is_zero() {
        // Before the cliff, nothing should be claimable
        let (env, contract_id, token_id, beneficiary, admin) = setup_cliff_equals_duration();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &500);

        // advance to just before the cliff
        env.ledger().with_mut(|l| l.timestamp += 499);

        let status = client.get_status();
        assert!(!status.cliff_reached);
        assert_eq!(status.claimable, 0);
        assert_eq!(status.vested, 0);

        // claim should fail with CliffNotReached
        let result = client.try_claim();
        assert_eq!(result, Err(Ok(VestingError::CliffNotReached)));
    }

    #[test]
    fn test_cliff_equals_duration_at_cliff_all_tokens_vested() {
        // Exactly at the cliff timestamp the full amount should be vested
        let (env, contract_id, token_id, beneficiary, admin) = setup_cliff_equals_duration();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &500);

        // advance exactly to the cliff / duration boundary
        env.ledger().with_mut(|l| l.timestamp += 500);

        let status = client.get_status();
        assert!(status.cliff_reached);
        assert!(status.fully_vested);
        assert_eq!(status.vested, 1_000_000);
        assert_eq!(status.claimable, 1_000_000);
    }

    #[test]
    fn test_cliff_equals_duration_claim_transfers_full_amount_in_one_call() {
        // A single claim() call after the cliff should transfer all tokens at once
        let (env, contract_id, token_id, beneficiary, admin) = setup_cliff_equals_duration();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &500);

        env.ledger().with_mut(|l| l.timestamp += 500);

        let claimed = client.claim();
        assert_eq!(claimed, 1_000_000);

        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 1_000_000);

        // nothing left to claim
        let result = client.try_claim();
        assert_eq!(result, Err(Ok(VestingError::NothingToClaim)));
    }

    #[test]
    fn test_invariant_claimed_never_exceeds_vested() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        let total_amount = 1_000_000_i128;
        let cliff_seconds = 100_u64;
        let duration_seconds = 1000_u64;

        client.initialize(
            &token_id,
            &beneficiary,
            &admin,
            &total_amount,
            &cliff_seconds,
            &duration_seconds,
        );

        // Track cumulative claimed amount
        let mut cumulative_claimed = 0_i128;

        // Test points: before cliff, at cliff, mid-vesting, fully vested
        let test_timestamps = [
            50_u64, // Before cliff
            100,    // At cliff
            300,    // 30% through vesting
            550,    // 55% through vesting
            800,    // 80% through vesting
            1000,   // Fully vested
            1500,   // Past vesting end
        ];

        for &timestamp in &test_timestamps {
            env.ledger().with_mut(|l| l.timestamp = timestamp);

            let status = client.get_status();

            // Core invariant 1: claimed <= vested
            assert!(
                status.claimed <= status.vested,
                "Invariant violated at t={}: claimed ({}) > vested ({})",
                timestamp,
                status.claimed,
                status.vested
            );

            // Core invariant 2: vested <= total_amount
            assert!(
                status.vested <= status.total_amount,
                "Invariant violated at t={}: vested ({}) > total_amount ({})",
                timestamp,
                status.vested,
                status.total_amount
            );

            // Core invariant 3: claimed <= total_amount
            assert!(
                status.claimed <= status.total_amount,
                "Invariant violated at t={}: claimed ({}) > total_amount ({})",
                timestamp,
                status.claimed,
                status.total_amount
            );

            // Attempt to claim if past cliff
            if timestamp >= cliff_seconds && status.claimable > 0 {
                let claimed_now = client.claim();
                cumulative_claimed += claimed_now;

                // Verify the claim amount is positive and reasonable
                assert!(
                    claimed_now > 0,
                    "Claimed amount should be positive at t={}",
                    timestamp
                );
                assert!(
                    claimed_now <= status.claimable,
                    "Claimed more than claimable at t={}",
                    timestamp
                );

                // Verify status after claim
                let status_after = client.get_status();

                // Invariants must still hold after claim
                assert!(
                    status_after.claimed <= status_after.vested,
                    "Invariant violated after claim at t={}: claimed ({}) > vested ({})",
                    timestamp,
                    status_after.claimed,
                    status_after.vested
                );

                assert!(
                    status_after.vested <= status_after.total_amount,
                    "Invariant violated after claim at t={}: vested ({}) > total_amount ({})",
                    timestamp,
                    status_after.vested,
                    status_after.total_amount
                );

                // Verify cumulative claimed matches status
                assert_eq!(
                    cumulative_claimed, status_after.claimed,
                    "Cumulative claimed mismatch at t={}: tracked={}, status={}",
                    timestamp, cumulative_claimed, status_after.claimed
                );
            }
        }

        // Final verification: all tokens should be claimed by the end
        let final_status = client.get_status();
        assert_eq!(
            final_status.claimed, total_amount,
            "Not all tokens were claimed: claimed={}, total={}",
            final_status.claimed, total_amount
        );
        assert_eq!(
            cumulative_claimed, total_amount,
            "Cumulative tracking mismatch: cumulative={}, total={}",
            cumulative_claimed, total_amount
        );
    }

    // ── Pause / Unpause Tests ─────────────────────────────────────────────────

    /// Test 1: Admin pauses at 50% vesting. Verify get_status shows amount frozen
    /// and claim() fails with Paused.
    #[test]
    fn test_pause_freezes_vested_amount_and_blocks_claim() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        // 1000s duration, 0 cliff
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);

        // Advance to 50% vesting
        env.ledger().with_mut(|l| l.timestamp = 500);
        client.pause();

        let status = client.get_status();
        assert!(status.paused);
        assert_eq!(status.vested, 500_000); // frozen at 50%

        // claim must fail
        assert_eq!(client.try_claim(), Err(Ok(VestingError::Paused)));
    }

    /// Test 2: Advance time by 30 days while paused. Verify vested amount has not increased.
    #[test]
    fn test_vested_amount_does_not_increase_while_paused() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);

        env.ledger().with_mut(|l| l.timestamp = 500);
        client.pause();
        let vested_at_pause = client.get_status().vested;

        // Advance 30 days while paused
        env.ledger().with_mut(|l| l.timestamp += 30 * 24 * 3600);
        let vested_after_30_days = client.get_status().vested;

        assert_eq!(vested_at_pause, vested_after_30_days);
    }

    /// Test 3: Unpause and verify the new end_time (start_time + duration_seconds)
    /// has shifted forward by the pause duration.
    #[test]
    fn test_unpause_shifts_timeline_correctly() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);

        let original_start = client.get_config().start_time;

        env.ledger().with_mut(|l| l.timestamp = 500);
        client.pause();

        // Pause for 200 seconds
        env.ledger().with_mut(|l| l.timestamp = 700);
        client.unpause();

        let config = client.get_config();
        assert!(!config.paused);
        assert_eq!(config.paused_at, None);
        // start_time shifted by 200s
        assert_eq!(config.start_time, original_start + 200);
        // effective end_time = new start_time + duration = original_start + 200 + 1000
        let expected_end = original_start + 200 + 1000;
        assert_eq!(config.start_time + config.duration_seconds, expected_end);
    }

    /// Test 4: Non-admin cannot pause or unpause.
    #[test]
    fn test_non_admin_cannot_pause_or_unpause() {
        use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
        use soroban_sdk::IntoVal;

        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeVesting);
        let token = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &0, &1000);

        let non_admin = Address::generate(&env);

        // Attempt pause as non-admin — must fail (auth error panics, so use try_ with catch)
        env.mock_auths(&[MockAuth {
            address: &non_admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "pause",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_pause().is_err());

        // Restore mock_all_auths and pause as real admin
        env.mock_all_auths();
        client.pause();

        // Attempt unpause as non-admin
        env.mock_auths(&[MockAuth {
            address: &non_admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "unpause",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_unpause().is_err());
    }

    /// Verifies that multiple sequential claim() calls across four time points
    /// accumulate correctly: no tokens are double-counted or lost.
    ///
    /// Schedule: 1_000_000 tokens, no cliff, 1000s duration.
    /// Claims at 25%, 50%, 75%, and 100% vested.
    /// Asserts the sum of all four return values equals total_amount and that
    /// get_status().claimed equals total_amount after all claims.
    #[test]
    fn test_sequential_claims_accumulate_correctly() {
        const TOTAL: i128 = 1_000_000;
        const DURATION: u64 = 1000;

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &0, &DURATION);

        // 25% vested → expect 250_000 claimable
        env.ledger().with_mut(|l| l.timestamp = 250);
        let claim1 = client.claim();
        assert_eq!(claim1, 250_000);

        // 50% vested → 500_000 total vested, 250_000 already claimed → 250_000 claimable
        env.ledger().with_mut(|l| l.timestamp = 500);
        let claim2 = client.claim();
        assert_eq!(claim2, 250_000);

        // 75% vested → 750_000 total vested, 500_000 already claimed → 250_000 claimable
        env.ledger().with_mut(|l| l.timestamp = 750);
        let claim3 = client.claim();
        assert_eq!(claim3, 250_000);

        // 100% vested → 1_000_000 total vested, 750_000 already claimed → 250_000 claimable
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let claim4 = client.claim();
        assert_eq!(claim4, 250_000);

        // Sum of all claims must equal total_amount — no tokens lost or double-counted
        assert_eq!(claim1 + claim2 + claim3 + claim4, TOTAL);

        // get_status().claimed must reflect the full amount
        let status = client.get_status();
        assert_eq!(status.claimed, TOTAL);
        assert!(status.fully_vested);

        // No tokens remain — next claim must fail
        assert_eq!(client.try_claim(), Err(Ok(VestingError::NothingToClaim)));
    }

    // ── Cliff boundary edge case tests ───────────────────────────────────────

    /// claim() must revert with CliffNotReached one second before the cliff.
    #[test]
    fn test_claim_one_second_before_cliff_fails() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &1000);

        // elapsed = 499 → one second before cliff of 500
        env.ledger().with_mut(|l| l.timestamp = 499);
        assert_eq!(client.try_claim(), Err(Ok(VestingError::CliffNotReached)));
    }

    /// claim() must succeed when called exactly at the cliff timestamp.
    #[test]
    fn test_claim_exactly_at_cliff_succeeds() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &1000);

        // elapsed = 500 → exactly at cliff
        env.ledger().with_mut(|l| l.timestamp = 500);
        let result = client.try_claim();
        assert!(result.is_ok());
        // 500/1000 * 1_000_000 = 500_000 vested at cliff
        assert_eq!(result.unwrap(), Ok(500_000));
    }

    /// Tests that claim() returns the correct proportional amount at 25%, 50%, 75%,
    /// and 100% of the vesting duration, and that cumulative claimed never exceeds
    /// total_amount. Uses a cliff at 25% of duration to also verify cliff boundary.
    #[test]
    fn test_claim_correct_amount_at_multiple_time_points() {
        const TOTAL: i128 = 1_000_000;
        const CLIFF: u64 = 250; // 25% of duration
        const DURATION: u64 = 1000;

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &CLIFF, &DURATION);

        // 25% — exactly at cliff: 250/1000 * 1_000_000 = 250_000 vested
        env.ledger().with_mut(|l| l.timestamp = 250);
        let claim1 = client.claim();
        assert_eq!(claim1, 250_000);
        assert!(client.get_status().claimed <= TOTAL);

        // 50% — 500_000 vested, 250_000 already claimed → 250_000 claimable
        env.ledger().with_mut(|l| l.timestamp = 500);
        let claim2 = client.claim();
        assert_eq!(claim2, 250_000);
        assert!(client.get_status().claimed <= TOTAL);

        // 75% — 750_000 vested, 500_000 already claimed → 250_000 claimable
        env.ledger().with_mut(|l| l.timestamp = 750);
        let claim3 = client.claim();
        assert_eq!(claim3, 250_000);
        assert!(client.get_status().claimed <= TOTAL);

        // 100% — 1_000_000 vested, 750_000 already claimed → 250_000 claimable
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let claim4 = client.claim();
        assert_eq!(claim4, 250_000);

        // Cumulative claimed equals total — no tokens lost or double-counted
        assert_eq!(claim1 + claim2 + claim3 + claim4, TOTAL);
        assert_eq!(client.get_status().claimed, TOTAL);
    }

    // ── Event emission tests ──────────────────────────────────────────────────

    /// Verifies initialize() emits a "vesting_initialized" event whose data
    /// payload is exactly (total_amount, cliff_seconds, duration_seconds).
    #[test]
    fn test_event_vesting_initialized() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);

        let total: i128 = 5_000_000;
        let cliff: u64 = 200;
        let duration: u64 = 2000;
        client.initialize(&token, &beneficiary, &admin, &total, &cliff, &duration);

        let events = env.events().all();
        assert_eq!(events.len(), 1);

        let (_, topics, data) = events.get(0).unwrap();

        // Topic must be the symbol "vesting_initialized"
        assert_eq!(topics.len(), 1);
        let topic_sym = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic_sym, Symbol::new(&env, "vesting_initialized"));

        // Decode data as (i128, u64, u64) and compare field by field
        let (got_total, got_cliff, got_duration) =
            <(i128, u64, u64)>::try_from_val(&env, &data).unwrap();
        assert_eq!(got_total, total);
        assert_eq!(got_cliff, cliff);
        assert_eq!(got_duration, duration);
    }

    /// Verifies claim() emits a "claimed" event whose data payload is
    /// exactly (beneficiary, amount_claimed).
    #[test]
    fn test_event_claimed() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);

        // Advance to 50% vested and claim
        env.ledger().with_mut(|l| l.timestamp = 500);
        let claimed_amount = client.claim();

        // Find the "claimed" event among all emitted events
        let events = env.events().all();
        let (_, topics, data) = events
            .iter()
            .find(|(_, topics, _)| {
                topics.len() == 1
                    && Symbol::try_from_val(&env, &topics.get(0).unwrap())
                        .map(|s| s == Symbol::new(&env, "claimed"))
                        .unwrap_or(false)
            })
            .expect("claimed event not found");

        assert_eq!(topics.len(), 1);
        let topic_sym = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic_sym, Symbol::new(&env, "claimed"));

        // Decode data as (Address, i128)
        let (got_beneficiary, got_amount) = <(Address, i128)>::try_from_val(&env, &data).unwrap();
        assert_eq!(got_beneficiary, beneficiary);
        assert_eq!(got_amount, claimed_amount);
    }

    /// Verifies cancel() emits a "vesting_cancelled" event whose data payload is
    /// exactly (admin, to_admin, beneficiary, to_beneficiary).
    #[test]
    fn test_event_vesting_cancelled() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);

        // Advance to 40% vested, then cancel (no prior claim)
        // 40% vested → 400_000 to beneficiary, 600_000 to admin
        env.ledger().with_mut(|l| l.timestamp = 400);
        client.cancel();

        let events = env.events().all();
        let (_, topics, data) = events
            .iter()
            .find(|(_, topics, _)| {
                topics.len() == 1
                    && Symbol::try_from_val(&env, &topics.get(0).unwrap())
                        .map(|s| s == Symbol::new(&env, "vesting_cancelled"))
                        .unwrap_or(false)
            })
            .expect("vesting_cancelled event not found");

        assert_eq!(topics.len(), 1);
        let topic_sym = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic_sym, Symbol::new(&env, "vesting_cancelled"));

        // Decode data as (Address, i128, Address, i128)
        let (got_admin, got_to_admin, got_beneficiary, got_to_beneficiary) =
            <(Address, i128, Address, i128)>::try_from_val(&env, &data).unwrap();
        assert_eq!(got_admin, admin);
        assert_eq!(got_to_admin, 600_000);
        assert_eq!(got_beneficiary, beneficiary);
        assert_eq!(got_to_beneficiary, 400_000);
    }

    // ── cancel_and_claim tests ────────────────────────────────────────────────

    #[test]
    fn test_cancel_and_claim_before_cliff_beneficiary_gets_zero() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &1000);

        env.ledger().with_mut(|l| l.timestamp += 100); // before cliff
        let (to_beneficiary, to_admin) = client.cancel_and_claim();

        assert_eq!(to_beneficiary, 0);
        assert_eq!(to_admin, 1_000_000);
        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 0);
        assert_eq!(tc.balance(&admin), 1_000_000);
    }

    #[test]
    fn test_cancel_and_claim_after_cliff_splits_correctly() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &100, &1000);

        env.ledger().with_mut(|l| l.timestamp += 400); // 40% vested
        let (to_beneficiary, to_admin) = client.cancel_and_claim();

        assert_eq!(to_beneficiary, 400_000);
        assert_eq!(to_admin, 600_000);
        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 400_000);
        assert_eq!(tc.balance(&admin), 600_000);
    }

    #[test]
    fn test_cancel_and_claim_fully_vested_admin_gets_zero() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);

        env.ledger().with_mut(|l| l.timestamp += 1000); // 100% vested
        let (to_beneficiary, to_admin) = client.cancel_and_claim();

        assert_eq!(to_beneficiary, 1_000_000);
        assert_eq!(to_admin, 0);
        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 1_000_000);
        assert_eq!(tc.balance(&admin), 0);
    }

    #[test]
    fn test_get_status_vested_reflects_cancel_time_not_zero() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        // 1_000_000 tokens, no cliff, 1000s duration
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);

        // Advance to 40% vested
        env.ledger().with_mut(|l| l.timestamp += 400);
        client.cancel();

        // Advance time further — vested should still reflect cancel-time amount
        env.ledger().with_mut(|l| l.timestamp += 600);
        let status = client.get_status();

        assert_eq!(
            status.vested, 400_000,
            "vested should reflect amount at cancel time"
        );
        assert_eq!(
            status.claimable, 0,
            "claimable should be 0 after cancel pays out"
        );
    }

    /// Verifies transfer_admin() emits an "admin_transferred" event with the correct
    /// old and new admin addresses in the data payload.
    #[test]
    fn test_event_admin_transferred_emitted_with_correct_addresses() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        let new_admin = Address::generate(&env);
        client.transfer_admin(&new_admin);

        let events = env.events().all();
        let (_, topics, data) = events
            .iter()
            .find(|(_, topics, _)| {
                topics.len() == 1
                    && Symbol::try_from_val(&env, &topics.get(0).unwrap())
                        .map(|s| s == Symbol::new(&env, "admin_transferred"))
                        .unwrap_or(false)
            })
            .expect("admin_transferred event not found");

        let topic_sym = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic_sym, Symbol::new(&env, "admin_transferred"));

        let (got_old_admin, got_new_admin) =
            <(Address, Address)>::try_from_val(&env, &data).unwrap();
        assert_eq!(got_old_admin, admin);
        assert_eq!(got_new_admin, new_admin);
    }

    /// Verifies that no "admin_transferred" event is emitted when transfer_admin() fails
    /// (e.g. SameAdmin case).
    #[test]
    fn test_event_admin_transferred_not_emitted_on_failure() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        // Attempt to transfer to the same admin — should fail with SameAdmin
        let result = client.try_transfer_admin(&admin);
        assert_eq!(result, Err(Ok(VestingError::SameAdmin)));

        // No admin_transferred event should have been emitted
        let events = env.events().all();
        let found = events.iter().any(|(_, topics, _)| {
            topics.len() == 1
                && Symbol::try_from_val(&env, &topics.get(0).unwrap())
                    .map(|s| s == Symbol::new(&env, "admin_transferred"))
                    .unwrap_or(false)
        });
        assert!(
            !found,
            "admin_transferred event should not be emitted on failure"
        );
    }

    /// Verifies claimable amount at exactly cliff_seconds is (total * cliff) / duration.
    /// Uses total=10_000, cliff=100, duration=1000 → expected 1_000.
    #[test]
    fn test_claimable_at_exactly_cliff_is_correct() {
        const TOTAL: i128 = 10_000;
        const CLIFF: u64 = 100;
        const DURATION: u64 = 1000;

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &CLIFF, &DURATION);

        // t=100: 10_000 * 100 / 1000 = 1_000
        env.ledger().with_mut(|l| l.timestamp = 100);
        assert_eq!(client.get_status().claimable, 1_000);
        assert_eq!(client.claim(), 1_000);
    }

    /// Verifies claimable amount at cliff_seconds+1 is (total * (cliff+1)) / duration.
    /// Uses total=10_000, cliff=100, duration=1000 → expected 1_010.
    #[test]
    fn test_claimable_at_cliff_plus_one_is_correct() {
        const TOTAL: i128 = 10_000;
        const CLIFF: u64 = 100;
        const DURATION: u64 = 1000;

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &CLIFF, &DURATION);

        // t=101: 10_000 * 101 / 1000 = 1_010
        env.ledger().with_mut(|l| l.timestamp = 101);
        assert_eq!(client.get_status().claimable, 1_010);
        assert_eq!(client.claim(), 1_010);
    }

    /// Verifies claimable amount at duration_seconds-1 is (total * (duration-1)) / duration.
    /// Uses total=10_000, cliff=100, duration=1000 → expected 9_990 (truncated).
    #[test]
    fn test_claimable_at_duration_minus_one_is_correct() {
        const TOTAL: i128 = 10_000;
        const CLIFF: u64 = 100;
        const DURATION: u64 = 1000;

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &CLIFF, &DURATION);

        // t=999: 10_000 * 999 / 1000 = 9_990
        env.ledger().with_mut(|l| l.timestamp = 999);
        assert_eq!(client.get_status().claimable, 9_990);
        assert_eq!(client.claim(), 9_990);
    }

    /// Verifies claimable amount at exactly duration_seconds equals total_amount.
    /// Uses total=10_000, cliff=100, duration=1000 → expected 10_000.
    #[test]
    fn test_claimable_at_full_duration_is_total_amount() {
        const TOTAL: i128 = 10_000;
        const CLIFF: u64 = 100;
        const DURATION: u64 = 1000;

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &CLIFF, &DURATION);

        // t=1000: fully vested → 10_000
        env.ledger().with_mut(|l| l.timestamp = 1000);
        assert_eq!(client.get_status().claimable, TOTAL);
        assert_eq!(client.claim(), TOTAL);
    }

    /// Verifies that claiming at duration-1 then duration yields 9_990 + 10 = 10_000,
    /// recovering the truncated remainder with no tokens lost.
    #[test]
    fn test_sequential_claim_duration_minus_one_then_duration_sums_to_total() {
        const TOTAL: i128 = 10_000;
        const CLIFF: u64 = 100;
        const DURATION: u64 = 1000;

        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);

        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &TOTAL, &CLIFF, &DURATION);

        // t=999: 10_000 * 999 / 1000 = 9_990
        env.ledger().with_mut(|l| l.timestamp = 999);
        let first = client.claim();
        assert_eq!(first, 9_990);

        // t=1000: fully vested, 10 remaining (truncated dust recovered)
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let second = client.claim();
        assert_eq!(second, 10);

        assert_eq!(first + second, TOTAL);
    }
}
