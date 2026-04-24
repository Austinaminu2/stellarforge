use soroban_sdk::contracttype;

#[contracttype]
pub enum DataKey {
    Config,
    Claimed,
    VestedAtCancel,
}
