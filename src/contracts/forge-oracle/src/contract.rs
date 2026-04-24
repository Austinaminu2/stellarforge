use soroban_sdk::{contract, contractimpl, vec, Address, Env, Symbol, Vec};
use crate::storage::{DataKey, PricePair};
use crate::types::{PriceData, PriceEntry};
use crate::errors::OracleError;

#[contract]
pub struct ForgeOracle;

#[contractimpl]
impl ForgeOracle {
    /// Initializes the oracle contract.
    pub fn initialize(
        env: Env,
        admin: Address,
        staleness_threshold: u64,
    ) -> Result<(), OracleError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(OracleError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::StalenessThreshold, &staleness_threshold);
        Ok(())
    }

    /// Submits a new price for a specified trading pair.
    pub fn submit_price(
        env: Env,
        base: Symbol,
        quote: Symbol,
        price: i128,
    ) -> Result<(), OracleError> {
        if base == quote {
            return Err(OracleError::InvalidPair);
        }

        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(OracleError::NotInitialized)?;

        admin.require_auth();

        if price <= 0 {
            return Err(OracleError::InvalidPrice);
        }

        let pair = PricePair {
            base: base.clone(),
            quote: quote.clone(),
        };
        let now = env.ledger().timestamp();

        let max_deviation_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxDeviation)
            .unwrap_or(0u32);
        if max_deviation_bps > 0 {
            if let Some(prev_price) = env
                .storage()
                .persistent()
                .get::<DataKey, i128>(&DataKey::Price(pair.clone()))
            {
                if prev_price > 0 {
                    let deviation = (price - prev_price).abs() * 10_000 / prev_price;
                    if deviation > max_deviation_bps as i128 {
                        return Err(OracleError::PriceDeviationTooHigh);
                    }
                }
            }
        }

        let pair_key = PricePair {
            base: base.clone(),
            quote: quote.clone(),
        };
        if !env.storage().persistent().has(&DataKey::Price(pair_key)) {
            let mut pairs: Vec<PricePair> = env
                .storage()
                .persistent()
                .get(&DataKey::Pairs)
                .unwrap_or_else(|| vec![&env]);
            pairs.push_back(PricePair {
                base: base.clone(),
                quote: quote.clone(),
            });
            env.storage().persistent().set(&DataKey::Pairs, &pairs);
            env.storage().persistent().extend_ttl(&DataKey::Pairs, 17280, 34560);
        }

        env.storage()
            .persistent()
            .set(&DataKey::Price(pair.clone()), &price);
        env.storage()
            .persistent()
            .set(&DataKey::UpdatedAt(pair), &now);

        env.storage().instance().extend_ttl(17280, 34560);

        env.events().publish(
            (Symbol::new(&env, "price_updated"),),
            (base, quote, price, now),
        );

        Ok(())
    }

    /// Retrieves the current price.
    pub fn get_price(env: Env, base: Symbol, quote: Symbol) -> Result<PriceData, OracleError> {
        let pair = PricePair { base, quote };

        let price: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Price(pair.clone()))
            .ok_or(OracleError::PriceNotFound)?;

        let updated_at: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::UpdatedAt(pair))
            .ok_or(OracleError::PriceNotFound)?;

        let threshold: u64 = env
            .storage()
            .instance()
            .get(&DataKey::StalenessThreshold)
            .ok_or(OracleError::NotInitialized)?;

        let now = env.ledger().timestamp();
        if now >= updated_at + threshold {
            return Err(OracleError::PriceStale);
        }

        Ok(PriceData { price, updated_at })
    }

    /// Retrieves the raw price without staleness check.
    pub fn get_price_unsafe(
        env: Env,
        base: Symbol,
        quote: Symbol,
    ) -> Result<PriceData, OracleError> {
        let pair = PricePair { base, quote };

        let price: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Price(pair.clone()))
            .ok_or(OracleError::PriceNotFound)?;

        let updated_at: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::UpdatedAt(pair))
            .ok_or(OracleError::PriceNotFound)?;

        Ok(PriceData { price, updated_at })
    }

    /// Updates the staleness threshold.
    pub fn set_staleness_threshold(env: Env, new_threshold: u64) -> Result<(), OracleError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(OracleError::NotInitialized)?;
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::StalenessThreshold, &new_threshold);
        env.storage().instance().extend_ttl(17280, 34560);
        Ok(())
    }

    /// Transfers the admin role.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), OracleError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(OracleError::NotInitialized)?;
        admin.require_auth();
        let old_admin = admin.clone();
        env.storage().instance().set(&DataKey::Admin, &new_admin);

        env.events().publish(
            (Symbol::new(&env, "admin_transferred"),),
            (old_admin, new_admin),
        );

        Ok(())
    }

    /// Sets the maximum allowed price deviation.
    pub fn set_max_price_deviation(env: Env, bps: u32) -> Result<(), OracleError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(OracleError::NotInitialized)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::MaxDeviation, &bps);
        env.storage().instance().extend_ttl(17280, 34560);
        Ok(())
    }

    /// Returns all currently stored prices.
    pub fn get_all_prices(env: Env) -> Result<Vec<PriceEntry>, OracleError> {
        if !env.storage().instance().has(&DataKey::Admin) {
            return Err(OracleError::NotInitialized);
        }
        let pairs: Vec<PricePair> = env
            .storage()
            .persistent()
            .get(&DataKey::Pairs)
            .unwrap_or_else(|| vec![&env]);
        let mut result: Vec<PriceEntry> = vec![&env];
        for pair in pairs.iter() {
            let price: i128 = match env
                .storage()
                .persistent()
                .get(&DataKey::Price(pair.clone()))
            {
                Some(p) => p,
                None => continue,
            };
            let updated_at: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::UpdatedAt(pair.clone()))
                .unwrap_or(0);
            result.push_back(PriceEntry {
                base: pair.base.clone(),
                quote: pair.quote.clone(),
                price,
                updated_at,
            });
        }
        Ok(result)
    }

    /// Retrieves the current admin address.
    pub fn get_admin(env: Env) -> Result<Address, OracleError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(OracleError::NotInitialized)
    }

    /// Return the current staleness threshold.
    pub fn get_staleness_threshold(env: Env) -> Result<u64, OracleError> {
        env.storage()
            .instance()
            .get(&DataKey::StalenessThreshold)
            .ok_or(OracleError::NotInitialized)
    }
}
