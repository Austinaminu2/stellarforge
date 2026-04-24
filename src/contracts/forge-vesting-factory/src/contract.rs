use soroban_sdk::{contract, contractimpl, token, Address, Env, Symbol};
use crate::storage::DataKey;
use crate::types::{ScheduleConfig, VestingStatus};
use crate::errors::FactoryError;

#[contract]
pub struct ForgeVestingFactory;

#[contractimpl]
impl ForgeVestingFactory {
    /// Create a new vesting schedule and return its `schedule_id`.
    pub fn create_schedule(
        env: Env,
        token: Address,
        beneficiary: Address,
        admin: Address,
        total_amount: i128,
        cliff_seconds: u64,
        duration_seconds: u64,
    ) -> Result<u64, FactoryError> {
        admin.require_auth();

        if total_amount <= 0 || duration_seconds == 0 || cliff_seconds > duration_seconds {
            return Err(FactoryError::InvalidConfig);
        }

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ScheduleCount)
            .unwrap_or(0);

        let config = ScheduleConfig {
            token: token.clone(),
            beneficiary,
            admin,
            total_amount,
            start_time: env.ledger().timestamp(),
            cliff_seconds,
            duration_seconds,
            cancelled: false,
        };

        // Pull tokens from admin into the contract
        token::Client::new(&env, &token).transfer(
            &config.admin,
            &env.current_contract_address(),
            &total_amount,
        );

        env.storage()
            .persistent()
            .set(&DataKey::Schedule(id), &config);
        env.storage()
            .instance()
            .set(&DataKey::ScheduleCount, &(id + 1));

        env.events()
            .publish((Symbol::new(&env, "schedule_created"),), (id, total_amount));

        Ok(id)
    }

    /// Claim all currently vested and unclaimed tokens for a schedule.
    pub fn claim(env: Env, schedule_id: u64) -> Result<i128, FactoryError> {
        let config: ScheduleConfig = env
            .storage()
            .persistent()
            .get(&DataKey::Schedule(schedule_id))
            .ok_or(FactoryError::ScheduleNotFound)?;

        config.beneficiary.require_auth();

        if config.cancelled {
            return Err(FactoryError::Cancelled);
        }

        let now = env.ledger().timestamp();
        let vested = Self::compute_vested(&config, now);
        let claimed: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Claimed(schedule_id))
            .unwrap_or(0);

        let elapsed = now.saturating_sub(config.start_time);
        if elapsed < config.cliff_seconds {
            return Err(FactoryError::CliffNotReached);
        }

        let claimable = (vested - claimed).max(0);
        if claimable == 0 {
            return Err(FactoryError::NothingToClaim);
        }

        env.storage()
            .persistent()
            .set(&DataKey::Claimed(schedule_id), &(claimed + claimable));

        token::Client::new(&env, &config.token).transfer(
            &env.current_contract_address(),
            &config.beneficiary,
            &claimable,
        );

        env.events()
            .publish((Symbol::new(&env, "claimed"),), (schedule_id, claimable));

        Ok(claimable)
    }

    /// Cancel a vesting schedule.
    pub fn cancel(env: Env, schedule_id: u64) -> Result<(), FactoryError> {
        let mut config: ScheduleConfig = env
            .storage()
            .persistent()
            .get(&DataKey::Schedule(schedule_id))
            .ok_or(FactoryError::ScheduleNotFound)?;

        config.admin.require_auth();

        if config.cancelled {
            return Err(FactoryError::Cancelled);
        }

        let now = env.ledger().timestamp();
        let vested = Self::compute_vested(&config, now);
        let claimed: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Claimed(schedule_id))
            .unwrap_or(0);

        let token = token::Client::new(&env, &config.token);

        let beneficiary_amount = (vested - claimed).max(0);
        if beneficiary_amount > 0 {
            token.transfer(
                &env.current_contract_address(),
                &config.beneficiary,
                &beneficiary_amount,
            );
        }

        let admin_amount = (config.total_amount - vested).max(0);
        if admin_amount > 0 {
            token.transfer(
                &env.current_contract_address(),
                &config.admin,
                &admin_amount,
            );
        }

        config.cancelled = true;
        env.storage()
            .persistent()
            .set(&DataKey::Schedule(schedule_id), &config);

        env.events()
            .publish((Symbol::new(&env, "schedule_cancelled"),), (schedule_id,));

        Ok(())
    }

    /// Return the current vesting status for a schedule.
    pub fn get_status(env: Env, schedule_id: u64) -> Result<VestingStatus, FactoryError> {
        let config: ScheduleConfig = env
            .storage()
            .persistent()
            .get(&DataKey::Schedule(schedule_id))
            .ok_or(FactoryError::ScheduleNotFound)?;

        let now = env.ledger().timestamp();
        let vested = Self::compute_vested(&config, now);
        let claimed: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Claimed(schedule_id))
            .unwrap_or(0);

        let elapsed = now.saturating_sub(config.start_time);
        let claimable = if elapsed >= config.cliff_seconds {
            (vested - claimed).max(0)
        } else {
            0
        };

        Ok(VestingStatus {
            schedule_id,
            total_amount: config.total_amount,
            claimed,
            vested,
            claimable,
            cliff_reached: elapsed >= config.cliff_seconds,
            fully_vested: vested >= config.total_amount,
            cancelled: config.cancelled,
        })
    }

    /// Return the total number of schedules ever created.
    pub fn get_schedule_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ScheduleCount)
            .unwrap_or(0)
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn compute_vested(config: &ScheduleConfig, now: u64) -> i128 {
        if config.cancelled {
            return 0;
        }
        let elapsed = now.saturating_sub(config.start_time);
        if elapsed < config.cliff_seconds {
            return 0;
        }
        if elapsed >= config.duration_seconds {
            return config.total_amount;
        }
        (config.total_amount * elapsed as i128) / config.duration_seconds as i128
    }
}
