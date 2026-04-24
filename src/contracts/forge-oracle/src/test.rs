#[cfg(test)]
mod tests {
    extern crate std;
    use crate::contract::{ForgeOracle, ForgeOracleClient};
    use crate::errors::OracleError;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Env, Symbol,
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

        client.submit_price(&base, &quote, &11_000_000);
        let data = client.get_price(&base, &quote);

        assert_eq!(data.price, 11_000_000);
        assert_eq!(data.updated_at, 1000);
    }

    #[test]
    fn test_stale_price_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (_, client) = setup(&env);

        let base = Symbol::new(&env, "XLM");
        let quote = Symbol::new(&env, "USDC");

        client.submit_price(&base, &quote, &10_000_000);

        env.ledger().with_mut(|l| l.timestamp = 7200);
        let result = client.try_get_price(&base, &quote);
        assert_eq!(result, Err(Ok(OracleError::PriceStale)));
    }
}
