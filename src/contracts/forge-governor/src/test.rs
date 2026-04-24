#[cfg(test)]
mod tests {
    extern crate std;
    use crate::contract::{GovernorContract, GovernorContractClient};
    use crate::types::GovernorConfig;
    use soroban_sdk::{
        testutils::{Address as _},
        Address, Env,
    };

    fn setup(env: &Env) -> (GovernorContractClient, Address, Address) {
        let contract_id = env.register_contract(None, GovernorContract);
        let client = GovernorContractClient::new(env, &contract_id);
        let admin = Address::generate(env);
        let token = Address::generate(env);
        let config = GovernorConfig {
            admin: admin.clone(),
            vote_token: token.clone(),
            voting_period: 3600,
            quorum: 1000,
            timelock_delay: 0,
        };
        client.initialize(&config);
        (client, admin, token)
    }

    #[test]
    fn test_initialize_success() {
        let env = Env::default();
        let _ = setup(&env);
    }
}
