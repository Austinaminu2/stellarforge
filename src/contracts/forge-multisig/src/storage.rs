use soroban_sdk::{contracttype, Address};

#[contracttype]
pub enum DataKey {
    Owners,
    Threshold,
    TimelockDelay,
    Proposal(u64),
    NextProposalId,
    /// Boolean flag per address — `true` means the address is an owner.
    IsOwner(Address),
    /// Boolean flag for whether an address has approved a proposal.
    HasApproved(u64, Address),
    /// Boolean flag for whether an address has rejected a proposal.
    HasRejected(u64, Address),
    /// Total tokens committed to approved-but-not-yet-executed proposals per token address.
    CommittedAmount(Address),
}
