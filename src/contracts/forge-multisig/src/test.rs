#[cfg(test)]
mod tests {
    extern crate std;
    use crate::contract::{MultisigContract, MultisigContractClient};
    use crate::errors::MultisigError;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token, Address, Env, Vec,
    };

    fn setup(env: &Env) -> (MultisigContractClient, Address, Address, Address) {
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(env, &contract_id);
        let owner1 = Address::generate(env);
        let owner2 = Address::generate(env);
        let owner3 = Address::generate(env);
        client.initialize(&Vec::from_array(env, [owner1.clone(), owner2.clone(), owner3.clone()]), &2, &0);
        (client, owner1, owner2, owner3)
    }

    #[test]
    fn test_initialize_success() {
        let env = Env::default();
        let _ = setup(&env);
    }

    #[test]
    fn test_propose_success() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, owner1, _, _) = setup(&env);
        let to = Address::generate(&env);
        let token = Address::generate(&env);
        let id = client.propose(&owner1, &to, &token, &1000);
        assert_eq!(id, 0);
    }
}
