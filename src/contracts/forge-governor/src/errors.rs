use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum GovernorError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    ProposalNotFound = 3,
    VotingClosed = 4,
    VotingStillOpen = 5,
    AlreadyVoted = 6,
    QuorumNotReached = 7,
    ProposalNotPassed = 8,
    TimelockNotElapsed = 9,
    AlreadyExecuted = 10,
    AlreadyCancelled = 11,
    InvalidConfig = 12,
    InvalidWeight = 13,
    Unauthorized = 14,
    AlreadyFinalized = 15,
}
