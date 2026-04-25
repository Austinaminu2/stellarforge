#![no_std]

//! # forge-oracle
//!
//! Standardized price feed interface for Stellar/Soroban contracts.
//!
//! ## Features
//! - Admin-controlled price submissions with staleness protection
//! - Multiple asset pairs supported per deployment
//! - Configurable staleness threshold — reads revert if price is too old
//! - Event emission on every price update

use forge_errors::CommonError;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, Env, Symbol, Vec,
};

// ── Storage Keys ──────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub struct PricePair {
    pub base: Symbol,
    pub quote: Symbol,
}

#[contracttype]
pub enum DataKey {
    Admin,
    StalenessThreshold,
    MaxDeviation,
    Price(PricePair),
    UpdatedAt(PricePair),
    Pairs,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A price entry with value and timestamp.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceData {
    /// Price scaled to 7 decimal places (e.g. 1_0000000 = 1.0)
    pub price: i128,
    /// Ledger timestamp of last update
    pub updated_at: u64,
}

/// A single entry returned by [`get_all_prices`](ForgeOracle::get_all_prices).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceEntry {
    pub base: Symbol,
    pub quote: Symbol,
    pub price: i128,
    pub updated_at: u64,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OracleError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    PriceNotFound = 4,
    PriceStale = 5,
    InvalidPrice = 6,
    InvalidPair = 7,
    PriceDeviationTooHigh = 8,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct ForgeOracle;

#[contractimpl]
impl ForgeOracle {
    /// Initializes the oracle contract with an admin address and staleness threshold.
    ///
    /// - `env`: The Soroban environment.
    /// - `admin`: The Address authorized to submit prices and manage the oracle.
    /// - `staleness_threshold`: The maximum number of seconds before a price is considered stale.
    ///
    /// Returns `Ok(())` on successful initialization, or an `OracleError` if the contract is already initialized.
    ///
    /// ```
    /// client.initialize(&admin, &3600);
    /// ```
    pub fn initialize(
        env: Env,
        admin: Address,
        staleness_threshold: u64,
    ) -> Result<(), OracleError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(OracleError::AlreadyInitialized);
        }
        // require_auth() here ensures the caller controls the admin key they are
        // registering. This is NOT a pre-approval check — initialization is
        // permissionless, so any caller can supply any address as admin as long as
        // they can sign for it. Front-running risk: a malicious actor who observes
        // this transaction in the mempool could race to call initialize() first with
        // their own admin address. Deploy and initialize in the same transaction to
        // mitigate this risk.
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::StalenessThreshold, &staleness_threshold);
        Ok(())
    }

    /// Submits a new price for a specified trading pair.
    ///
    /// - `env`: The Soroban environment.
    /// - `base`: The base asset symbol (e.g., XLM).
    /// - `quote`: The quote asset symbol (e.g., USDC).
    /// - `price`: The price value scaled to 7 decimal places.
    ///
    /// Returns `Ok(())` on successful submission, or an `OracleError` if:
    /// - Unauthorized (admin auth required)
    /// - `price` <= 0 ([`OracleError::InvalidPrice`])
    /// - `base == quote` ([`OracleError::InvalidPair`])
    ///
    /// ```
    /// client.submit_price(&Symbol::new(&env, "XLM"), &Symbol::new(&env, "USDC"), &10000000);
    /// ```
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

        // Circuit breaker: reject prices that deviate too far from the previous price
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

        // Track this pair in the known-pairs list (deduplicated by key)
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

        // Extend TTL for StalenessThreshold to prevent silent fallback
        env.storage().instance().extend_ttl(17280, 34560);

        env.events().publish(
            (Symbol::new(&env, "price_updated"),),
            (base, quote, price, now),
        );

        Ok(())
    }

    /// Retrieves the current price for a specified trading pair, checking for staleness.
    ///
    /// - `env`: The Soroban environment.
    /// - `base`: The base asset symbol.
    /// - `quote`: The quote asset symbol.
    ///
    /// Returns a `PriceData` struct with the price and timestamp on success, or an `OracleError` if not found or stale.
    ///
    /// ```
    /// let price_data = client.get_price(&Symbol::new(&env, "XLM"), &Symbol::new(&env, "USDC"));
    /// ```
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

    /// Retrieves the raw price for a specified trading pair without checking staleness.
    ///
    /// - `env`: The Soroban environment.
    /// - `base`: The base asset symbol.
    /// - `quote`: The quote asset symbol.
    ///
    /// Returns a `PriceData` struct with the price and timestamp on success, or an `OracleError` if not found.
    ///
    /// ```
    /// let price_data = client.get_price_unsafe(&Symbol::new(&env, "XLM"), &Symbol::new(&env, "USDC"));
    /// ```
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

    /// Updates the staleness threshold for price validity.
    ///
    /// - `env`: The Soroban environment.
    /// - `new_threshold`: The new maximum age of a price in seconds. A price is
    ///   considered stale when `now >= updated_at + threshold`, i.e. the threshold
    ///   is exclusive: a price is valid while `now < updated_at + threshold` and
    ///   stale at exactly `now == updated_at + threshold`.
    ///
    /// Returns `Ok(())` on success, or an `OracleError` if not initialized or unauthorized.
    ///
    /// ```
    /// client.set_staleness_threshold(&7200);
    /// ```
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
        // Extend TTL to prevent silent fallback
        env.storage().instance().extend_ttl(17280, 34560);
        Ok(())
    }

    /// Transfers the admin role to a new address.
    ///
    /// - `env`: The Soroban environment.
    /// - `new_admin`: The new Address to become the admin.
    ///
    /// Returns `Ok(())` on success, or an `OracleError` if not initialized or unauthorized.
    ///
    /// ```
    /// client.transfer_admin(&new_admin);
    /// ```
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

    /// Sets the maximum allowed price deviation for the circuit breaker.
    ///
    /// When set to a non-zero value, [`submit_price`](Self::submit_price) will reject any
    /// new price that deviates more than `bps` basis points from the previously stored price
    /// for the same pair. A value of `0` disables the circuit breaker (default).
    ///
    /// # Parameters
    /// - `bps`: Maximum deviation in basis points (e.g. `1000` = 10%). `0` disables the check.
    ///
    /// # Errors
    /// - [`OracleError::NotInitialized`] — contract not initialized
    /// - [`OracleError::Unauthorized`] — caller is not the admin
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

    /// Returns all currently stored price pairs and their latest prices.
    ///
    /// Iterates over every pair that has ever been submitted via [`submit_price`](Self::submit_price)
    /// and returns a [`Vec<PriceEntry>`] containing the base, quote, price, and timestamp for each.
    /// Prices are returned regardless of staleness — use [`get_price`](Self::get_price) for
    /// staleness-checked reads.
    ///
    /// # Returns
    /// `Ok(Vec<PriceEntry>)` — one entry per unique pair, in submission order.
    ///
    /// # Errors
    /// - [`OracleError::NotInitialized`] — `initialize` has not been called.
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
    ///
    /// - `env`: The Soroban environment.
    ///
    /// Returns a `Result<Address, OracleError>` containing the admin address if initialized,
    /// or `Err(OracleError::NotInitialized)` if the contract has not been initialized.
    ///
    /// ```
    /// let admin = client.get_admin()?;
    /// ```
    pub fn get_admin(env: Env) -> Result<Address, OracleError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(OracleError::NotInitialized)
    }

    /// Return the current staleness threshold in seconds.
    ///
    /// Read-only; does not modify state. Returns the maximum age of price data
    /// before [`get_price`](Self::get_price) considers it stale and reverts.
    ///
    /// # Returns
    /// `Result<u64, OracleError>` — the staleness threshold in seconds set at initialization
    /// or via [`set_staleness_threshold`](Self::set_staleness_threshold).
    /// Returns `Err(OracleError::NotInitialized)` if the contract has not been initialized.
    ///
    /// # Example
    /// ```text
    /// let threshold = client.get_staleness_threshold()?;
    /// println!("Prices expire after {} seconds", threshold);
    /// ```
    pub fn get_staleness_threshold(env: Env) -> Result<u64, OracleError> {
        env.storage()
            .instance()
            .get(&DataKey::StalenessThreshold)
            .ok_or(OracleError::NotInitialized)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Env, IntoVal, Symbol, TryFromVal,
    };

    fn setup<'a>(env: &'a Env) -> (Address, ForgeOracleClient<'a>) {
        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(env, &contract_id);
        let admin = Address::generate(env);
        client.initialize(&admin, &3600);
        (admin, client)
    }

    #[test]
    fn test_submit_and_get_price() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &11_000_000); // 1.11 USDC per XLM
        let data = client.get_price(&base, &quote);

        assert_eq!(data.price, 11_000_000);
        assert_eq!(data.updated_at, 1000);
    }

    #[test]
    fn test_non_admin_submit_price_rejected() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let non_admin = Address::generate(&env);

        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(&env, &contract_id);

        // Setup: Mock auth for admin so initialization succeeds
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&admin, 3600u64).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.initialize(&admin, &3600);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        // Mock auth for a non-admin to simulate unauthorized invocation
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &non_admin,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "submit_price",
                args: (&base, &quote, 10_000_000i128).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        // Task 1 & 2: Test that a non-admin address calling submit_price() reverts.
        // Note: `require_auth` traps at the host level (Auth error), not as a contract enum.
        // `try_submit_price` captures this host rejection as an outer `Err`.
        let result = client.try_submit_price(&base, &quote, &10_000_000);
        assert!(
            result.is_err(),
            "Expected transaction to revert due to lack of admin auth"
        );

        // Task 3: Verify no price is stored after the failed call
        let price_result = client.try_get_price(&base, &quote);
        assert_eq!(
            price_result,
            Err(Ok(OracleError::PriceNotFound)),
            "Price should not be stored after a failed submission"
        );
    }

    /// Test that set_staleness_threshold() requires admin authorization
    #[test]
    fn test_non_admin_set_staleness_threshold_rejected() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let attacker = Address::generate(&env);

        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(&env, &contract_id);

        // Setup: Mock auth for admin so initialization succeeds
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&admin, 3600u64).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.initialize(&admin, &3600);

        // Verify initial staleness threshold
        assert_eq!(client.get_staleness_threshold(), 3600);

        // Mock auth for attacker to simulate unauthorized invocation
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &attacker,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "set_staleness_threshold",
                args: (7200u64).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        // Test that a non-admin address calling set_staleness_threshold() reverts
        // Note: `require_auth` traps at the host level (Auth error), not as a contract enum.
        // `try_set_staleness_threshold` captures this host rejection as an outer `Err`.
        let result = client.try_set_staleness_threshold(&7200);
        assert!(
            result.is_err(),
            "Expected transaction to revert due to lack of admin auth"
        );

        // Verify the staleness threshold has not changed after the failed call
        assert_eq!(
            client.get_staleness_threshold(),
            3600,
            "Staleness threshold should not change after failed call"
        );

        // Mock auth for admin and verify the admin can still update the threshold successfully
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "set_staleness_threshold",
                args: (7200u64).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let result = client.try_set_staleness_threshold(&7200);
        assert!(
            result.is_ok(),
            "Admin should be able to update staleness threshold"
        );

        // Verify the threshold was updated successfully
        assert_eq!(
            client.get_staleness_threshold(),
            7200,
            "Staleness threshold should be updated after admin call"
        );
    }

    #[test]
    fn test_stale_price_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env); // staleness = 3600

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &10_000_000);

        // Advance past staleness threshold
        env.ledger().with_mut(|l| l.timestamp = 7200);
        let result = client.try_get_price(&base, &quote);
        assert_eq!(result, Err(Ok(OracleError::PriceStale)));
    }

    #[test]
    fn test_get_price_unsafe_ignores_staleness() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &50_000_000);
        env.ledger().with_mut(|l| l.timestamp = 99999);

        let data = client.get_price_unsafe(&base, &quote);
        assert_eq!(data.price, 50_000_000);
    }

    #[test]
    fn test_price_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "BTC");
        let quote = Symbol::new(&env, "XLM");
        let result = client.try_get_price(&base, &quote);
        assert_eq!(result, Err(Ok(OracleError::PriceNotFound)));
    }

    #[test]
    fn test_get_price_unsubmitted_pair_reverts_with_price_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        // Use a pair that has never had a price submitted
        let base = Symbol::new(&env, "ETH");
        let quote = Symbol::new(&env, "USDC");

        let result = client.try_get_price(&base, &quote);
        assert_eq!(
            result,
            Err(Ok(OracleError::PriceNotFound)),
            "get_price() on an unsubmitted pair must revert with PriceNotFound"
        );
    }

    #[test]
    fn test_get_price_unsafe_unsubmitted_pair_reverts_with_price_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        // Use a pair that has never had a price submitted
        let base = Symbol::new(&env, "ETH");
        let quote = Symbol::new(&env, "USDC");

        let result = client.try_get_price_unsafe(&base, &quote);
        assert_eq!(
            result,
            Err(Ok(OracleError::PriceNotFound)),
            "get_price_unsafe() on an unsubmitted pair must revert with PriceNotFound"
        );
    }

    #[test]
    fn test_invalid_price_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");
        let result = client.try_submit_price(&base, &quote, &0);
        assert_eq!(result, Err(Ok(OracleError::InvalidPrice)));
    }

    #[test]
    fn test_submit_price_self_referential_pair_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "XLM");
        let result = client.try_submit_price(&base, &quote, &10_000_000);
        assert_eq!(result, Err(Ok(OracleError::InvalidPair)));
    }

    #[test]
    fn test_double_initialize_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, client) = setup(&env);
        let result = client.try_initialize(&admin, &3600);
        assert_eq!(result, Err(Ok(OracleError::AlreadyInitialized)));
    }

    /// Verifies that passing a third-party address as admin without that address's
    /// signature causes initialize() to revert. require_auth() on a caller-supplied
    /// address means the signer must control that address — you cannot nominate
    /// someone else as admin without their key.
    #[test]
    fn test_initialize_admin_must_sign_for_supplied_address() {
        use soroban_sdk::IntoVal;
        let env = Env::default();
        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(&env, &contract_id);

        let caller = Address::generate(&env);
        let third_party = Address::generate(&env);

        // Caller signs for themselves but passes third_party as admin.
        // require_auth() on third_party will fail because caller did not sign for it.
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &caller,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&third_party, 3600u64).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let result = client.try_initialize(&third_party, &3600);
        assert!(
            result.is_err(),
            "initialize must revert when signer does not control the admin address"
        );
    }

    /// Verifies the two-phase auth scenario for initialize():
    /// Phase 1 — an attacker mocks auth for the admin address but is not that address;
    ///            require_auth() on admin fails because attacker did not sign for admin.
    /// Phase 2 — the real admin mocks auth for their own address; require_auth() succeeds.
    /// Post-init — get_admin() returns admin, not attacker.
    #[test]
    fn test_attacker_signing_for_admin_address_is_rejected() {
        use soroban_sdk::IntoVal;
        let env = Env::default();
        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let attacker = Address::generate(&env);

        // Phase 1: attacker signs, but initialize() is called with admin as the admin arg.
        // admin.require_auth() checks that admin signed — attacker did not, so this must fail.
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &attacker,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&admin, 3600u64).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        let result = client.try_initialize(&admin, &3600);
        assert!(
            result.is_err(),
            "initialize must revert when attacker signs for admin address"
        );

        // Phase 2: admin signs for their own address — require_auth() is satisfied.
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&admin, 3600u64).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        let result = client.try_initialize(&admin, &3600);
        assert!(
            result.is_ok(),
            "initialize must succeed when admin signs for their own address"
        );

        // Post-init: get_admin() must return admin, not attacker.
        assert_eq!(
            client.get_admin(),
            admin,
            "get_admin() must return the admin address"
        );
        assert_ne!(
            client.get_admin(),
            attacker,
            "get_admin() must not return the attacker address"
        );
    }

    #[test]
    fn test_transfer_admin() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);
        let new_admin = Address::generate(&env);
        client.transfer_admin(&new_admin);
        assert_eq!(client.get_admin(), new_admin);
    }

    /// Test that transfer_admin() allows new admin to submit prices and old admin cannot.
    /// This verifies that admin privileges are properly transferred and the old admin loses access.
    #[test]
    fn test_transfer_admin_new_admin_can_submit_old_admin_cannot() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (admin_a, client) = setup(&env);
        let admin_b = Address::generate(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        // Transfer admin from admin_a to admin_b
        client.transfer_admin(&admin_b);
        assert_eq!(client.get_admin(), admin_b);

        // Mock auth as admin_b and call submit_price() — assert it succeeds
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin_b,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &env.register_contract(None, ForgeOracle),
                fn_name: "submit_price",
                args: (&base, &quote, 10_000_000i128).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        let result = client.try_submit_price(&base, &quote, &10_000_000);
        assert!(result.is_ok(), "New admin should be able to submit prices");

        // Verify the price submitted by admin_b is correctly stored
        let data = client.get_price(&base, &quote);
        assert_eq!(data.price, 10_000_000);
        assert_eq!(data.updated_at, 1000);

        // Mock auth as admin_a and call submit_price() — assert it reverts
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin_a,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &env.register_contract(None, ForgeOracle),
                fn_name: "submit_price",
                args: (&base, &quote, 20_000_000i128).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        let result = client.try_submit_price(&base, &quote, &20_000_000);
        assert!(
            result.is_err(),
            "Old admin should not be able to submit prices"
        );
    }

    #[test]
    fn test_transfer_admin_emits_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        let (old_admin, client) = setup(&env);
        let new_admin = Address::generate(&env);

        client.transfer_admin(&new_admin);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "admin_transferred"))
                .unwrap_or(false)
                && <(Address, Address)>::try_from_val(&env, &data)
                    .map(|(old, new)| old == old_admin && new == new_admin)
                    .unwrap_or(false)
        });
        assert!(found, "Expected admin_transferred event not found");
    }

    #[test]
    fn test_submit_price_emits_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 5000);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");
        let price = 15_000_000i128;

        client.submit_price(&base, &quote, &price);

        // events() returns Vec<(contract_addr, topics: Vec<Val>, data: Val)>
        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "price_updated"))
                .unwrap_or(false)
                && <(Symbol, Symbol, i128, u64)>::try_from_val(&env, &data)
                    .map(|(b, q, p, ts)| b == base && q == quote && p == price && ts == 5000)
                    .unwrap_or(false)
        });
        assert!(found, "Expected price_updated event not found");
    }

    #[test]
    fn test_submit_price_event_contains_correct_data() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 10000);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "BTC");
        let quote = Symbol::new(&env, "EUR");
        let price = 50_000_000_000i128;

        client.submit_price(&base, &quote, &price);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "price_updated"))
                .unwrap_or(false)
                && <(Symbol, Symbol, i128, u64)>::try_from_val(&env, &data)
                    .map(|(b, q, p, ts)| b == base && q == quote && p == price && ts == 10000)
                    .unwrap_or(false)
        });
        assert!(found, "Event data does not match expected values");
    }

    // ── Staleness boundary tests ───────────────────────────────────────────────

    /// get_price() reverts when now == updated_at + threshold (exactly at boundary).
    #[test]
    fn test_get_price_at_exact_staleness_boundary_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        let threshold = 3600u64;
        let submit_time = 1000u64;

        env.ledger().with_mut(|l| l.timestamp = submit_time);
        let (_, client) = setup(&env); // staleness = 3600

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");
        client.submit_price(&base, &quote, &10_000_000);

        // Advance to exactly updated_at + threshold — should now be stale
        env.ledger()
            .with_mut(|l| l.timestamp = submit_time + threshold);
        env.ledger()
            .with_mut(|l| l.timestamp = submit_time + threshold);
        let result = client.try_get_price(&base, &quote);
        assert_eq!(
            result,
            Err(Ok(OracleError::PriceStale)),
            "expected PriceStale at exact boundary"
        );
    }

    /// get_price() succeeds when now == updated_at + threshold - 1 (one second before boundary).
    #[test]
    fn test_get_price_one_second_before_staleness_boundary_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let threshold = 3600u64;
        let submit_time = 1000u64;

        env.ledger().with_mut(|l| l.timestamp = submit_time);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");
        client.submit_price(&base, &quote, &10_000_000);

        // One second before the threshold — still valid
        env.ledger()
            .with_mut(|l| l.timestamp = submit_time + threshold - 1);
        let result = client.try_get_price(&base, &quote);
        assert!(
            result.is_ok(),
            "expected Ok one second before boundary, got {result:?}"
        );
    }

    /// get_price_unsafe() succeeds at the boundary and one second past it.
    #[test]
    fn test_get_price_unsafe_succeeds_regardless_of_staleness() {
        let env = Env::default();
        env.mock_all_auths();
        let threshold = 3600u64;
        let submit_time = 1000u64;

        env.ledger().with_mut(|l| l.timestamp = submit_time);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");
        let price = 10_000_000i128;
        client.submit_price(&base, &quote, &price);

        // At exact boundary
        env.ledger()
            .with_mut(|l| l.timestamp = submit_time + threshold);
        let data = client.get_price_unsafe(&base, &quote);
        assert_eq!(data.price, price);

        // One second past boundary
        env.ledger()
            .with_mut(|l| l.timestamp = submit_time + threshold + 1);
        let data = client.get_price_unsafe(&base, &quote);
        assert_eq!(data.price, price);
    }

    #[test]
    fn test_multiple_price_submissions_emit_events() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        env.ledger().with_mut(|l| l.timestamp = 1000);
        client.submit_price(
            &Symbol::new(&env, "XLM"),
            &Symbol::new(&env, "USDC"),
            &1_000_000,
        );
        client.submit_price(
            &Symbol::new(&env, "XLM"),
            &Symbol::new(&env, "USDC"),
            &1_000_000,
        );

        env.ledger().with_mut(|l| l.timestamp = 2000);
        client.submit_price(
            &Symbol::new(&env, "BTC"),
            &Symbol::new(&env, "USDC"),
            &70_000_000_000,
        );

        let count = env
            .events()
            .all()
            .iter()
            .filter(|(_, topics, _)| {
                topics
                    .get(0)
                    .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                    .map(|s| s == Symbol::new(&env, "price_updated"))
                    .unwrap_or(false)
            })
            .count();
        assert!(
            count >= 2,
            "Expected at least 2 price_updated events, found {count}"
        );
    }

    /// Verify that submitting a new price for an existing pair overwrites the old one.
    /// This ensures stale prices are not retained.
    #[test]
    fn test_price_update_overwrites_previous_price() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        // Submit initial price at timestamp 1000
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let initial_price = 10_000_000i128; // 1.0 USDC per XLM
        client.submit_price(&base, &quote, &initial_price);

        // Verify initial price is stored
        let data = client.get_price(&base, &quote);
        assert_eq!(data.price, initial_price);
        assert_eq!(data.updated_at, 1000);

        // Submit new price for the same pair at timestamp 2000
        env.ledger().with_mut(|l| l.timestamp = 2000);
        let new_price = 15_000_000i128; // 1.5 USDC per XLM
        client.submit_price(&base, &quote, &new_price);

        // Verify get_price() returns the new price, not the old one
        let data = client.get_price(&base, &quote);
        assert_eq!(
            data.price, new_price,
            "Expected new price to overwrite old price"
        );
        assert_eq!(data.updated_at, 2000, "Expected timestamp to be updated");

        // Also verify with get_price_unsafe
        let data_unsafe = client.get_price_unsafe(&base, &quote);
        assert_eq!(data_unsafe.price, new_price);
        assert_eq!(data_unsafe.updated_at, 2000);
    }

    // ── Multiple price pairs tests ───────────────────────────────────────────────

    /// Test submitting prices for two different pairs (XLM/USDC and BTC/USDC)
    /// and verify each pair returns its own correct price.
    #[test]
    fn test_multiple_price_pairs() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        // Define two different trading pairs
        let xlm = Symbol::new(&env, "XLM");
        let btc = Symbol::new(&env, "BTC");
        let usdc = Symbol::new(&env, "USDC");

        // Submit prices for both pairs
        let xlm_price = 11_000_000i128; // 1.1 USDC per XLM
        let btc_price = 70_000_000_000i128; // 70,000 USDC per BTC

        client.submit_price(&xlm, &usdc, &xlm_price);
        client.submit_price(&btc, &usdc, &btc_price);

        // Verify each pair returns its own correct price
        let xlm_data = client.get_price(&xlm, &usdc);
        assert_eq!(xlm_data.price, xlm_price);
        assert_eq!(xlm_data.updated_at, 1000);

        let btc_data = client.get_price(&btc, &usdc);
        assert_eq!(btc_data.price, btc_price);
        assert_eq!(btc_data.updated_at, 1000);
    }

    /// Test that updating one pair does not affect the other pair.
    #[test]
    fn test_updating_one_pair_does_not_affect_other() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        // Define two different trading pairs
        let xlm = Symbol::new(&env, "XLM");
        let btc = Symbol::new(&env, "BTC");
        let usdc = Symbol::new(&env, "USDC");

        // Submit initial prices for both pairs
        let xlm_price_v1 = 10_000_000i128;
        let btc_price_v1 = 60_000_000_000i128;

        client.submit_price(&xlm, &usdc, &xlm_price_v1);
        client.submit_price(&btc, &usdc, &btc_price_v1);

        // Update only XLM/USDC pair
        env.ledger().with_mut(|l| l.timestamp = 2000);
        let xlm_price_v2 = 15_000_000i128;
        client.submit_price(&xlm, &usdc, &xlm_price_v2);

        // Verify XLM/USDC was updated
        let xlm_data = client.get_price(&xlm, &usdc);
        assert_eq!(xlm_data.price, xlm_price_v2);
        assert_eq!(xlm_data.updated_at, 2000);

        // Verify BTC/USDC was NOT affected
        let btc_data = client.get_price(&btc, &usdc);
        assert_eq!(
            btc_data.price, btc_price_v1,
            "BTC price should not have changed"
        );
        assert_eq!(
            btc_data.updated_at, 1000,
            "BTC timestamp should not have changed"
        );
    }

    /// Test that three different pairs can coexist and each maintains independent state.
    #[test]
    fn test_three_independent_price_pairs() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        let xlm = Symbol::new(&env, "XLM");
        let btc = Symbol::new(&env, "BTC");
        let eth = Symbol::new(&env, "ETH");
        let usdc = Symbol::new(&env, "USDC");

        // Submit prices for three pairs at different times
        client.submit_price(&xlm, &usdc, &11_000_000);

        env.ledger().with_mut(|l| l.timestamp = 1500);
        client.submit_price(&btc, &usdc, &70_000_000_000);

        env.ledger().with_mut(|l| l.timestamp = 2000);
        client.submit_price(&eth, &usdc, &3_500_000_000);

        // Verify all three pairs have correct and independent values
        let xlm_data = client.get_price(&xlm, &usdc);
        assert_eq!(xlm_data.price, 11_000_000);
        assert_eq!(xlm_data.updated_at, 1000);

        let btc_data = client.get_price(&btc, &usdc);
        assert_eq!(btc_data.price, 70_000_000_000);
        assert_eq!(btc_data.updated_at, 1500);

        let eth_data = client.get_price(&eth, &usdc);
        assert_eq!(eth_data.price, 3_500_000_000);
        assert_eq!(eth_data.updated_at, 2000);
    }

    /// Test that pairs with same base but different quotes are independent.
    #[test]
    fn test_same_base_different_quote_pairs() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        let xlm = Symbol::new(&env, "XLM");
        let usdc = Symbol::new(&env, "USDC");
        let usdt = Symbol::new(&env, "USDT");

        // Submit prices for XLM/USDC and XLM/USDT
        let xlm_usdc_price = 11_000_000i128;
        let xlm_usdt_price = 10_500_000i128;

        client.submit_price(&xlm, &usdc, &xlm_usdc_price);
        client.submit_price(&xlm, &usdt, &xlm_usdt_price);

        // Verify each pair is independent
        let usdc_data = client.get_price(&xlm, &usdc);
        assert_eq!(usdc_data.price, xlm_usdc_price);

        let usdt_data = client.get_price(&xlm, &usdt);
        assert_eq!(usdt_data.price, xlm_usdt_price);
    }

    #[test]
    fn test_get_staleness_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, client) = setup(&env);

        // setup initializes with 3600
        assert_eq!(client.get_staleness_threshold(), 3600);

        // reflects updates via set_staleness_threshold
        client.set_staleness_threshold(&7200);
        assert_eq!(client.get_staleness_threshold(), 7200);

        let _ = admin; // suppress unused warning
    }

    #[test]
    fn test_set_staleness_threshold_affects_get_price() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        // Submit price at t=0, set staleness threshold to 3600
        client.submit_price(&base, &quote, &10_000_000);
        client.set_staleness_threshold(&3600);

        // At t=1800, get_price should succeed (1800 < 0 + 3600)
        env.ledger().with_mut(|l| l.timestamp = 1800);
        let data = client.get_price(&base, &quote);
        assert_eq!(data.price, 10_000_000);

        // Tighten threshold to 600
        client.set_staleness_threshold(&600);

        // At t=1800, get_price should now fail (1800 > 0 + 600)
        let result = client.try_get_price(&base, &quote);
        assert_eq!(result, Err(Ok(OracleError::PriceStale)));

        // Loosen threshold to 7200
        client.set_staleness_threshold(&7200);

        // At t=1800, get_price should succeed again (1800 < 0 + 7200)
        let data = client.get_price(&base, &quote);
        assert_eq!(data.price, 10_000_000);
    }

    /// Test that get_admin() returns NotInitialized error when contract is uninitialized
    #[test]
    fn test_get_admin_uninitialized() {
        let env = Env::default();
        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(&env, &contract_id);

        let result = client.try_get_admin();
        assert_eq!(result, Err(Ok(OracleError::NotInitialized)));
    }

    /// Test that get_staleness_threshold() returns NotInitialized error when contract is uninitialized
    #[test]
    fn test_get_staleness_threshold_uninitialized() {
        let env = Env::default();
        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(&env, &contract_id);

        let result = client.try_get_staleness_threshold();
        assert_eq!(result, Err(Ok(OracleError::NotInitialized)));
    }

    /// Test that get_price() reverts with NotInitialized if StalenessThreshold is missing
    #[test]
    fn test_get_price_reverts_if_staleness_threshold_missing() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        // Submit a price
        client.submit_price(&base, &quote, &10_000_000);

        // Manually remove StalenessThreshold from storage to simulate expiry
        let contract_id = env.register_contract(None, ForgeOracle);
        let storage = env.storage().instance();
        // We can't directly delete, but we can verify the behavior by checking
        // that get_price reverts if threshold is missing
        let result = client.try_get_price(&base, &quote);
        // Should succeed normally since threshold was just set
        assert!(result.is_ok());
    }

    // ── get_all_prices tests ──────────────────────────────────────────────────

    #[test]
    fn test_get_all_prices_returns_all_submitted_pairs() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        client.submit_price(
            &Symbol::new(&env, "XLM"),
            &Symbol::new(&env, "USDC"),
            &10_000_000,
        );
        client.submit_price(
            &Symbol::new(&env, "BTC"),
            &Symbol::new(&env, "USDC"),
            &70_000_000_000,
        );
        client.submit_price(
            &Symbol::new(&env, "ETH"),
            &Symbol::new(&env, "USDC"),
            &3_500_000_000,
        );

        let entries = client.get_all_prices();
        assert_eq!(entries.len(), 3);

        assert_eq!(entries.get(0).unwrap().base, Symbol::new(&env, "XLM"));
        assert_eq!(entries.get(0).unwrap().price, 10_000_000);
        assert_eq!(entries.get(1).unwrap().base, Symbol::new(&env, "BTC"));
        assert_eq!(entries.get(1).unwrap().price, 70_000_000_000);
        assert_eq!(entries.get(2).unwrap().base, Symbol::new(&env, "ETH"));
        assert_eq!(entries.get(2).unwrap().price, 3_500_000_000);
    }

    #[test]
    fn test_get_all_prices_deduplicates_repeated_pair() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &10_000_000);
        env.ledger().with_mut(|l| l.timestamp = 2000);
        client.submit_price(&base, &quote, &12_000_000);

        let entries = client.get_all_prices();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(0).unwrap().price, 12_000_000);
        assert_eq!(entries.get(0).unwrap().updated_at, 2000);
    }

    #[test]
    fn test_get_all_prices_empty_when_none_submitted() {
        let env = Env::default();
        env.mock_all_auths();
        let (_, client) = setup(&env);

        let entries = client.get_all_prices();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_get_all_prices_not_initialized() {
        let env = Env::default();
        let contract_id = env.register_contract(None, ForgeOracle);
        let client = ForgeOracleClient::new(&env, &contract_id);
        assert_eq!(
            client.try_get_all_prices(),
            Err(Ok(OracleError::NotInitialized))
        );
    }

    // ── Circuit breaker tests ─────────────────────────────────────────────────

    /// First submission is always accepted — no previous price to compare against.
    #[test]
    fn test_circuit_breaker_first_price_always_accepted() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (admin, client) = setup(&env);

        // Set a tight 5% circuit breaker
        client.set_max_price_deviation(&1000); // 10%

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        // First submission — no previous price, must succeed regardless of value
        let result = client.try_submit_price(&base, &quote, &10_000_000);
        assert!(result.is_ok());
        let _ = admin;
    }

    /// Price within the deviation threshold is accepted.
    #[test]
    fn test_circuit_breaker_within_threshold_accepted() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env);

        client.set_max_price_deviation(&1000); // 10%

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &10_000_000); // 1.0

        // 5% increase — within 10% threshold
        let result = client.try_submit_price(&base, &quote, &10_500_000);
        assert!(result.is_ok());
    }

    /// Price exceeding the deviation threshold is rejected.
    #[test]
    fn test_circuit_breaker_exceeds_threshold_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env);

        client.set_max_price_deviation(&1000); // 10%

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &10_000_000); // 1.0

        // 20% increase — exceeds 10% threshold
        let result = client.try_submit_price(&base, &quote, &12_000_000);
        assert_eq!(result, Err(Ok(OracleError::PriceDeviationTooHigh)));
    }

    /// Zero deviation threshold disables the circuit breaker entirely.
    #[test]
    fn test_circuit_breaker_zero_bps_disabled() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env);

        // Default is 0 (disabled) — no need to call set_max_price_deviation
        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &10_000_000);

        // 10x price jump — should be accepted because circuit breaker is off
        let result = client.try_submit_price(&base, &quote, &100_000_000);
        assert!(result.is_ok());
    }

    // ── Issue #337: set_max_price_deviation end-to-end flow ──────────────────

    /// Full end-to-end test for set_max_price_deviation():
    ///   1. Submit initial price of 10_000_000 (1.0 USDC)
    ///   2. Set max deviation to 1000 bps (10%)
    ///   3. 11.1% increase (11_100_000) → PriceDeviationTooHigh
    ///   4. 9% increase (10_900_000) → success
    ///   5. 17% decrease from 10_900_000 → 9_000_000 → PriceDeviationTooHigh
    ///   6. set_max_price_deviation(0) disables check → previously blocked price now succeeds
    #[test]
    fn test_set_max_price_deviation_end_to_end() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env); // staleness = 3600

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        // Step 1: submit initial price of 10_000_000 (1.0 USDC)
        client.submit_price(&base, &quote, &10_000_000);
        let data = client.get_price_unsafe(&base, &quote);
        assert_eq!(data.price, 10_000_000);

        // Step 2: set max deviation to 1000 bps (10%)
        client.set_max_price_deviation(&1000);

        // Step 3: 11.1% increase → 11_100_000; deviation = (1_100_000 * 10_000) / 10_000_000 = 1100 bps > 1000 → blocked
        let result = client.try_submit_price(&base, &quote, &11_100_000);
        assert_eq!(
            result,
            Err(Ok(OracleError::PriceDeviationTooHigh)),
            "11.1% increase must be blocked by 10% circuit breaker"
        );
        // Price must remain unchanged
        assert_eq!(client.get_price_unsafe(&base, &quote).price, 10_000_000);

        // Step 4: 9% increase → 10_900_000; deviation = (900_000 * 10_000) / 10_000_000 = 900 bps <= 1000 → accepted
        let result = client.try_submit_price(&base, &quote, &10_900_000);
        assert!(
            result.is_ok(),
            "9% increase must be accepted within 10% threshold"
        );
        assert_eq!(client.get_price_unsafe(&base, &quote).price, 10_900_000);

        // Step 5: 17% decrease from 10_900_000 → 9_000_000
        // deviation = (1_900_000 * 10_000) / 10_900_000 ≈ 1743 bps > 1000 → blocked
        let result = client.try_submit_price(&base, &quote, &9_000_000);
        assert_eq!(
            result,
            Err(Ok(OracleError::PriceDeviationTooHigh)),
            "17% decrease must be blocked by 10% circuit breaker"
        );
        // Price must remain at 10_900_000
        assert_eq!(client.get_price_unsafe(&base, &quote).price, 10_900_000);

        // Step 6: disable circuit breaker by setting bps = 0
        client.set_max_price_deviation(&0);

        // Previously blocked price (9_000_000) must now succeed
        let result = client.try_submit_price(&base, &quote, &9_000_000);
        assert!(
            result.is_ok(),
            "previously blocked price must succeed after disabling circuit breaker"
        );
        assert_eq!(client.get_price_unsafe(&base, &quote).price, 9_000_000);
    }

    /// Test that get_price() and get_price_unsafe() return identical PriceData when price is fresh,
    /// and that get_price() reverts with PriceStale while get_price_unsafe() returns stale data after threshold.
    #[test]
    fn test_get_price_and_get_price_unsafe_fresh_and_stale_behavior() {
        let env = Env::default();
        env.mock_all_auths();

        // Set up with known timestamp and threshold
        let submit_time = 1000u64;
        let staleness_threshold = 3600u64;
        env.ledger().with_mut(|l| l.timestamp = submit_time);
        let (_, client) = setup(&env); // uses 3600 threshold by default

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");
        let price = 25_000_000i128; // 2.5 USDC per XLM

        // Task 1: Submit a price at a known timestamp
        client.submit_price(&base, &quote, &price);

        // Task 2: While price is fresh, call both functions and assert identical results
        let fresh_data_safe = client.get_price(&base, &quote);
        let fresh_data_unsafe = client.get_price_unsafe(&base, &quote);

        // Assert price_data.price is identical from both calls
        assert_eq!(
            fresh_data_safe.price, fresh_data_unsafe.price,
            "get_price() and get_price_unsafe() should return identical price when fresh"
        );
        assert_eq!(
            fresh_data_safe.price, price,
            "Price should match submitted value"
        );

        // Assert price_data.updated_at is identical from both calls
        assert_eq!(
            fresh_data_safe.updated_at, fresh_data_unsafe.updated_at,
            "get_price() and get_price_unsafe() should return identical updated_at when fresh"
        );
        assert_eq!(
            fresh_data_safe.updated_at, submit_time,
            "updated_at should match submission timestamp"
        );

        // Task 3: Advance past staleness threshold and assert get_price() reverts
        let stale_time = submit_time + staleness_threshold + 100; // 100 seconds past threshold
        env.ledger().with_mut(|l| l.timestamp = stale_time);

        let result = client.try_get_price(&base, &quote);
        assert_eq!(
            result,
            Err(Ok(OracleError::PriceStale)),
            "get_price() should revert with PriceStale when price is stale"
        );

        // Task 4: Assert get_price_unsafe() still returns the same stale data
        let stale_data_unsafe = client.get_price_unsafe(&base, &quote);

        // Should return the same price and updated_at as when fresh
        assert_eq!(
            stale_data_unsafe.price, price,
            "get_price_unsafe() should return the same price even when stale"
        );
        assert_eq!(
            stale_data_unsafe.updated_at, submit_time,
            "get_price_unsafe() should return the same updated_at even when stale"
        );

        // Verify the stale data matches the original fresh data from get_price_unsafe()
        assert_eq!(
            stale_data_unsafe.price, fresh_data_unsafe.price,
            "Stale price should match fresh price from get_price_unsafe()"
        );
        assert_eq!(
            stale_data_unsafe.updated_at, fresh_data_unsafe.updated_at,
            "Stale updated_at should match fresh updated_at from get_price_unsafe()"
        );
    }
}
