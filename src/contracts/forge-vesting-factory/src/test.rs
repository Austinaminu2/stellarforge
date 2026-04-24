#[cfg(test)]
mod tests {
    extern crate std;
    use crate::contract::{ForgeVestingFactory, ForgeVestingFactoryClient};
    use crate::errors::FactoryError;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token, Address, Env,
    };

    fn setup_token(env: &Env, admin: &Address, amount: i128) -> Address {
        let token_admin = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();
        token::Client::new(env, &token).mint(admin, &amount);
        token
    }

    fn make_client(env: &Env) -> ForgeVestingFactoryClient {
        let id = env.register_contract(None, ForgeVestingFactory);
        ForgeVestingFactoryClient::new(env, &id)
    }

    #[test]
    fn test_create_schedule_success() {
        let env = Env::default();
        env.mock_all_auths();
        let client = make_client(&env);
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token = setup_token(&env, &admin, 1_000);

        let id = client.create_schedule(&token, &beneficiary, &admin, &1_000, &100, &1_000);
        assert_eq!(id, 0);
        assert_eq!(client.get_schedule_count(), 1);
    }

    #[test]
    fn test_claim_after_cliff() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = make_client(&env);
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token = setup_token(&env, &admin, 1_000);

        let id = client.create_schedule(&token, &beneficiary, &admin, &1_000, &100, &1_000);

        env.ledger().with_mut(|l| l.timestamp = 500);
        let claimed = client.claim(&id);
        assert_eq!(claimed, 500); 

        let status = client.get_status(&id);
        assert_eq!(status.claimed, 500);
    }

    #[test]
    fn test_cancel_splits_tokens_correctly() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = make_client(&env);
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token_addr = setup_token(&env, &admin, 1_000);
        let tok = token::Client::new(&env, &token_addr);

        let id = client.create_schedule(&token_addr, &beneficiary, &admin, &1_000, &0, &1_000);

        env.ledger().with_mut(|l| l.timestamp = 300);
        client.cancel(&id);

        assert_eq!(tok.balance(&beneficiary), 300);
        assert_eq!(tok.balance(&admin), 700);
    }
}
