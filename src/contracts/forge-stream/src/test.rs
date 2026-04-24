#[cfg(test)]
mod tests {
    extern crate std;
    use crate::contract::{ForgeStream, ForgeStreamClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token, Address, Env,
    };

    fn setup(env: &Env) -> (ForgeStreamClient, Address, Address, Address) {
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(env, &contract_id);
        let sender = Address::generate(env);
        let recipient = Address::generate(env);
        let token_admin = Address::generate(env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        token::StellarAssetClient::new(env, &token_id).mint(&sender, &1_000_000);
        (client, sender, recipient, token_id)
    }

    #[test]
    fn test_create_stream_success() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, sender, recipient, token) = setup(&env);
        let id = client.create_stream(&sender, &token, &recipient, &100, &1000, &0);
        assert_eq!(id, 0);
    }
}
