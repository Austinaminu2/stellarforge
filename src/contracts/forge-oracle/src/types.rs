use soroban_sdk::{contracttype, Symbol};

/// A price entry with value and timestamp.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceData {
    /// Price scaled to 7 decimal places (e.g. 1_0000000 = 1.0)
    pub price: i128,
    /// Ledger timestamp of last update
    pub updated_at: u64,
}

/// A single entry returned by `get_all_prices`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceEntry {
    pub base: Symbol,
    pub quote: Symbol,
    pub price: i128,
    pub updated_at: u64,
}
