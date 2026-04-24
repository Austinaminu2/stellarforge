use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum StreamError {
    StreamNotFound = 1,
    Unauthorized = 2,
    NothingToWithdraw = 3,
    AlreadyCancelled = 4,
    InvalidConfig = 5,
    StreamFinished = 6,
    /// Sender's token balance is less than the total required to fund the stream
    InsufficientFunds = 7,
    /// Withdrawal amount is below the minimum threshold
    BelowMinimumWithdrawal = 8,
}
