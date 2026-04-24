use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FactoryError {
    ScheduleNotFound = 1,
    Unauthorized = 2,
    CliffNotReached = 3,
    NothingToClaim = 4,
    Cancelled = 5,
    InvalidConfig = 6,
}
