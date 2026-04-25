#![no_std]

//! # forge-multisig
//!
//! An N-of-M multisig treasury contract for Stellar/Soroban.
//!
//! ## Features
//! - N-of-M signature threshold for transaction approval
//! - Timelock delay before execution after approval
//! - Owners can propose, approve, reject, and execute transactions
//! - Native token support via Stellar token interface

use forge_errors::CommonError;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Env, Symbol, Vec,
};

const INSTANCE_TTL_THRESHOLD: u32 = 17_280;
const INSTANCE_TTL_EXTEND: u32 = 34_560;

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Owners,
    Threshold,
    TimelockDelay,
    Proposal(u64),
    NextProposalId,
    /// Boolean flag per address — `true` means the address is an owner.
    /// Enables O(1) ownership checks without scanning the full owner Vec.
    IsOwner(Address),
    /// Boolean flag for whether an address has approved a proposal.
    HasApproved(u64, Address),
    /// Boolean flag for whether an address has rejected a proposal.
    HasRejected(u64, Address),
    /// Total tokens committed to approved-but-not-yet-executed proposals per token address.
    CommittedAmount(Address),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A pending treasury transaction proposal.
#[contracttype]
#[derive(Clone)]
pub struct Proposal {
    /// Who proposed this transaction.
    pub proposer: Address,
    /// Destination address for the transfer.
    pub to: Address,
    /// Token address. For native XLM proposals this holds the SAC address of
    /// the native asset and `is_native` is set to `true`.
    pub token: Address,
    /// Amount to transfer.
    pub amount: i128,
    /// Number of approvals recorded for this proposal.
    pub approval_count: u32,
    /// Number of rejections recorded for this proposal.
    pub rejection_count: u32,
    /// Ledger timestamp when approval threshold was reached.
    pub approved_at: Option<u64>,
    /// Whether the proposal has been executed.
    pub executed: bool,
    /// Whether the proposal has been cancelled.
    pub cancelled: bool,
    /// Whether this is a native XLM transfer proposal.
    /// When `true`, `execute()` uses the native asset SAC address stored in
    /// `token` rather than a custom Soroban token contract.
    pub is_native: bool,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MultisigError {
    #[from(CommonError)]
    Common(CommonError),
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

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct MultisigContract;

#[contractimpl]
impl MultisigContract {
    /// Initialize the multisig treasury.
    ///
    /// Stores the owner list, approval threshold, and timelock delay. Must be
    /// called exactly once before any other function. Does not require auth —
    /// the deployer is responsible for calling this immediately after deployment.
    ///
    /// Duplicate owner addresses are automatically deduplicated to ensure each
    /// owner is unique and counts only once toward the threshold.
    ///
    /// # Parameters
    /// - `owners` — List of addresses that are permitted to propose, vote, and execute.
    /// - `threshold` — Minimum number of approvals required to pass a proposal (N in N-of-M).
    ///   Must be ≥ 1 and ≤ the number of unique owners after deduplication.
    /// - `timelock_delay` — Seconds that must elapse after a proposal reaches the approval
    ///   threshold before it can be executed. Use `0` for no delay.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`MultisigError::AlreadyInitialized`] — Contract has already been initialized.
    /// - [`MultisigError::InvalidThreshold`] — `threshold` is 0 or exceeds the number of unique owners.
    ///
    /// # Example
    /// ```text
    /// // 2-of-3 multisig with a 3600 s (1 h) timelock
    /// client.initialize(&vec![&env, owner_a, owner_b, owner_c], &2, &3600);
    /// ```
    pub fn initialize(
        env: Env,
        owners: Vec<Address>,
        threshold: u32,
        timelock_delay: u64,
    ) -> Result<(), MultisigError> {
        if env.storage().instance().has(&DataKey::Owners) {
            return Err(MultisigError::Common(CommonError::AlreadyInitialized));
        }

        // Deduplicate owners to ensure uniqueness
        let mut unique_owners = Vec::new(&env);
        for owner in owners.iter() {
            if !unique_owners.contains(&owner) {
                unique_owners.push_back(owner);
            }
        }

        if threshold == 0 || threshold > unique_owners.len() {
            return Err(MultisigError::Common(CommonError::InvalidThreshold));
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

        // Populate O(1) ownership lookup map.
        for owner in unique_owners.iter() {
            env.storage()
                .instance()
                .set(&DataKey::IsOwner(owner), &true);
        }

        Ok(())
    }

    /// Propose a token transfer from the treasury.
    ///
    /// Creates a new [`Proposal`] and automatically records the proposer's approval.
    /// The returned ID is used to reference this proposal in subsequent `approve`,
    /// `reject`, and `execute` calls. Requires authorization from `proposer`.
    ///
    /// # Parameters
    /// - `proposer` — An owner address submitting the proposal.
    /// - `to` — Destination address that will receive the tokens if executed.
    /// - `token` — Address of the Soroban token contract to transfer from.
    /// - `amount` — Number of tokens (in the token's smallest unit) to transfer. Must be > 0.
    ///
    /// # Returns
    /// `Ok(proposal_id)` — the unique ID assigned to the new proposal.
    ///
    /// # Errors
    /// - [`MultisigError::Unauthorized`] — `proposer` is not in the owner list.
    /// - [`MultisigError::InvalidAmount`] — `amount` is ≤ 0.
    ///
    /// # Example
    /// ```text
    /// let id = client.propose(&owner, &recipient, &token, &500_000);
    /// ```
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
            return Err(MultisigError::Common(CommonError::InvalidAmount));
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

        // Extend TTL for NextProposalId to prevent expiry (1 year)
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::NextProposalId, 31536000, 31536000);

        // If threshold was met immediately (threshold=1), commit the amount now
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

    /// Propose a native XLM transfer from the treasury.
    ///
    /// Identical to [`propose`](Self::propose) but marks the proposal as a
    /// native XLM transfer (`is_native = true`). The contract must hold
    /// sufficient native XLM balance before `execute()` is called.
    ///
    /// On Soroban, native XLM is accessed through the Stellar Asset Contract
    /// (SAC) for the native asset. The SAC exposes the same `token::Client`
    /// interface as any other Soroban token, so `execute()` handles both cases
    /// identically — the `is_native` flag is a semantic marker for callers.
    ///
    /// # Parameters
    /// - `proposer` — An owner address submitting the proposal.
    /// - `to` — Destination address that will receive XLM if executed.
    /// - `xlm_token` — Address of the native asset SAC contract.
    /// - `amount` — Stroops to transfer (1 XLM = 10,000,000 stroops). Must be > 0.
    ///
    /// # Returns
    /// `Ok(proposal_id)` — the unique ID assigned to the new proposal.
    ///
    /// # Errors
    /// - [`MultisigError::Unauthorized`] — `proposer` is not in the owner list.
    /// - [`MultisigError::InvalidAmount`] — `amount` is ≤ 0.
    ///
    /// # Example
    /// ```text
    /// // Transfer 10 XLM (100_000_000 stroops) to recipient
    /// let id = client.propose_xlm(&owner, &recipient, &xlm_sac_address, &100_000_000);
    /// ```
    pub fn propose_xlm(
        env: Env,
        proposer: Address,
        to: Address,
        xlm_token: Address,
        amount: i128,
    ) -> Result<u64, MultisigError> {
        proposer.require_auth();
        Self::require_owner(&env, &proposer)?;

        if amount <= 0 {
            return Err(MultisigError::Common(CommonError::InvalidAmount));
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
            token: xlm_token.clone(),
            amount,
            approval_count: 1,
            rejection_count: 0,
            approved_at,
            executed: false,
            cancelled: false,
            is_native: true,
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

        // If threshold was met immediately (threshold=1), commit the amount now
        if approved_at.is_some() {
            let committed: i128 = env
                .storage()
                .instance()
                .get(&DataKey::CommittedAmount(xlm_token.clone()))
                .unwrap_or(0);
            env.storage().instance().set(
                &DataKey::CommittedAmount(xlm_token.clone()),
                &(committed + amount),
            );
        }

        env.events().publish(
            (Symbol::new(&env, "proposal_created"),),
            (proposal_id, &proposer, &to, &xlm_token, amount),
        );

        Ok(proposal_id)
    }

    /// Approve a proposal.
    ///
    /// Records `owner`'s approval on the given proposal. If the total approval count
    /// reaches the configured threshold for the first time, the timelock countdown
    /// begins by storing the current ledger timestamp in `approved_at`.
    /// Requires authorization from `owner`.
    ///
    /// # Parameters
    /// - `owner` — An owner address casting the approval vote.
    /// - `proposal_id` — ID of the proposal to approve.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`MultisigError::Unauthorized`] — `owner` is not in the owner list.
    /// - [`MultisigError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`MultisigError::AlreadyVoted`] — `owner` has already approved or rejected this proposal.
    /// - [`MultisigError::AlreadyExecuted`] — The proposal has already been executed.
    /// - [`MultisigError::AlreadyCancelled`] — The proposal has been cancelled.
    ///
    /// # Example
    /// ```text
    /// client.approve(&owner_b, &proposal_id);
    /// ```
    pub fn approve(env: Env, owner: Address, proposal_id: u64) -> Result<(), MultisigError> {
        owner.require_auth();
        Self::require_owner(&env, &owner)?;

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(MultisigError::Common(CommonError::ProposalNotFound))?;

        if proposal.executed {
            return Err(MultisigError::Common(CommonError::AlreadyExecuted));
        }
        if proposal.cancelled {
            return Err(MultisigError::Common(CommonError::AlreadyCancelled));
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
            return Err(MultisigError::Common(CommonError::AlreadyVoted));
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
        // The is_none() guard ensures approved_at is set only once, when the threshold is first reached.
        // This prevents the timelock countdown from being reset if threshold changes in the future.
        // Currently, owners and threshold are immutable after initialize(), but this guard protects
        // against accidental resets if threshold mutability is added later.
        if proposal.approval_count >= threshold && proposal.approved_at.is_none() {
            proposal.approved_at = Some(env.ledger().timestamp());
            // Track committed tokens to prevent over-commitment across concurrent proposals
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

    /// Reject a proposal.
    ///
    /// Records `owner`'s rejection on the given proposal. A rejected proposal can
    /// no longer reach the approval threshold once enough owners have rejected it,
    /// though the contract does not automatically cancel it.
    /// Requires authorization from `owner`.
    ///
    /// # Parameters
    /// - `owner` — An owner address casting the rejection vote.
    /// - `proposal_id` — ID of the proposal to reject.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`MultisigError::Unauthorized`] — `owner` is not in the owner list.
    /// - [`MultisigError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`MultisigError::AlreadyVoted`] — `owner` has already approved or rejected this proposal.
    /// - [`MultisigError::AlreadyExecuted`] — The proposal has already been executed.
    ///
    /// # Example
    /// ```text
    /// client.reject(&owner_c, &proposal_id);
    /// ```
    pub fn reject(env: Env, owner: Address, proposal_id: u64) -> Result<(), MultisigError> {
        owner.require_auth();
        Self::require_owner(&env, &owner)?;

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(MultisigError::Common(CommonError::ProposalNotFound))?;

        if proposal.executed {
            return Err(MultisigError::Common(CommonError::AlreadyExecuted));
        }
        if proposal.cancelled {
            return Err(MultisigError::Common(CommonError::AlreadyCancelled));
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
            return Err(MultisigError::Common(CommonError::AlreadyVoted));
        }

        proposal.rejection_count = proposal.rejection_count.saturating_add(1);
        env.storage()
            .persistent()
            .set(&DataKey::HasRejected(proposal_id, owner.clone()), &true);
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (Symbol::new(&env, "proposal_rejected"),),
            (proposal_id, &owner, proposal.rejection_count),
        );

        Ok(())
    }

    /// Execute an approved proposal after the timelock delay has elapsed.
    ///
    /// Transfers the proposed token amount from the contract's treasury balance to
    /// the proposal's `to` address. The proposal must have reached the approval
    /// threshold and the configured `timelock_delay` must have passed since
    /// `approved_at`. Requires authorization from `executor`.
    ///
    /// # Parameters
    /// - `executor` — An owner address triggering execution.
    /// - `proposal_id` — ID of the proposal to execute.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`MultisigError::Unauthorized`] — `executor` is not in the owner list.
    /// - [`MultisigError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`MultisigError::AlreadyExecuted`] — The proposal has already been executed.
    /// - [`MultisigError::AlreadyCancelled`] — The proposal has been cancelled.
    /// - [`MultisigError::InsufficientApprovals`] — Threshold has not been reached yet.
    /// - [`MultisigError::TimelockNotElapsed`] — The timelock delay has not fully passed.
    ///
    /// # Example
    /// ```text
    /// // After timelock has elapsed:
    /// client.execute(&owner_a, &proposal_id);
    /// ```
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

        proposal.executed = true;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        let token_client = token::Client::new(&env, &proposal.token);

        // Verify the treasury holds enough to cover all committed proposals.
        // For both token and native XLM proposals the transfer goes through
        // token::Client. Native XLM proposals store the native asset SAC address
        // in `proposal.token`, so the call is identical in both cases.
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

        // Mark executed AFTER the transfer succeeds. Setting executed = true before
        // the transfer would permanently lock funds if the transfer traps — the
        // proposal would be unretryable and the tokens unreachable forever.
        proposal.executed = true;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        // Release the committed amount for this proposal
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

    /// Cancel a proposal that can no longer reach the approval threshold.
    ///
    /// Allows an owner to cancel a proposal if it is mathematically impossible
    /// for it to reach the approval threshold, or if the proposer cancels before
    /// execution. This helps clean up dead proposals and free storage.
    /// Requires authorization from `owner`.
    ///
    /// # Parameters
    /// - `owner` — An owner address requesting cancellation.
    /// - `proposal_id` — ID of the proposal to cancel.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`MultisigError::Unauthorized`] — `owner` is not in the owner list.
    /// - [`MultisigError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`MultisigError::AlreadyExecuted`] — The proposal has already been executed.
    /// - [`MultisigError::AlreadyCancelled`] — The proposal has already been cancelled.
    /// - [`MultisigError::CannotCancel`] — The proposal can still reach the approval threshold.
    ///
    /// # Example
    /// ```text
    /// // Cancel a proposal that can no longer reach threshold
    /// client.cancel(&owner_a, &proposal_id);
    /// ```
    pub fn cancel(env: Env, owner: Address, proposal_id: u64) -> Result<(), MultisigError> {
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

        // Allow proposer to cancel at any time before execution
        if proposal.proposer == owner {
            proposal.cancelled = true;
            // Release committed amount if threshold had been reached
            if proposal.approved_at.is_some() {
                let committed: i128 = env
                    .storage()
                    .instance()
                    .get(&DataKey::CommittedAmount(proposal.token.clone()))
                    .unwrap_or(0);
                env.storage().instance().set(
                    &DataKey::CommittedAmount(proposal.token.clone()),
                    &committed.saturating_sub(proposal.amount),
                );
            }
            env.storage()
                .persistent()
                .set(&DataKey::Proposal(proposal_id), &proposal);

            env.events().publish(
                (Symbol::new(&env, "proposal_cancelled"),),
                (proposal_id, &owner),
            );

            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);

            return Ok(());
        }

        // For other owners, only allow cancellation if mathematically impossible to pass
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .ok_or(MultisigError::NotInitialized)?;
        let owners: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Owners)
            .ok_or(MultisigError::NotInitialized)?;
        let total_owners = owners.len();

        // Calculate remaining possible approvals
        // remaining_possible = total_owners - rejection_count - approval_count
        let remaining_possible = total_owners
            .saturating_sub(proposal.rejection_count)
            .saturating_sub(proposal.approval_count);

        // If remaining possible approvals < threshold, it's impossible to pass
        if remaining_possible < threshold {
            proposal.cancelled = true;
            // Release committed amount if threshold had been reached
            if proposal.approved_at.is_some() {
                let committed: i128 = env
                    .storage()
                    .instance()
                    .get(&DataKey::CommittedAmount(proposal.token.clone()))
                    .unwrap_or(0);
                env.storage().instance().set(
                    &DataKey::CommittedAmount(proposal.token.clone()),
                    &committed.saturating_sub(proposal.amount),
                );
            }
            env.storage()
                .persistent()
                .set(&DataKey::Proposal(proposal_id), &proposal);

            env.events().publish(
                (Symbol::new(&env, "proposal_cancelled"),),
                (proposal_id, &owner),
            );

            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);

            return Ok(());
        }

        Err(MultisigError::CannotCancel)
    }

    /// Return a proposal by its ID.
    ///
    /// Read-only; does not modify state. Returns `Err(MultisigError::ProposalNotFound)`
    /// if no proposal exists with the given ID, consistent with the error-returning
    /// convention used across all Forge contracts (e.g. `forge-governor`).
    ///
    /// # Parameters
    /// - `proposal_id` — The ID returned by [`propose`](Self::propose).
    ///
    /// # Returns
    /// `Ok(`[`Proposal`]`)` if found, `Err(`[`MultisigError::ProposalNotFound`]`)` otherwise.
    ///
    /// # Example
    /// ```text
    /// let proposal = client.get_proposal(&id).expect("proposal not found");
    /// println!("approvals: {}", proposal.approval_count);
    /// ```
    pub fn get_proposal(env: Env, proposal_id: u64) -> Result<Proposal, MultisigError> {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(MultisigError::ProposalNotFound)
    }

    /// Return the list of authorized owner addresses.
    ///
    /// Read-only; returns an empty `Vec` if the contract has not been initialized.
    ///
    /// # Returns
    /// A [`Vec<Address>`] of all current owners.
    ///
    /// # Example
    /// ```text
    /// let owners = client.get_owners();
    /// assert_eq!(owners.len(), 3);
    /// ```
    pub fn get_owners(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::Owners)
            .unwrap_or(Vec::new(&env))
    }

    /// Return the list of authorized owner addresses. Alias for [`get_owners`](Self::get_owners).
    ///
    /// # Returns
    /// A [`Vec<Address>`] of all current owners.
    pub fn get_owner_list(env: Env) -> Vec<Address> {
        Self::get_owners(env)
    }

    /// Return the current approval threshold (N in N-of-M).
    ///
    /// Read-only; returns `0` if the contract has not been initialized.
    ///
    /// # Returns
    /// The minimum number of owner approvals required to pass a proposal.
    ///
    /// # Example
    /// ```text
    /// let threshold = client.get_threshold(); // e.g. 2 for a 2-of-3 setup
    /// ```
    pub fn get_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(0)
    }

    /// Return the configured timelock delay in seconds.
    ///
    /// Read-only; returns `0` if the contract has not been initialized.
    /// This is the number of seconds that must elapse after a proposal reaches
    /// the approval threshold before it can be executed.
    ///
    /// # Returns
    /// `u64` — the timelock delay in seconds set at initialization.
    ///
    /// # Example
    /// ```text
    /// let delay = client.get_timelock_delay();
    /// println!("Timelock: {} seconds", delay);
    /// ```
    pub fn get_timelock_delay(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::TimelockDelay)
            .unwrap_or(0)
    }

    /// Check if an address is one of the multisig owners.
    ///
    /// Read-only; returns `false` if the contract has not been initialized.
    /// This is a lightweight alternative to [`get_owners`](Self::get_owners) when
    /// UIs or integrators only need to verify ownership status.
    ///
    /// # Parameters
    /// - `address` — The address to check for ownership.
    ///
    /// # Returns
    /// `true` if `address` is in the owner list, `false` otherwise.
    ///
    /// # Example
    /// ```text
    /// if client.is_owner(&some_address) {
    ///     // enable multisig actions
    /// }
    /// ```
    pub fn is_owner(env: Env, address: Address) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::IsOwner(address))
            .unwrap_or(false)
    }

    /// Return the number of owner approvals for a proposal.
    ///
    /// Lightweight read-only view intended for UIs that only need approval count.
    /// Returns `0` if the proposal does not exist.
    ///
    /// # Parameters
    /// - `proposal_id` — The target proposal ID.
    ///
    /// # Returns
    /// Number of approvals currently recorded for the proposal.
    pub fn get_approval_count(env: Env, proposal_id: u64) -> u32 {
        env.storage()
            .persistent()
            .get::<DataKey, Proposal>(&DataKey::Proposal(proposal_id))
            .map(|proposal| proposal.approval_count)
            .unwrap_or(0)
    }

    /// Return the total tokens committed to approved-but-not-yet-executed proposals
    /// for a given token address.
    ///
    /// This value increases when a proposal reaches the approval threshold and decreases
    /// when a proposal is executed or cancelled. It is used by [`execute`](Self::execute)
    /// to verify the treasury holds enough tokens to cover all pending commitments before
    /// transferring funds.
    ///
    /// # Parameters
    /// - `token` — The token contract address to query.
    ///
    /// # Returns
    /// `i128` — total committed tokens for `token`. Returns `0` if none committed.
    pub fn get_committed_amount(env: Env, token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::CommittedAmount(token))
            .unwrap_or(0)
    }

    // ── Private ───────────────────────────────────────────────────────────────

    fn require_owner(env: &Env, address: &Address) -> Result<(), MultisigError> {
        // Guard against calls before initialize() — IsOwner keys only exist post-init.
        if !env.storage().instance().has(&DataKey::Owners) {
            return Err(MultisigError::Common(CommonError::NotInitialized));
        }
        let is_owner: bool = env
            .storage()
            .instance()
            .get(&DataKey::IsOwner(address.clone()))
            .unwrap_or(false);
        if is_owner {
            Ok(())
        } else {
            return Err(MultisigError::Common(CommonError::Unauthorized));
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        vec, Env,
    };

    fn setup_2of3<'a>(env: &'a Env) -> (MultisigContractClient<'a>, Address, Address, Address) {
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(env, &contract_id);
        let o1 = Address::generate(env);
        let o2 = Address::generate(env);
        let o3 = Address::generate(env);
        client.initialize(&vec![env, o1.clone(), o2.clone(), o3.clone()], &2, &3600);
        (client, o1, o2, o3)
    }

    #[test]
    fn test_invalid_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let result = client.try_initialize(&vec![&env, o1], &5, &0);
        assert_eq!(result, Err(Ok(MultisigError::InvalidThreshold)));
    }

    /// TC: threshold > unique_owners.len() must return InvalidThreshold.
    /// Verifies the boundary: 4-of-3 is rejected even though 3-of-3 is valid.
    #[test]
    fn test_initialize_threshold_exceeds_owners_returns_invalid_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        let result = client.try_initialize(&vec![&env, o1, o2, o3], &4, &0);
        assert_eq!(result, Err(Ok(MultisigError::InvalidThreshold)));
    }

    /// TC: unanimous 3-of-3 multisig — every owner must approve before execution.
    ///
    /// Steps:
    /// 1. Initialize 3-of-3 with a 3600 s timelock.
    /// 2. propose() — auto-approves o1 (1/3); approved_at must still be None.
    /// 3. approve(o2) — 2/3; approved_at must still be None.
    /// 4. approve(o3) — 3/3 hits threshold; approved_at must be Some(timestamp).
    /// 5. Advance past timelock and execute — assert proposal.executed.
    #[test]
    fn test_unanimous_3of3_multisig() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &3, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        // Step 2: propose — o1 auto-approves (1/3), threshold not yet reached
        let pid = client.propose(&o1, &to, &token_id, &500);
        assert!(client.get_proposal(&pid).unwrap().approved_at.is_none());

        // Step 3: o2 approves (2/3), still not reached
        client.approve(&o2, &pid);
        assert!(client.get_proposal(&pid).unwrap().approved_at.is_none());

        // Step 4: o3 approves (3/3), threshold reached
        client.approve(&o3, &pid);
        assert!(client.get_proposal(&pid).unwrap().approved_at.is_some());

        // Step 5: advance past timelock and execute
        env.ledger().with_mut(|l| l.timestamp = 1000 + 3600 + 1);
        client.execute(&o1, &pid);
        assert!(client.get_proposal(&pid).unwrap().executed);
    }

    #[test]
    fn test_get_timelock_delay() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _, _, _) = setup_2of3(&env);
        // setup_2of3 initializes with timelock_delay = 3600
        assert_eq!(client.get_timelock_delay(), 3600);
    }

    #[test]
    fn test_get_timelock_delay_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        client.initialize(&vec![&env, o1], &1, &0);
        assert_eq!(client.get_timelock_delay(), 0);
    }

    #[test]
    fn test_initialize_with_duplicate_owners() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let owners = vec![&env, o1.clone(), o1.clone(), o1.clone()]; // 3 duplicates
        client.initialize(&owners, &1, &0);
        let stored_owners = client.get_owners();
        assert_eq!(stored_owners.len(), 1);
        assert!(stored_owners.contains(&o1));
    }

    #[test]
    fn test_propose_and_approve_reaches_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);
        client.approve(&o2, &pid);

        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.approved_at.is_some());
    }

    /// TC: propose() with amount = 0 must return InvalidAmount and leave no proposal.
    #[test]
    fn test_propose_zero_amount_returns_invalid_amount() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let result = client.try_propose(&o1, &to, &token, &0);
        assert_eq!(result, Err(Ok(MultisigError::InvalidAmount)));
        assert!(client.get_proposal(&0).is_none());
    }

    /// TC: propose() with amount = -1 must return InvalidAmount and leave no proposal.
    #[test]
    fn test_propose_negative_amount_returns_invalid_amount() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let result = client.try_propose(&o1, &to, &token, &-1);
        assert_eq!(result, Err(Ok(MultisigError::InvalidAmount)));
        assert!(client.get_proposal(&0).is_none());
    }

    /// TC: propose() with amount = 1 must succeed and create a proposal.
    #[test]
    fn test_propose_minimum_valid_amount_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &1);
        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.amount, 1);
    }

    #[test]
    fn test_double_vote_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);
        let result = client.try_approve(&o1, &pid);
        assert_eq!(result, Err(Ok(MultisigError::AlreadyVoted)));
    }

    #[test]
    fn test_timelock_not_elapsed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, o1, o2, o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);
        client.approve(&o2, &pid);

        let result = client.try_execute(&o3, &pid);
        assert_eq!(result, Err(Ok(MultisigError::TimelockNotElapsed)));
    }

    #[test]
    fn test_execute_after_timelock() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &2, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        let pid = client.propose(&o1, &to, &token_id, &500);
        client.approve(&o2, &pid);

        env.ledger().with_mut(|l| l.timestamp = 7200);
        client.execute(&o3, &pid);

        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.executed);
    }

    #[test]
    fn test_execute_reverts_below_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, o1, _, o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        // Only proposer's auto-approval — 1 of 2 required
        let pid = client.propose(&o1, &to, &token, &500);
        env.ledger().with_mut(|l| l.timestamp = 7200);
        let result = client.try_execute(&o3, &pid);
        assert_eq!(result, Err(Ok(MultisigError::InsufficientApprovals)));
    }

    #[test]
    fn test_execute_succeeds_at_exact_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &2, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        let pid = client.propose(&o1, &to, &token_id, &500);
        // Second approval hits threshold exactly (2-of-3)
        client.approve(&o2, &pid);

        env.ledger().with_mut(|l| l.timestamp = 7200);
        client.execute(&o3, &pid);

        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.executed);
    }

    #[test]
    fn test_approve_after_execute_reverts_with_already_executed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &2, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        let pid = client.propose(&o1, &to, &token_id, &500);
        client.approve(&o2, &pid);

        env.ledger().with_mut(|l| l.timestamp = 7200);
        client.execute(&o3, &pid);

        // Try to approve with o3 (who hasn't voted yet) after execution
        let result = client.try_approve(&o3, &pid);
        assert_eq!(result, Err(Ok(MultisigError::AlreadyExecuted)));

        // Also test reject() on executed proposal
        let result = client.try_reject(&o3, &pid);
        assert_eq!(result, Err(Ok(MultisigError::AlreadyExecuted)));
    }

    #[test]
    fn test_get_approval_count_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _, _, _) = setup_2of3(&env);

        assert_eq!(client.get_approval_count(&999), 0);
    }

    #[test]
    fn test_get_approval_count_partial() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);

        assert_eq!(client.get_approval_count(&pid), 1);
    }

    #[test]
    fn test_get_approval_count_full() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);
        client.approve(&o2, &pid);

        assert_eq!(client.get_approval_count(&pid), 2);
    }

    #[test]
    fn test_get_approval_count_full_lifecycle_including_execution() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &3, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        let pid = client.propose(&o1, &to, &token_id, &500);
        assert_eq!(client.get_approval_count(&pid), 1);

        client.approve(&o2, &pid);
        assert_eq!(client.get_approval_count(&pid), 2);

        client.approve(&o3, &pid);
        assert_eq!(client.get_approval_count(&pid), 3);

        env.ledger().with_mut(|l| l.timestamp = 1000 + 3600 + 1);
        client.execute(&o1, &pid);
        assert_eq!(client.get_approval_count(&pid), 3);
    }

    #[test]
    fn test_rejected_proposal_cannot_execute() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        let o4 = Address::generate(&env);

        // 3-of-4 multisig
        client.initialize(
            &vec![&env, o1.clone(), o2.clone(), o3.clone(), o4.clone()],
            &3,
            &3600,
        );

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        // o1 proposes (auto-approval)
        let pid = client.propose(&o1, &to, &token_id, &500);

        // o2 and o3 reject - proposal is now rejected (2 rejections means only 2 owners left who could approve)
        client.reject(&o2, &pid);
        client.reject(&o3, &pid);

        // Verify proposal has 2 rejections
        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.rejection_count, 2);
        assert_eq!(proposal.approval_count, 1); // only proposer

        // Even if o4 approves, bringing total approvals to 2, it should not be executable
        // because 2 rejections means threshold of 3 can never be reached
        client.approve(&o4, &pid);

        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.approval_count, 2);

        // Advance time past timelock
        env.ledger().with_mut(|l| l.timestamp = 7200);

        // Execution should fail because proposal is effectively rejected
        let result = client.try_execute(&o1, &pid);
        assert_eq!(result, Err(Ok(MultisigError::InsufficientApprovals)));

        // Verify proposal state remains unchanged
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(!proposal.executed);
        assert_eq!(proposal.rejection_count, 2);
    }

    #[test]
    fn test_get_approval_count_rejected_proposal_returns_approvals_at_rejection_time() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        let o4 = Address::generate(&env);

        client.initialize(
            &vec![&env, o1.clone(), o2.clone(), o3.clone(), o4.clone()],
            &3,
            &3600,
        );

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        let pid = client.propose(&o1, &to, &token_id, &500);
        client.approve(&o4, &pid);
        assert_eq!(client.get_approval_count(&pid), 2);

        client.reject(&o2, &pid);
        client.reject(&o3, &pid);
        assert_eq!(client.get_approval_count(&pid), 2);
    }

    /// Test mixed approval/rejection scenario where threshold is still reached
    ///
    /// Steps:
    /// 1. Set up 2-of-3 multisig with owners o1, o2, o3
    /// 2. o1 proposes (auto-approves, 1 approval)
    /// 3. o2 rejects (1 approval, 1 rejection)
    /// 4. o3 approves — threshold of 2 is now reached
    /// 5. Assert proposal.approved_at is set after o3 approves
    /// 6. Advance past timelock and execute — assert success
    /// 7. Verify token balances are correct
    #[test]
    fn test_mixed_approval_rejection_threshold_reached() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);

        // 2-of-3 multisig with 3600s timelock
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &2, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let recipient = Address::generate(&env);

        // Mint tokens to the contract treasury
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &1000);

        // Record initial balances
        let initial_contract_balance =
            soroban_sdk::token::StellarAssetClient::new(&env, &token_id).balance(&contract_id);
        let initial_recipient_balance =
            soroban_sdk::token::StellarAssetClient::new(&env, &token_id).balance(&recipient);

        // o1 proposes (auto-approves, 1 approval)
        let pid = client.propose(&o1, &recipient, &token_id, &500);

        // Verify initial state: 1 approval, 0 rejections, approved_at = None
        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.approval_count, 1);
        assert_eq!(proposal.rejection_count, 0);
        assert_eq!(proposal.approved_at, None);

        // o2 rejects (1 approval, 1 rejection)
        client.reject(&o2, &pid);

        // Verify state after rejection: still 1 approval, 1 rejection, approved_at = None
        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.approval_count, 1);
        assert_eq!(proposal.rejection_count, 1);
        assert_eq!(proposal.approved_at, None);

        // o3 approves — threshold of 2 is now reached
        client.approve(&o3, &pid);

        // Verify state after o3 approves: 2 approvals, 1 rejection, approved_at is set
        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.approval_count, 2);
        assert_eq!(proposal.rejection_count, 1);
        assert!(proposal.approved_at.is_some());
        assert_eq!(proposal.approved_at.unwrap(), 0); // Should be set to current timestamp

        // Advance past timelock (3600s)
        env.ledger().with_mut(|l| l.timestamp = 7200);

        // Execute should succeed
        client.execute(&o1, &pid);

        // Verify proposal is executed
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.executed);

        // Verify token balances are correct
        let final_contract_balance =
            soroban_sdk::token::StellarAssetClient::new(&env, &token_id).balance(&contract_id);
        let final_recipient_balance =
            soroban_sdk::token::StellarAssetClient::new(&env, &token_id).balance(&recipient);

        assert_eq!(final_contract_balance, initial_contract_balance - 500);
        assert_eq!(final_recipient_balance, initial_recipient_balance + 500);
    }

    #[test]
    fn test_rejected_proposal_state_immutable() {
        let env = Env::default();
        env.mock_all_auths();

        let (client, o1, o2, o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        // o1 proposes (auto-approval)
        let pid = client.propose(&o1, &to, &token, &500);

        // o2 and o3 reject - proposal is now rejected (2 rejections in 2-of-3 means impossible to reach threshold)
        client.reject(&o2, &pid);
        client.reject(&o3, &pid);

        // Verify rejection state
        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.rejection_count, 2);
        assert_eq!(proposal.approval_count, 1);
        assert!(proposal.approved_at.is_none()); // Never reached approval threshold

        // Proposal should remain in rejected state
        let proposal_after = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal_after.rejection_count, 2);
        assert!(!proposal_after.executed);
    }

    // ── Timelock enforcement tests ────────────────────────────────────────────
    //
    // The timelock acts as a "cooling-off" period: even after enough owners have
    // approved a proposal, funds cannot move until the configured delay has fully
    // elapsed. This gives remaining owners (or the broader community) time to
    // detect and react to a compromised key or a rushed decision before it is
    // too late.

    /// Helper: set up a 2-of-3 multisig with a custom timelock and a funded token.
    /// Returns (client, [o1, o2, o3], token_id, recipient, contract_id).
    fn setup_funded<'a>(
        env: &'a Env,
        timelock_delay: u64,
    ) -> (
        MultisigContractClient<'a>,
        [Address; 3],
        Address,
        Address,
        Address,
    ) {
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(env, &contract_id);
        let o1 = Address::generate(env);
        let o2 = Address::generate(env);
        let o3 = Address::generate(env);
        client.initialize(
            &vec![env, o1.clone(), o2.clone(), o3.clone()],
            &2,
            &timelock_delay,
        );

        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        soroban_sdk::token::StellarAssetClient::new(env, &token_id).mint(&contract_id, &1000);
        let recipient = Address::generate(env);

        (client, [o1, o2, o3], token_id, recipient, contract_id)
    }

    /// TC1 — Premature execution (T+23 h) must revert with TimelockNotElapsed.
    #[test]
    fn test_timelock_premature_execution_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        const DELAY: u64 = 86_400; // 24 h
        let (client, [o1, o2, o3], token_id, recipient, _) = setup_funded(&env, DELAY);

        let pid = client.propose(&o1, &recipient, &token_id, &100);
        client.approve(&o2, &pid); // threshold reached at T=0

        // Advance to T+23 h — one hour short of the required delay
        env.ledger().with_mut(|l| l.timestamp = DELAY - 3_600);
        let result = client.try_execute(&o3, &pid);
        assert_eq!(result, Err(Ok(MultisigError::TimelockNotElapsed)));
    }

    /// TC2 — Execution at exactly T+24 h+1 s must succeed and mark the proposal executed.
    #[test]
    fn test_timelock_exact_boundary_execution_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        const DELAY: u64 = 86_400; // 24 h
        let (client, [o1, o2, o3], token_id, recipient, _) = setup_funded(&env, DELAY);

        let pid = client.propose(&o1, &recipient, &token_id, &100);
        client.approve(&o2, &pid); // threshold reached at T=0

        // Advance to T+24 h+1 s — just past the boundary
        env.ledger().with_mut(|l| l.timestamp = DELAY + 1);
        client.execute(&o3, &pid);

        assert!(client.get_proposal(&pid).unwrap().executed);
    }

    /// TC3 — Zero-delay timelock: execute() must succeed immediately after threshold is met.
    #[test]
    fn test_timelock_zero_delay_executes_immediately() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1_000);

        let (client, [o1, o2, o3], token_id, recipient, _) = setup_funded(&env, 0);

        let pid = client.propose(&o1, &recipient, &token_id, &100);
        client.approve(&o2, &pid); // threshold reached — no time advance needed

        client.execute(&o3, &pid);
        assert!(client.get_proposal(&pid).unwrap().executed);
    }

    #[test]
    fn test_is_owner_returns_true_for_owner() {
        let env = Env::default();
        let (client, o1, _, _) = setup_2of3(&env);

        assert!(client.is_owner(&o1));
    }

    #[test]
    fn test_is_owner_returns_false_for_non_owner() {
        let env = Env::default();
        let (client, _, _, _) = setup_2of3(&env);
        let non_owner = Address::generate(&env);

        assert!(!client.is_owner(&non_owner));
    }

    #[test]
    fn test_get_threshold_returns_initialized_value() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _, _, _) = setup_2of3(&env);

        // setup_2of3 initializes with threshold = 2
        assert_eq!(client.get_threshold(), 2);
    }

    #[test]
    fn test_get_owners_list() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, o3) = setup_2of3(&env);
        let owners = client.get_owner_list();
        assert_eq!(owners.len(), 3);
        assert!(owners.contains(&o1));
        assert!(owners.contains(&o2));
        assert!(owners.contains(&o3));
    }

    /// Test that get_owner_list() and get_owners() return identical results.
    /// get_owner_list() is documented as an alias for get_owners() and simply delegates to it.
    /// This test verifies the delegation is correct and both functions return identical results.
    #[test]
    fn test_get_owner_list_and_get_owners_return_identical_results() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, o3) = setup_2of3(&env);

        let owners = client.get_owners();
        let owner_list = client.get_owner_list();

        // Assert same length
        assert_eq!(owners.len(), owner_list.len());

        // Assert same elements in same order
        for i in 0..owners.len() {
            assert_eq!(owners.get(i).unwrap(), owner_list.get(i).unwrap());
        }

        // Assert all expected owners are present
        assert!(owners.contains(&o1));
        assert!(owners.contains(&o2));
        assert!(owners.contains(&o3));
        assert!(owner_list.contains(&o1));
        assert!(owner_list.contains(&o2));
        assert!(owner_list.contains(&o3));
    }

    // ── 1-of-N threshold tests ─────────────────────────────────────────────────
    //
    // A threshold of 1 means any single owner can unilaterally authorize a
    // treasury transfer. This is a valid but high-risk configuration — useful for
    // hot wallets or automated systems where speed matters more than consensus.
    // It must be fully supported for flexible treasury management.

    /// Helper: 1-of-3 multisig with a 3600 s timelock and a funded token.
    fn setup_1of3_funded<'a>(
        env: &'a Env,
    ) -> (
        MultisigContractClient<'a>,
        Address,
        Address,
        Address,
        Address,
        Address,
    ) {
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(env, &contract_id);
        let o1 = Address::generate(env);
        let o2 = Address::generate(env);
        let o3 = Address::generate(env);
        client.initialize(&vec![env, o1.clone(), o2.clone(), o3.clone()], &1, &3600);
        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        soroban_sdk::token::StellarAssetClient::new(env, &token_id).mint(&contract_id, &1000);
        let recipient = Address::generate(env);
        (client, o1, o2, o3, token_id, recipient)
    }

    /// TC1 — Single approval flow: proposer's own approval meets threshold=1,
    /// proposal is ready after timelock elapses.
    #[test]
    fn test_threshold_1_single_approval_flow() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let (client, o1, _, o3, token_id, recipient) = setup_1of3_funded(&env);

        // propose auto-approves for proposer — threshold=1 is immediately met
        let pid = client.propose(&o1, &recipient, &token_id, &100);
        let proposal = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal.approval_count, 1);
        assert!(proposal.approved_at.is_some()); // threshold reached at proposal time

        // advance past timelock and execute
        env.ledger().with_mut(|l| l.timestamp = 3601);
        client.execute(&o3, &pid);
        assert!(client.get_proposal(&pid).unwrap().executed);
    }

    /// TC2 — Inter-owner independence: Owner B's approved proposal cannot be
    /// blocked by Owner C rejecting after threshold is already met.
    #[test]
    fn test_threshold_1_rejection_cannot_block_approved_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let (client, _, o2, o3, token_id, recipient) = setup_1of3_funded(&env);

        // o2 proposes — threshold=1 met immediately via auto-approval
        let pid = client.propose(&o2, &recipient, &token_id, &100);
        assert!(client.get_proposal(&pid).unwrap().approved_at.is_some());

        // o3 tries to reject — already voted check: o3 hasn't voted, so rejection
        // is recorded, but approved_at is already set and cannot be unset
        client.reject(&o3, &pid);
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.approved_at.is_some()); // still approved
        assert_eq!(proposal.rejection_count, 1);

        // execution still succeeds after timelock
        env.ledger().with_mut(|l| l.timestamp = 3601);
        client.execute(&o3, &pid);
        assert!(client.get_proposal(&pid).unwrap().executed);
    }

    /// TC3 — Immediate threshold check: get_proposal returns approvals=1 right
    /// after propose(), confirming threshold=1 is satisfied by the proposer alone.
    #[test]
    fn test_threshold_1_immediate_approval_count() {
        let env = Env::default();
        env.mock_all_auths();

        let (client, o1, _, _, token_id, recipient) = setup_1of3_funded(&env);

        let pid = client.propose(&o1, &recipient, &token_id, &100);
        assert_eq!(client.get_approval_count(&pid), 1);
        assert_eq!(client.get_threshold(), 1);
    }

    /// Non-owner cannot provide the single required signature.
    #[test]
    fn test_threshold_1_non_owner_cannot_propose() {
        let env = Env::default();
        env.mock_all_auths();

        let (client, _, _, _, token_id, recipient) = setup_1of3_funded(&env);
        let non_owner = Address::generate(&env);

        let result = client.try_propose(&non_owner, &recipient, &token_id, &100);
        assert_eq!(result, Err(Ok(MultisigError::Unauthorized)));
    }

    // ── Non-owner propose() rejection ─────────────────────────────────────────

    #[test]
    fn test_non_owner_propose_reverts() {
        // A caller not in the owner list must be rejected
        let env = Env::default();
        env.mock_all_auths();
        let (client, _, _, _) = setup_2of3(&env);
        let non_owner = Address::generate(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let result = client.try_propose(&non_owner, &to, &token, &500);
        assert_eq!(result, Err(Ok(MultisigError::Unauthorized)));
    }

    #[test]
    fn test_non_owner_propose_returns_unauthorized_error() {
        // Verify the specific error variant is Unauthorized, not any other error
        let env = Env::default();
        env.mock_all_auths();
        let (client, _, _, _) = setup_2of3(&env);
        let non_owner = Address::generate(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        match client.try_propose(&non_owner, &to, &token, &500) {
            Err(Ok(err)) => assert_eq!(err, MultisigError::Unauthorized),
            other => panic!("expected Unauthorized, got {:?}", other),
        }
    }

    #[test]
    fn test_non_owner_propose_creates_no_proposal() {
        // After a failed propose(), no proposal should exist and the counter stays at 0
        let env = Env::default();
        env.mock_all_auths();
        let (client, _, _, _) = setup_2of3(&env);
        let non_owner = Address::generate(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let _ = client.try_propose(&non_owner, &to, &token, &500);

        // Proposal ID 0 must not exist
        assert_eq!(
            client.try_get_proposal(&0).unwrap_err().unwrap(),
            MultisigError::ProposalNotFound
        );
        // Approval count for a non-existent proposal returns 0
        assert_eq!(client.get_approval_count(&0), 0);
    }

    // ── Token balance verification after execute() ────────────────────────────

    /// After a proposal is approved, the timelock elapses, and execute() is called,
    /// the recipient's token balance must increase by exactly the proposed amount,
    /// and the multisig contract's balance must decrease by the same amount.
    #[test]
    fn test_execute_transfers_exact_token_amount_to_recipient() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        // Step 1: Fund the multisig contract with tokens
        const TIMELOCK: u64 = 3600;
        const TRANSFER_AMOUNT: i128 = 250;
        const FUNDED_AMOUNT: i128 = 1000;

        let (client, [o1, o2, o3], token_id, recipient, contract_id) = setup_funded(&env, TIMELOCK);

        let token = soroban_sdk::token::Client::new(&env, &token_id);

        // Verify initial balances
        let initial_contract_balance = token.balance(&contract_id);
        let initial_recipient_balance = token.balance(&recipient);
        assert_eq!(initial_contract_balance, FUNDED_AMOUNT);
        assert_eq!(initial_recipient_balance, 0);

        // Step 2: Propose a transfer of a specific amount to the recipient
        let pid = client.propose(&o1, &recipient, &token_id, &TRANSFER_AMOUNT);

        // Step 3: Approve to reach the 2-of-3 threshold
        client.approve(&o2, &pid);

        // Step 4: Advance past the timelock and execute
        env.ledger().with_mut(|l| l.timestamp = TIMELOCK + 1);
        client.execute(&o3, &pid);

        // Step 5: Verify recipient balance increased by exactly the proposed amount
        let final_recipient_balance = token.balance(&recipient);
        assert_eq!(
            final_recipient_balance,
            initial_recipient_balance + TRANSFER_AMOUNT,
            "recipient balance must increase by exactly the proposed amount"
        );

        // Step 6: Verify multisig balance decreased by the same amount
        let final_contract_balance = token.balance(&contract_id);
        assert_eq!(
            final_contract_balance,
            initial_contract_balance - TRANSFER_AMOUNT,
            "multisig balance must decrease by exactly the proposed amount"
        );

        // Sanity check: no tokens created or destroyed
        assert_eq!(
            final_recipient_balance + final_contract_balance,
            FUNDED_AMOUNT
        );
    }

    /// Test that propose() emits a proposal_created event with correct payload
    #[test]
    fn test_propose_emits_event() {
        use soroban_sdk::testutils::Events;
        use soroban_sdk::TryFromVal;
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "proposal_created"))
                .unwrap_or(false)
                && <(u64, Address, Address, Address, i128)>::try_from_val(&env, &data)
                    .map(|(id, proposer, recipient, tok, amt)| {
                        id == pid && proposer == o1 && recipient == to && tok == token && amt == 500
                    })
                    .unwrap_or(false)
        });
        assert!(found, "Expected proposal_created event not found");
    }

    /// Test for issue #213: In a 1-of-3 multisig, approved_at is set during propose()
    /// (when proposer's auto-approval meets threshold) and is not overwritten by subsequent approve() calls.
    #[test]
    fn test_1of3_approved_at_set_at_propose_not_overwritten() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);

        let (client, o1, o2, o3, _, _) = setup_1of3_funded(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        // propose() auto-approves proposer; threshold=1 is met immediately
        let pid = client.propose(&o1, &to, &token, &100);
        let proposal_after_propose = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal_after_propose.approval_count, 1);
        let approved_at_from_propose = proposal_after_propose.approved_at;
        assert_eq!(approved_at_from_propose, Some(1000));

        // Advance time and have another owner approve
        env.ledger().with_mut(|l| l.timestamp = 2000);
        client.approve(&o2, &pid);

        // Verify approved_at was NOT overwritten
        let proposal_after_approve = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal_after_approve.approval_count, 2);
        assert_eq!(
            proposal_after_approve.approved_at, approved_at_from_propose,
            "approved_at must not be overwritten by subsequent approve()"
        );
        assert_eq!(proposal_after_approve.approved_at, Some(1000));

        // Verify a third approval also doesn't change approved_at
        env.ledger().with_mut(|l| l.timestamp = 3000);
        client.approve(&o3, &pid);
        let proposal_after_third_approve = client.get_proposal(&pid).unwrap();
        assert_eq!(proposal_after_third_approve.approved_at, Some(1000));
    }

    /// Test that approve() emits a proposal_approved event with correct payload
    #[test]
    fn test_approve_emits_event() {
        use soroban_sdk::testutils::Events;
        use soroban_sdk::TryFromVal;
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);
        client.approve(&o2, &pid);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "proposal_approved"))
                .unwrap_or(false)
                && <(u64, Address, u32)>::try_from_val(&env, &data)
                    .map(|(id, owner, count)| id == pid && owner == o2 && count == 2)
                    .unwrap_or(false)
        });
        assert!(found, "Expected proposal_approved event not found");
    }

    /// Test that reject() emits a proposal_rejected event with correct payload
    #[test]
    fn test_reject_emits_event() {
        use soroban_sdk::testutils::Events;
        use soroban_sdk::TryFromVal;
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, _) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);
        client.reject(&o2, &pid);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "proposal_rejected"))
                .unwrap_or(false)
                && <(u64, Address, u32)>::try_from_val(&env, &data)
                    .map(|(id, owner, count)| id == pid && owner == o2 && count == 1)
                    .unwrap_or(false)
        });
        assert!(found, "Expected proposal_rejected event not found");
    }

    /// Test that reject() on a cancelled proposal reverts with AlreadyCancelled
    #[test]
    fn test_reject_on_cancelled_proposal_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, _o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);

        // Cancel the proposal
        client.cancel(&o1, &pid);

        // Try to reject the cancelled proposal
        let result = client.try_reject(&o2, &pid);
        assert_eq!(result, Err(Ok(MultisigError::AlreadyCancelled)));

        // Verify proposal is still cancelled
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.cancelled);
    }

    /// Test that proposer can cancel their own proposal before execution
    #[test]
    fn test_proposer_can_cancel_own_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _o2, _o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);

        // Proposer cancels their own proposal
        let result = client.try_cancel(&o1, &pid);
        assert!(result.is_ok());

        // Verify proposal is cancelled
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.cancelled);
    }

    /// Test that non-proposer can cancel a proposal that can no longer reach threshold
    #[test]
    fn test_non_proposer_can_cancel_dead_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);

        // o2 rejects, making it impossible to reach threshold (2 approvals needed, only 1 possible)
        client.reject(&o2, &pid);

        // o3 can cancel because remaining possible approvals (1) < threshold (2)
        let result = client.try_cancel(&o3, &pid);
        assert!(result.is_ok());

        // Verify proposal is cancelled
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.cancelled);
    }

    #[test]
    fn test_cancel_returns_not_initialized_when_threshold_missing() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        let owners = vec![&env, o1.clone(), o2.clone(), o3.clone()];
        client.initialize(&owners, &2, &0);

        let token = Address::generate(&env);
        let to = Address::generate(&env);
        let pid = client.propose(&o1, &to, &token, &500);

        env.as_contract(&contract_id, || {
            env.storage().instance().remove(&DataKey::Threshold);
        });

        let result = client.try_cancel(&o2, &pid);
        assert_eq!(result, Err(Ok(MultisigError::NotInitialized)));
    }

    /// Test that non-proposer cannot cancel a proposal that can still reach threshold
    #[test]
    fn test_non_proposer_cannot_cancel_active_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, o2, _o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);

        // o2 tries to cancel, but proposal can still reach threshold
        let result = client.try_cancel(&o2, &pid);
        assert_eq!(result, Err(Ok(MultisigError::CannotCancel)));

        // Verify proposal is not cancelled
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(!proposal.cancelled);
    }

    /// Test that cancel() on an already cancelled proposal reverts with AlreadyCancelled
    #[test]
    fn test_cancel_already_cancelled_proposal_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, o1, _o2, _o3) = setup_2of3(&env);
        let token = Address::generate(&env);
        let to = Address::generate(&env);

        let pid = client.propose(&o1, &to, &token, &500);

        // Cancel the proposal
        client.cancel(&o1, &pid);

        // Try to cancel again
        let result = client.try_cancel(&o1, &pid);
        assert_eq!(result, Err(Ok(MultisigError::AlreadyCancelled)));
    }

    /// Test that cancel() on an executed proposal reverts with AlreadyExecuted
    #[test]
    fn test_cancel_executed_proposal_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &2, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        let pid = client.propose(&o1, &to, &token_id, &500);
        client.approve(&o2, &pid);

        env.ledger().with_mut(|l| l.timestamp = 7200);
        client.execute(&o3, &pid);

        // Try to cancel the executed proposal
        let result = client.try_cancel(&o1, &pid);
        assert_eq!(result, Err(Ok(MultisigError::AlreadyExecuted)));
    }

    /// Test that execute() emits a proposal_executed event with correct payload
    #[test]
    fn test_execute_emits_event() {
        use soroban_sdk::testutils::Events;
        use soroban_sdk::TryFromVal;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone(), o3.clone()], &2, &3600);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let to = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        let pid = client.propose(&o1, &to, &token_id, &500);
        client.approve(&o2, &pid);

        env.ledger().with_mut(|l| l.timestamp = 7200);
        client.execute(&o3, &pid);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "proposal_executed"))
                .unwrap_or(false)
                && <(u64, Address, Address, i128)>::try_from_val(&env, &data)
                    .map(|(id, executor, recipient, amt)| {
                        id == pid && executor == o3 && recipient == to && amt == 500
                    })
                    .unwrap_or(false)
        });
        assert!(found, "Expected proposal_executed event not found");
    }

    /// Test that ownership checks are correct with a large owner set (10 owners).
    /// Verifies O(1) IsOwner map correctly identifies owners and non-owners.
    #[test]
    fn test_large_owner_set_ownership_checks() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);

        // Generate 10 owners
        let owners: std::vec::Vec<Address> = (0..10).map(|_| Address::generate(&env)).collect();
        let sdk_owners = {
            let mut v = soroban_sdk::Vec::new(&env);
            for o in owners.iter() {
                v.push_back(o.clone());
            }
            v
        };

        // 6-of-10 multisig
        client.initialize(&sdk_owners, &6, &0);

        // All 10 owners must be recognised
        for owner in owners.iter() {
            assert!(client.is_owner(owner), "Expected address to be an owner");
        }

        // A freshly generated address must NOT be an owner
        let non_owner = Address::generate(&env);
        assert!(
            !client.is_owner(&non_owner),
            "Expected address to not be an owner"
        );

        // get_owners() must still return all 10
        assert_eq!(client.get_owners().len(), 10);

        // A non-owner cannot propose (Unauthorized)
        let token = Address::generate(&env);
        let to = Address::generate(&env);
        let result = client.try_propose(&non_owner, &to, &token, &100);
        assert_eq!(result, Err(Ok(MultisigError::Unauthorized)));

        // An owner can propose successfully
        let result = client.try_propose(&owners[0], &to, &token, &100);
        assert!(result.is_ok());
    }

    // ── CommittedAmount / over-commitment tests ───────────────────────────────

    fn setup_token(env: &Env, contract_id: &Address, amount: i128) -> Address {
        use soroban_sdk::token::StellarAssetClient;
        let token_admin = Address::generate(env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(env, &token_id).mint(contract_id, &amount);
        token_id
    }

    /// Two proposals approved against the same treasury cannot both drain it.
    /// The second execute must fail with InsufficientFunds.
    #[test]
    fn test_two_proposals_cannot_double_drain_treasury() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone()], &2, &0);

        // Treasury has 1000 tokens
        let token_id = setup_token(&env, &contract_id, 1000);
        let recipient = Address::generate(&env);

        // Propose two transfers of 800 each — together they exceed the 1000 balance
        let pid1 = client.propose(&o1, &recipient, &token_id, &800);
        let pid2 = client.propose(&o1, &recipient, &token_id, &800);

        // Approve both to threshold
        client.approve(&o2, &pid1);
        client.approve(&o2, &pid2);

        // committed = 1600, balance = 1000 → first execute succeeds
        let result1 = client.try_execute(&o1, &pid1);
        assert!(result1.is_ok(), "first execute should succeed");

        // After first execute: balance = 200, committed = 800 → second must fail
        let result2 = client.try_execute(&o1, &pid2);
        assert_eq!(result2, Err(Ok(MultisigError::InsufficientFunds)));
    }

    /// get_committed_amount tracks the lifecycle: 0 → committed → released on execute.
    #[test]
    fn test_committed_amount_lifecycle() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone()], &2, &0);

        let token_id = setup_token(&env, &contract_id, 1000);
        let recipient = Address::generate(&env);

        assert_eq!(client.get_committed_amount(&token_id), 0);

        let pid = client.propose(&o1, &recipient, &token_id, &300);
        // Not yet at threshold — committed still 0
        assert_eq!(client.get_committed_amount(&token_id), 0);

        client.approve(&o2, &pid);
        // Threshold reached — committed = 300
        assert_eq!(client.get_committed_amount(&token_id), 300);

        client.execute(&o1, &pid);
        // Executed — committed back to 0
        assert_eq!(client.get_committed_amount(&token_id), 0);
    }

    /// Cancelling an approved proposal releases its committed amount.
    #[test]
    fn test_cancel_approved_proposal_releases_committed_amount() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone()], &2, &0);

        let token_id = setup_token(&env, &contract_id, 1000);
        let recipient = Address::generate(&env);

        let pid = client.propose(&o1, &recipient, &token_id, &500);
        client.approve(&o2, &pid);
        assert_eq!(client.get_committed_amount(&token_id), 500);

        // Proposer cancels — committed must be released
        client.cancel(&o1, &pid);
        assert_eq!(client.get_committed_amount(&token_id), 0);
    }

    /// Issue #266: execute() returns InsufficientFunds when the contract holds no tokens.
    #[test]
    fn test_execute_returns_insufficient_funds_when_no_tokens() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone()], &2, &0);

        // Register a token but do NOT mint any to the contract
        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let recipient = Address::generate(&env);

        let pid = client.propose(&o1, &recipient, &token_id, &500);
        client.approve(&o2, &pid);

        // Contract has zero balance — execute must return InsufficientFunds
        let result = client.try_execute(&o1, &pid);
        assert_eq!(result, Err(Ok(MultisigError::InsufficientFunds)));
    }

    /// Issue #266: execute() succeeds when the contract holds exactly the required balance.
    #[test]
    fn test_execute_succeeds_with_exact_required_balance() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        client.initialize(&vec![&env, o1.clone(), o2.clone()], &2, &0);

        // Mint exactly the proposed amount to the contract
        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);
        let recipient = Address::generate(&env);

        let pid = client.propose(&o1, &recipient, &token_id, &500);
        client.approve(&o2, &pid);

        // Contract has exactly 500 — execute must succeed
        let result = client.try_execute(&o1, &pid);
        assert!(result.is_ok(), "execute should succeed with exact balance");
        assert!(client.get_proposal(&pid).unwrap().executed);
    }

    // ── Native XLM proposal tests ─────────────────────────────────────────────

    /// Helper: 2-of-3 multisig with zero timelock, funded with native XLM via SAC.
    fn setup_xlm_funded<'a>(
        env: &'a Env,
    ) -> (
        MultisigContractClient<'a>,
        [Address; 3],
        Address,
        Address,
        Address,
    ) {
        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(env, &contract_id);
        let o1 = Address::generate(env);
        let o2 = Address::generate(env);
        let o3 = Address::generate(env);
        client.initialize(&vec![env, o1.clone(), o2.clone(), o3.clone()], &2, &0);

        let xlm_sac = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        soroban_sdk::token::StellarAssetClient::new(env, &xlm_sac)
            .mint(&contract_id, &1_000_000_000); // 100 XLM in stroops

        let recipient = Address::generate(env);
        (client, [o1, o2, o3], xlm_sac, recipient, contract_id)
    }

    /// propose_xlm() creates a proposal with is_native=true and correct fields.
    #[test]
    fn test_propose_xlm_creates_native_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, [o1, _, _], xlm_sac, recipient, _) = setup_xlm_funded(&env);

        let amount: i128 = 100_000_000; // 10 XLM
        let pid = client.propose_xlm(&o1, &recipient, &xlm_sac, &amount);

        let proposal = client.get_proposal(&pid).unwrap();
        assert!(proposal.is_native, "proposal must be marked as native XLM");
        assert_eq!(proposal.token, xlm_sac);
        assert_eq!(proposal.to, recipient);
        assert_eq!(proposal.amount, amount);
        assert_eq!(proposal.approval_count, 1); // proposer auto-approves
        assert!(!proposal.executed);
        assert!(!proposal.cancelled);
    }

    /// Full flow: propose_xlm → approve → execute transfers XLM to recipient.
    #[test]
    fn test_propose_xlm_approve_execute_transfers_xlm() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, [o1, o2, o3], xlm_sac, recipient, _) = setup_xlm_funded(&env);

        let amount: i128 = 100_000_000; // 10 XLM
        let pid = client.propose_xlm(&o1, &recipient, &xlm_sac, &amount);

        // o2 approves — threshold=2 reached
        client.approve(&o2, &pid);
        assert!(client.get_proposal(&pid).unwrap().approved_at.is_some());

        // execute (zero timelock — no advance needed)
        client.execute(&o3, &pid);

        // recipient received the XLM
        let token_client = soroban_sdk::token::Client::new(&env, &xlm_sac);
        assert_eq!(token_client.balance(&recipient), amount);
        assert!(client.get_proposal(&pid).unwrap().executed);
    }

    /// propose_xlm() by a non-owner must revert with Unauthorized.
    #[test]
    fn test_propose_xlm_non_owner_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _, xlm_sac, recipient, _) = setup_xlm_funded(&env);
        let non_owner = Address::generate(&env);

        let result = client.try_propose_xlm(&non_owner, &recipient, &xlm_sac, &100_000_000);
        assert_eq!(result, Err(Ok(MultisigError::Unauthorized)));
    }

    /// propose_xlm() with amount=0 must revert with InvalidAmount.
    #[test]
    fn test_propose_xlm_zero_amount_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, [o1, _, _], xlm_sac, recipient, _) = setup_xlm_funded(&env);

        let result = client.try_propose_xlm(&o1, &recipient, &xlm_sac, &0);
        assert_eq!(result, Err(Ok(MultisigError::InvalidAmount)));
    }

    /// A regular token proposal and a native XLM proposal can coexist and both
    /// execute correctly — is_native does not bleed across proposals.
    #[test]
    fn test_token_and_xlm_proposals_coexist() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, [o1, o2, o3], xlm_sac, recipient, contract_id) = setup_xlm_funded(&env);

        // Also fund the contract with a regular token
        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500);

        // Token proposal
        let pid_token = client.propose(&o1, &recipient, &token_id, &200);
        client.approve(&o2, &pid_token);

        // XLM proposal
        let pid_xlm = client.propose_xlm(&o1, &recipient, &xlm_sac, &50_000_000);
        client.approve(&o2, &pid_xlm);

        // Verify is_native is set correctly on each
        assert!(!client.get_proposal(&pid_token).unwrap().is_native);
        assert!(client.get_proposal(&pid_xlm).unwrap().is_native);

        // Execute both
        client.execute(&o3, &pid_token);
        client.execute(&o3, &pid_xlm);

        let token_client = soroban_sdk::token::Client::new(&env, &token_id);
        let xlm_client = soroban_sdk::token::Client::new(&env, &xlm_sac);
        assert_eq!(token_client.balance(&recipient), 200);
        assert_eq!(xlm_client.balance(&recipient), 50_000_000);
    }

    /// If execute() traps before the transfer completes (e.g. InsufficientFunds),
    /// proposal.executed must remain false so the proposal can be retried once
    /// the treasury is funded. This guards against the pre-transfer executed=true
    /// bug that would permanently lock funds.
    #[test]
    fn test_execute_does_not_mark_executed_on_failed_transfer() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        // Zero timelock so we can execute immediately
        client.initialize(&vec![&env, o1.clone(), o2.clone()], &2, &0);

        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let recipient = Address::generate(&env);

        // Propose 500 but do NOT fund the contract — transfer will fail
        let pid = client.propose(&o1, &recipient, &token_id, &500);
        client.approve(&o2, &pid);

        // Execute must return InsufficientFunds
        let result = client.try_execute(&o1, &pid);
        assert_eq!(result, Err(Ok(MultisigError::InsufficientFunds)));

        // proposal.executed must still be false — proposal is retryable
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(
            !proposal.executed,
            "executed must remain false when transfer fails"
        );
    }

    // ── Issue #336: cancel() by non-proposer boundary — unreachable threshold ──

    /// 3-of-5 multisig: verifies the exact boundary where cancel() by a non-proposer
    /// is blocked (remaining_possible == threshold) vs allowed (remaining_possible < threshold).
    ///
    /// Timeline:
    ///   o1 proposes  → approval=1, rejection=0, remaining_possible=4  (4 >= 3 → cannot cancel)
    ///   o2 rejects   → approval=1, rejection=1, remaining_possible=3  (3 >= 3 → cannot cancel)
    ///   o3 rejects   → approval=1, rejection=2, remaining_possible=2  (2 < 3  → can cancel)
    ///   o4 tries cancel at remaining=2 → CannotCancel (boundary: 2 == threshold-1? No: 2 < 3)
    ///
    /// Wait — let's be precise per the contract logic:
    ///   remaining_possible = total_owners - rejection_count - approval_count
    ///   cancel allowed when remaining_possible < threshold
    ///
    ///   After o1 proposes (approval=1, rejection=0): remaining = 5-0-1 = 4; 4 >= 3 → CannotCancel
    ///   After o2 rejects  (approval=1, rejection=1): remaining = 5-1-1 = 3; 3 >= 3 → CannotCancel
    ///   After o3 rejects  (approval=1, rejection=2): remaining = 5-2-1 = 2; 2 < 3  → can cancel
    ///   o4 attempts cancel at remaining=3 (after o2 rejects) → CannotCancel
    ///   o4 rejects        (approval=1, rejection=3): remaining = 5-3-1 = 1; 1 < 3  → can cancel
    ///   o5 calls cancel() → success, proposal.cancelled == true
    #[test]
    fn test_cancel_non_proposer_boundary_3of5() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, MultisigContract);
        let client = MultisigContractClient::new(&env, &contract_id);
        let o1 = Address::generate(&env);
        let o2 = Address::generate(&env);
        let o3 = Address::generate(&env);
        let o4 = Address::generate(&env);
        let o5 = Address::generate(&env);

        // 3-of-5 multisig, zero timelock
        client.initialize(
            &vec![
                &env,
                o1.clone(),
                o2.clone(),
                o3.clone(),
                o4.clone(),
                o5.clone(),
            ],
            &3,
            &0,
        );

        let token = Address::generate(&env);
        let to = Address::generate(&env);

        // o1 proposes — auto-approves (approval=1, rejection=0, remaining=4)
        let pid = client.propose(&o1, &to, &token, &100);
        {
            let p = client.get_proposal(&pid).unwrap();
            assert_eq!(p.approval_count, 1);
            assert_eq!(p.rejection_count, 0);
        }

        // o2 rejects → rejection=1, remaining = 5-1-1 = 3; 3 >= 3 → CannotCancel
        client.reject(&o2, &pid);
        {
            let p = client.get_proposal(&pid).unwrap();
            assert_eq!(p.rejection_count, 1);
        }
        // o4 attempts cancel at this point — remaining=3 == threshold → CannotCancel
        let result = client.try_cancel(&o4, &pid);
        assert_eq!(
            result,
            Err(Ok(MultisigError::CannotCancel)),
            "cancel must fail when remaining_possible == threshold (3 == 3)"
        );

        // o3 rejects → rejection=2, remaining = 5-2-1 = 2; 2 < 3 → can cancel
        client.reject(&o3, &pid);
        {
            let p = client.get_proposal(&pid).unwrap();
            assert_eq!(p.rejection_count, 2);
        }

        // o4 rejects → rejection=3, remaining = 5-3-1 = 1; 1 < 3 → can cancel
        client.reject(&o4, &pid);
        {
            let p = client.get_proposal(&pid).unwrap();
            assert_eq!(p.rejection_count, 3);
        }

        // o5 calls cancel() — remaining=1 < threshold=3 → success
        let result = client.try_cancel(&o5, &pid);
        assert!(
            result.is_ok(),
            "cancel must succeed when threshold is unreachable"
        );

        // Verify proposal is cancelled
        let proposal = client.get_proposal(&pid).unwrap();
        assert!(
            proposal.cancelled,
            "proposal.cancelled must be true after successful cancel"
        );
    }
}
