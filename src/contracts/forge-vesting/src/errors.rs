use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum VestingError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    CliffNotReached = 4,
    NothingToClaim = 5,
    Cancelled = 6,
    InvalidConfig = 7,
    SameAdmin = 8,
    SameBeneficiary = 11,
    BeneficiaryAsAdmin = 12,
    Paused = 9,
    NotPaused = 10,
    VestingComplete = 13,
}
