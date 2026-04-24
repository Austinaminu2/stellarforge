use soroban_sdk::{contract, contractimpl, token, Address, Env, Symbol};
use crate::storage::DataKey;
use crate::types::{VestingConfig, VestingStatus, VestingSchedule};
use crate::errors::VestingError;

#[contract]
pub struct ForgeVesting;

#[contractimpl]
impl ForgeVesting {
    /// Initialize a new vesting schedule.
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
            return Err(VestingError::AlreadyInitialized);
        }
        if total_amount <= 0 || duration_seconds == 0 || cliff_seconds > duration_seconds {
            return Err(VestingError::InvalidConfig);
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
            return Err(VestingError::Paused);
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

        let to_beneficiary = vested - claimed;
        let to_admin = config.total_amount - vested;

        config.cancelled = true;
        env.storage().instance().set(&DataKey::Config, &config);
        env.storage()
            .instance()
            .set(&DataKey::VestedAtCancel, &vested);
        env.storage().instance().set(&DataKey::Claimed, &vested);

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
            (Symbol::new(&env, "vesting_cancelled"),),
            (&config.admin, to_admin, &config.beneficiary, to_beneficiary),
        );

        Ok(())
    }

    /// Atomically claim all vested tokens for the beneficiary and return unvested tokens to the admin.
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
            return Err(VestingError::Paused);
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
    pub fn get_config(env: Env) -> Result<VestingConfig, VestingError> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(VestingError::NotInitialized)
    }

    /// Return the vesting schedule parameters.
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

    /// Pause the vesting schedule.
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
            return Err(VestingError::Paused);
        }

        config.paused = true;
        config.paused_at = Some(env.ledger().timestamp());
        env.storage().instance().set(&DataKey::Config, &config);

        Ok(())
    }

    /// Unpause the vesting schedule.
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
            return Err(VestingError::NotPaused);
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

    // ── Internal ──────────────────────────────────────────────────────────────

    fn get_claimed(env: &Env) -> i128 {
        env.storage().instance().get(&DataKey::Claimed).unwrap_or(0)
    }

    fn compute_vested(config: &VestingConfig, now: u64) -> i128 {
        if config.cancelled {
            return 0;
        }
        let effective_now = if config.paused { config.paused_at.unwrap_or(now) } else { now };
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
