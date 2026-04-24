use soroban_sdk::{contracttype, Symbol};

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
