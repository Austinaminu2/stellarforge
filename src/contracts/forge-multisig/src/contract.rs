use soroban_sdk::{contract, contractimpl, token, Address, Env, Symbol, Vec};
use crate::storage::DataKey;
use crate::types::Proposal;
use crate::errors::MultisigError;

const INSTANCE_TTL_THRESHOLD: u32 = 17_280;
const INSTANCE_TTL_EXTEND: u32 = 34_560;

#[contract]
pub struct MultisigContract;

#[contractimpl]
impl MultisigContract {
    /// Initialize the multisig treasury.
    pub fn initialize(
        env: Env,
        owners: Vec<Address>,
        threshold: u32,
        timelock_delay: u64,
    ) -> Result<(), MultisigError> {
        if env.storage().instance().has(&DataKey::Owners) {
            return Err(MultisigError::AlreadyInitialized);
        }

        let mut unique_owners = Vec::new(&env);
        for owner in owners.iter() {
            if !unique_owners.contains(&owner) {
                unique_owners.push_back(owner);
            }
        }

        if threshold == 0 || threshold > unique_owners.len() {
            return Err(MultisigError::InvalidThreshold);
        }
        env.storage()
            .instance()
            .set(&DataKey::Owners, &unique_owners);
        env.storage()
            .instance()
            .set(&DataKey::Threshold, &threshold);
        env.storage()
            .instance()
            .set(&DataKey::TimelockDelay, &timelock_delay);

        for owner in unique_owners.iter() {
            env.storage()
                .instance()
                .set(&DataKey::IsOwner(owner), &true);
        }

        Ok(())
    }

    /// Propose a token transfer.
    pub fn propose(
        env: Env,
        proposer: Address,
        to: Address,
        token: Address,
        amount: i128,
    ) -> Result<u64, MultisigError> {
        proposer.require_auth();
        Self::require_owner(&env, &proposer)?;

        if amount <= 0 {
            return Err(MultisigError::InvalidAmount);
        }

        let proposal_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::NextProposalId)
            .unwrap_or(0u64);

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .ok_or(MultisigError::NotInitialized)?;
        let approved_at = if 1 >= threshold {
            Some(env.ledger().timestamp())
        } else {
            None
        };

        let proposal = Proposal {
            proposer: proposer.clone(),
            to: to.clone(),
            token: token.clone(),
            amount,
            approval_count: 1,
            rejection_count: 0,
            approved_at,
            executed: false,
            cancelled: false,
            is_native: false,
        };

        env.storage()
            .persistent()
            .set(&DataKey::HasApproved(proposal_id, proposer.clone()), &true);

        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);
        env.storage()
            .persistent()
            .set(&DataKey::NextProposalId, &(proposal_id + 1));

        env.storage()
            .persistent()
            .extend_ttl(&DataKey::NextProposalId, 31536000, 31536000);

        if approved_at.is_some() {
            let committed: i128 = env
                .storage()
                .instance()
                .get(&DataKey::CommittedAmount(token.clone()))
                .unwrap_or(0);
            env.storage().instance().set(
                &DataKey::CommittedAmount(token.clone()),
                &(committed + amount),
            );
        }

        env.events().publish(
            (Symbol::new(&env, "proposal_created"),),
            (proposal_id, &proposer, &to, &token, amount),
        );

        Ok(proposal_id)
    }

    /// Approve a proposal.
    pub fn approve(env: Env, owner: Address, proposal_id: u64) -> Result<(), MultisigError> {
        owner.require_auth();
        Self::require_owner(&env, &owner)?;

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(MultisigError::ProposalNotFound)?;

        if proposal.executed {
            return Err(MultisigError::AlreadyExecuted);
        }
        if proposal.cancelled {
            return Err(MultisigError::AlreadyCancelled);
        }
        if env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&DataKey::HasApproved(proposal_id, owner.clone()))
            .unwrap_or(false)
            || env
                .storage()
                .persistent()
                .get::<DataKey, bool>(&DataKey::HasRejected(proposal_id, owner.clone()))
                .unwrap_or(false)
        {
            return Err(MultisigError::AlreadyVoted);
        }

        proposal.approval_count = proposal.approval_count.saturating_add(1);
        env.storage()
            .persistent()
            .set(&DataKey::HasApproved(proposal_id, owner.clone()), &true);

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .ok_or(MultisigError::NotInitialized)?;
        
        if proposal.approval_count >= threshold && proposal.approved_at.is_none() {
            proposal.approved_at = Some(env.ledger().timestamp());
            let committed: i128 = env
                .storage()
                .instance()
                .get(&DataKey::CommittedAmount(proposal.token.clone()))
                .unwrap_or(0);
            env.storage().instance().set(
                &DataKey::CommittedAmount(proposal.token.clone()),
                &(committed + proposal.amount),
            );
        }

        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);

        env.events().publish(
            (Symbol::new(&env, "proposal_approved"),),
            (proposal_id, &owner, proposal.approval_count),
        );

        Ok(())
    }

    /// Execute an approved proposal.
    pub fn execute(env: Env, executor: Address, proposal_id: u64) -> Result<(), MultisigError> {
        executor.require_auth();
        Self::require_owner(&env, &executor)?;

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(MultisigError::ProposalNotFound)?;

        if proposal.executed {
            return Err(MultisigError::AlreadyExecuted);
        }
        if proposal.cancelled {
            return Err(MultisigError::AlreadyCancelled);
        }

        let approved_at = proposal
            .approved_at
            .ok_or(MultisigError::InsufficientApprovals)?;
        let delay: u64 = env
            .storage()
            .instance()
            .get(&DataKey::TimelockDelay)
            .unwrap_or(0);

        if env.ledger().timestamp() < approved_at + delay {
            return Err(MultisigError::TimelockNotElapsed);
        }

        let token_client = token::Client::new(&env, &proposal.token);
        let committed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::CommittedAmount(proposal.token.clone()))
            .unwrap_or(0);
        let balance = token_client.balance(&env.current_contract_address());
        if balance < committed {
            return Err(MultisigError::InsufficientFunds);
        }

        token_client.transfer(
            &env.current_contract_address(),
            &proposal.to,
            &proposal.amount,
        );

        proposal.executed = true;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        let new_committed = committed.saturating_sub(proposal.amount);
        env.storage().instance().set(
            &DataKey::CommittedAmount(proposal.token.clone()),
            &new_committed,
        );

        env.storage().instance().extend_ttl(17280, 34560);

        env.events().publish(
            (Symbol::new(&env, "proposal_executed"),),
            (proposal_id, &executor, &proposal.to, proposal.amount),
        );

        Ok(())
    }

    // ── Internal Helpers ──────────────────────────────────────────────────────

    fn require_owner(env: &Env, address: &Address) -> Result<(), MultisigError> {
        if !env
            .storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::IsOwner(address.clone()))
            .unwrap_or(false)
        {
            return Err(MultisigError::Unauthorized);
        }
        Ok(())
    }
}
