use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum MultisigError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    ProposalNotFound = 4,
    AlreadyVoted = 5,
    TimelockNotElapsed = 6,
    AlreadyExecuted = 7,
    AlreadyCancelled = 8,
    InsufficientApprovals = 9,
    InvalidThreshold = 10,
    InvalidAmount = 11,
    CannotCancel = 12,
    InsufficientFunds = 13,
}
