use soroban_sdk::{contracttype, Address};

#[contracttype]
pub enum DataKey {
    Config,
    Proposal(u64),
    Vote(u64, Address),
    NextProposalId,
    ActiveProposals,
    ActiveProposalIndex(u64),
}
