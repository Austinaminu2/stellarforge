use soroban_sdk::contracterror;

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
