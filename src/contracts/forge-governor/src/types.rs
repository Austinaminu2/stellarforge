use soroban_sdk::{contracttype, Address, String};

/// Governor configuration.
#[contracttype]
#[derive(Clone)]
pub struct GovernorConfig {
    pub admin: Address,
    pub vote_token: Address,
    pub voting_period: u64,
    pub quorum: i128,
    pub timelock_delay: u64,
}

/// Proposal state.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum ProposalState {
    Active,
    Passed,
    Failed,
    Executed,
    Cancelled,
}

/// Direction of a vote cast on a proposal.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum VoteDirection {
    For,
    Against,
    Abstain,
}

/// Vote tally for a proposal.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VoteTally {
    pub yes_votes: i128,
    pub no_votes: i128,
    pub abstain_votes: i128,
    pub total_votes: i128,
}

/// A governance proposal.
#[contracttype]
#[derive(Clone)]
pub struct Proposal {
    pub proposer: Address,
    pub title: String,
    pub description: String,
    pub vote_start: u64,
    pub vote_end: u64,
    pub votes_for: i128,
    pub votes_against: i128,
    pub abstentions: i128,
    pub passed_at: Option<u64>,
    pub state: ProposalState,
}
