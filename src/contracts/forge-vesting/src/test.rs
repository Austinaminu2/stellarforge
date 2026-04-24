#[cfg(test)]
mod tests {
    extern crate std;
    use crate::contract::{ForgeVesting, ForgeVestingClient};
    use crate::errors::VestingError;
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
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1000, &0, &3);

        env.mock_all_auths();
        let start_ts = env.ledger().timestamp();

        // t=1
        env.ledger().with_mut(|l| l.timestamp = start_ts + 1);
        let v1 = client.claim();
        assert_eq!(v1, 333); 

        // t=2
        env.ledger().with_mut(|l| l.timestamp = start_ts + 2);
        let v2 = client.claim();
        assert_eq!(v2, 333); 

        // t=3
        env.ledger().with_mut(|l| l.timestamp = start_ts + 3);
        let v3 = client.claim();
        assert_eq!(v3, 334); 

        assert_eq!(v1 + v2 + v3, 1000);
    }

    #[test]
    fn test_double_initialize_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);

        let result = client.try_initialize(&token, &Address::generate(&env), &Address::generate(&env), &9_999_999, &500, &5000);
        assert_eq!(result, Err(Ok(VestingError::AlreadyInitialized)));
    }

    #[test]
    fn test_claim_before_cliff_fails() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &500, &1000);
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
    }

    #[test]
    fn test_get_vesting_schedule_returns_init_params() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &2_500_000, &200, &5000);
        let schedule = client.get_vesting_schedule();
        assert_eq!(schedule.total_amount, 2_500_000);
        assert_eq!(schedule.cliff_seconds, 200);
        assert_eq!(schedule.duration_seconds, 5000);
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

    #[test]
    fn test_get_status_after_partial_claim_then_time_advance() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        env.ledger().with_mut(|l| l.timestamp = 0);
        client.initialize(&token_id, &beneficiary, &admin, &10_000, &0, &1000);

        env.ledger().with_mut(|l| l.timestamp = 200);
        client.claim();

        let s = client.get_status();
        assert_eq!(s.vested, 2_000);
        assert_eq!(s.claimed, 2_000);
        assert_eq!(s.claimable, 0);

        env.ledger().with_mut(|l| l.timestamp = 500);
        let s = client.get_status();
        assert_eq!(s.vested, 5_000);
        assert_eq!(s.claimed, 2_000);
        assert_eq!(s.claimable, 3_000);
    }

    #[test]
    fn test_cancel_before_cliff_beneficiary_gets_zero_admin_gets_all() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &500, &1000);
        env.ledger().with_mut(|l| l.timestamp += 100);
        client.cancel();
        let tc = soroban_sdk::token::Client::new(&env, &token_id);
        assert_eq!(tc.balance(&beneficiary), 0);
        assert_eq!(tc.balance(&admin), 1_000_000);
    }

    #[test]
    fn test_transfer_admin_success() {
        let (env, contract_id, token, beneficiary, admin) = setup();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token, &beneficiary, &admin, &1_000_000, &100, &1000);
        let new_admin = Address::generate(&env);
        client.transfer_admin(&new_admin);
        let config = client.get_config();
        assert_eq!(config.admin, new_admin);
    }

    #[test]
    fn test_pause_freezes_vested_amount_and_blocks_claim() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);
        env.ledger().with_mut(|l| l.timestamp = 500);
        client.pause();
        let status = client.get_status();
        assert!(status.paused);
        assert_eq!(status.vested, 500_000);
        assert!(client.try_claim().is_err());
    }

    #[test]
    fn test_unpause_shifts_timeline_correctly() {
        let (env, contract_id, token_id, beneficiary, admin) = setup_with_token();
        let client = ForgeVestingClient::new(&env, &contract_id);
        client.initialize(&token_id, &beneficiary, &admin, &1_000_000, &0, &1000);
        let original_start = client.get_config().start_time;
        env.ledger().with_mut(|l| l.timestamp = 500);
        client.pause();
        env.ledger().with_mut(|l| l.timestamp = 700);
        client.unpause();
        let config = client.get_config();
        assert_eq!(config.start_time, original_start + 200);
    }
}
