use soroban_sdk::{contract, contractimpl, token, Address, Env, String, Symbol, Vec};
use crate::storage::DataKey;
use crate::types::{GovernorConfig, Proposal, ProposalState, VoteDirection};
use crate::errors::GovernorError;

const INSTANCE_TTL_THRESHOLD: u32 = 17_280;
const INSTANCE_TTL_EXTEND: u32 = 34_560;
const PROPOSAL_TTL_EXTEND: u32 = 1_036_800;
const VOTE_TTL_EXTEND: u32 = 1_036_800;

#[contract]
pub struct GovernorContract;

#[contractimpl]
impl GovernorContract {
    /// Initialize the governor.
    pub fn initialize(env: Env, config: GovernorConfig) -> Result<(), GovernorError> {
        config.admin.require_auth();

        if env.storage().instance().has(&DataKey::Config) {
            return Err(GovernorError::AlreadyInitialized);
        }
        if config.quorum == 0 || config.voting_period == 0 {
            return Err(GovernorError::InvalidConfig);
        }
        if config.vote_token == config.admin {
            return Err(GovernorError::InvalidConfig);
        }
        env.storage().instance().set(&DataKey::Config, &config);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);
        Ok(())
    }

    /// Create a new governance proposal.
    pub fn propose(
        env: Env,
        proposer: Address,
        title: String,
        description: String,
    ) -> Result<u64, GovernorError> {
        proposer.require_auth();

        let config: GovernorConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(GovernorError::NotInitialized)?;

        let now = env.ledger().timestamp();
        let proposal_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::NextProposalId)
            .unwrap_or(0u64);

        let proposal = Proposal {
            proposer: proposer.clone(),
            title,
            description,
            vote_start: now,
            vote_end: now + config.voting_period,
            votes_for: 0,
            votes_against: 0,
            abstentions: 0,
            passed_at: None,
            state: ProposalState::Active,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);
        env.storage().persistent().extend_ttl(
            &DataKey::Proposal(proposal_id),
            PROPOSAL_TTL_EXTEND,
            PROPOSAL_TTL_EXTEND,
        );
        env.storage()
            .persistent()
            .set(&DataKey::NextProposalId, &(proposal_id + 1));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);

        let mut active: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::ActiveProposals)
            .unwrap_or_else(|| Vec::new(&env));
        let index = active.len();
        active.push_back(proposal_id);
        env.storage()
            .instance()
            .set(&DataKey::ActiveProposals, &active);
        env.storage()
            .instance()
            .set(&DataKey::ActiveProposalIndex(proposal_id), &index);

        env.events().publish(
            (Symbol::new(&env, "proposal_created"),),
            (proposal_id, &proposer, proposal.vote_end),
        );

        Ok(proposal_id)
    }

    /// Cast a vote.
    pub fn vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
        direction: VoteDirection,
        weight: i128,
    ) -> Result<(), GovernorError> {
        voter.require_auth();

        let config: GovernorConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(GovernorError::NotInitialized)?;

        let actual_balance = token::Client::new(&env, &config.vote_token).balance(&voter);
        if weight > actual_balance {
            return Err(GovernorError::InvalidWeight);
        }

        let vote_key = DataKey::Vote(proposal_id, voter.clone());
        if env.storage().persistent().has(&vote_key) {
            return Err(GovernorError::AlreadyVoted);
        }

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;

        if proposal.state != ProposalState::Active {
            return Err(GovernorError::VotingClosed);
        }

        let now = env.ledger().timestamp();
        if now > proposal.vote_end {
            return Err(GovernorError::VotingClosed);
        }

        if weight <= 0 {
            return Err(GovernorError::InvalidWeight);
        }

        match direction {
            VoteDirection::For => proposal.votes_for += weight,
            VoteDirection::Against => proposal.votes_against += weight,
            VoteDirection::Abstain => proposal.abstentions += weight,
        }

        env.storage().persistent().set(&vote_key, &weight);
        env.storage()
            .persistent()
            .extend_ttl(&vote_key, VOTE_TTL_EXTEND, VOTE_TTL_EXTEND);
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);
        env.storage().persistent().extend_ttl(
            &DataKey::Proposal(proposal_id),
            PROPOSAL_TTL_EXTEND,
            PROPOSAL_TTL_EXTEND,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);

        env.events().publish(
            (Symbol::new(&env, "vote_cast"),),
            (proposal_id, &voter, direction, weight),
        );

        Ok(())
    }

    /// Finalize a proposal.
    pub fn finalize(env: Env, proposal_id: u64) -> Result<ProposalState, GovernorError> {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;

        if proposal.state != ProposalState::Active {
            return Err(GovernorError::AlreadyFinalized);
        }

        let now = env.ledger().timestamp();
        if now <= proposal.vote_end {
            return Err(GovernorError::VotingStillOpen);
        }

        let config: GovernorConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(GovernorError::NotInitialized)?;
        let total_votes = proposal.votes_for + proposal.votes_against + proposal.abstentions;

        if total_votes >= config.quorum && proposal.votes_for > proposal.votes_against {
            proposal.state = ProposalState::Passed;
            proposal.passed_at = Some(proposal.vote_end);
        } else {
            proposal.state = ProposalState::Failed;
        }

        let state = proposal.state.clone();
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);
        env.storage().persistent().extend_ttl(
            &DataKey::Proposal(proposal_id),
            PROPOSAL_TTL_EXTEND,
            PROPOSAL_TTL_EXTEND,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);

        Self::remove_active_proposal(&env, proposal_id);

        env.events().publish(
            (Symbol::new(&env, "proposal_finalized"),),
            (proposal_id, proposal.votes_for, proposal.votes_against),
        );

        Ok(state)
    }

    // ── Internal Helpers ──────────────────────────────────────────────────────

    fn remove_active_proposal(env: &Env, proposal_id: u64) {
        let mut active: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::ActiveProposals)
            .unwrap_or_else(|| Vec::new(env));
        let index_key = DataKey::ActiveProposalIndex(proposal_id);
        if let Some(index) = env.storage().instance().get::<DataKey, u32>(&index_key) {
            active.remove(index);
            env.storage().instance().set(&DataKey::ActiveProposals, &active);
            env.storage().instance().remove(&index_key);
            
            for i in index..active.len() {
                let id = active.get(i).unwrap();
                env.storage().instance().set(&DataKey::ActiveProposalIndex(id), &i);
            }
        }
    }
}
