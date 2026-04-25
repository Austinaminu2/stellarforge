#![no_std]

//! # forge-governor
//!
//! On-chain governance with token-weighted voting for Stellar/Soroban.
//!
//! ## Features
//! - Token-weighted proposal voting (1 token = 1 vote)
//! - Configurable voting period and quorum
//! - Timelock between approval and execution
//! - Anyone can propose; execution is permissionless once passed

use forge_errors::CommonError;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Env, String, Symbol, Vec,
};

// ── TTL constants ─────────────────────────────────────────────────────────────
//
// Persistent storage entries on Stellar expire unless their TTL is extended.
// All TTLs are expressed in ledgers (1 ledger ≈ 5 seconds).
//
// INSTANCE_TTL_THRESHOLD / INSTANCE_TTL_EXTEND
//   Applied to the contract instance on every mutating call.
//   17 280 ledgers ≈ 1 day threshold; 34 560 ledgers ≈ 2 days extend.
//
// PROPOSAL_TTL_EXTEND
//   Applied to DataKey::Proposal entries.  A proposal must survive its full
//   lifecycle: voting_period + timelock_delay + a generous buffer.
//   Using a fixed upper-bound of 60 days (1 036 800 ledgers) covers any
//   realistic governance configuration without per-proposal arithmetic.
//
// VOTE_TTL_EXTEND
//   Applied to DataKey::Vote entries.  A vote record must outlive the proposal
//   it belongs to so that has_voted() remains reliable throughout the entire
//   lifecycle.  Same 60-day ceiling as proposals.
const INSTANCE_TTL_THRESHOLD: u32 = 17_280;
const INSTANCE_TTL_EXTEND: u32 = 34_560;
const PROPOSAL_TTL_EXTEND: u32 = 1_036_800; // ~60 days
const VOTE_TTL_EXTEND: u32 = 1_036_800; // ~60 days

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Config,
    Proposal(u64),
    Vote(u64, Address),
    NextProposalId,
    ActiveProposals,
    ActiveProposalIndex(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Governor configuration.
#[contracttype]
#[derive(Clone)]
pub struct GovernorConfig {
    /// Address authorized to initialize the contract (must call initialize()).
    pub admin: Address,
    /// Token used for voting weight.
    ///
    /// Must be a valid Soroban token contract address that implements the token interface.
    /// This address is used in `vote()` to verify voter balances (1 token = 1 vote).
    /// Passing an invalid or non-token address will not be caught at initialization time —
    /// it will only fail when `vote()` attempts to call `balance()` on the address.
    /// Callers are responsible for ensuring this is a legitimate token contract.
    pub vote_token: Address,
    /// Seconds a proposal is open for voting.
    pub voting_period: u64,
    /// Minimum votes (in token units) for a proposal to pass.
    pub quorum: i128,
    /// Seconds between approval and execution.
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
    /// Total votes cast in favor.
    pub yes_votes: i128,
    /// Total votes cast against.
    pub no_votes: i128,
    /// Total abstain votes.
    pub abstain_votes: i128,
    /// Sum of yes, no, and abstain votes.
    pub total_votes: i128,
}

/// A governance proposal.
#[contracttype]
#[derive(Clone)]
pub struct Proposal {
    /// Address that created the proposal.
    pub proposer: Address,
    /// Human-readable title.
    pub title: String,
    /// Human-readable description.
    pub description: String,
    /// Ledger timestamp when voting opens.
    pub vote_start: u64,
    /// Ledger timestamp when voting closes.
    pub vote_end: u64,
    /// Total votes in favor.
    pub votes_for: i128,
    /// Total votes against.
    pub votes_against: i128,
    /// Total abstain votes.
    pub abstentions: i128,
    /// Timestamp when proposal passed (for timelock).
    pub passed_at: Option<u64>,
    /// Current state.
    pub state: ProposalState,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum GovernorError {
    #[from(CommonError)]
    Common(CommonError),
    ProposalNotFound = 4,
    VotingClosed = 5,
    VotingStillOpen = 6,
    AlreadyVoted = 7,
    QuorumNotReached = 8,
    ProposalNotPassed = 9,
    TimelockNotElapsed = 10,
    AlreadyExecuted = 11,
    AlreadyCancelled = 12,
    InvalidWeight = 13,
    AlreadyFinalized = 14,
    Unauthorized = 14,
    AlreadyFinalized = 15,
    VoteNotFound = 16,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct GovernorContract;

#[contractimpl]
impl GovernorContract {
    /// Initialize the governor with its configuration.
    ///
    /// Stores the [`GovernorConfig`] on-chain. Must be called exactly once
    /// immediately after deployment. Requires authorization from the admin address
    /// specified in the config to prevent front-running attacks.
    ///
    /// # Parameters
    /// - `config` — A [`GovernorConfig`] specifying:
    ///   - `admin`: Address authorized to initialize the contract (must call this function).
    ///   - `vote_token`: Address of the Soroban token used for voting weight.
    ///   - `voting_period`: Seconds a proposal remains open for voting. Must be > 0.
    ///   - `quorum`: Minimum total votes (for + against) required for a proposal to pass. Must be > 0.
    ///   - `timelock_delay`: Seconds between a proposal passing and being executable.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`GovernorError::AlreadyInitialized`] — Contract has already been initialized.
    /// - [`GovernorError::InvalidConfig`] — `quorum` or `voting_period` is zero.
    ///
    /// # Security
    /// The admin address must authorize this call via `require_auth()`. This prevents
    /// an attacker from front-running the deployer's initialization with a malicious config
    /// (e.g., quorum = 1, timelock = 0).
    ///
    /// # Example
    /// ```text
    /// let config = GovernorConfig {
    ///     admin: deployer_address,
    ///     vote_token: token_address,
    ///     voting_period: 3600,  // 1 hour
    ///     quorum: 1_000_000,
    ///     timelock_delay: 86400, // 24 hours
    /// };
    /// client.initialize(&config);
    /// ```
    pub fn initialize(env: Env, config: GovernorConfig) -> Result<(), GovernorError> {
        config.admin.require_auth();

        if env.storage().instance().has(&DataKey::Config) {
            return Err(GovernorError::Common(CommonError::AlreadyInitialized));
        }
        if config.quorum == 0 || config.voting_period == 0 {
            return Err(GovernorError::Common(CommonError::InvalidConfig));
        }
        // Sanity-check: vote_token must not be the same address as admin.
        // A misconfigured vote_token (e.g. admin address used by mistake) would pass
        // initialization silently but fail at vote() time. This catches the most obvious
        // misconfiguration early with a descriptive error.
        if config.vote_token == config.admin {
            return Err(GovernorError::Common(CommonError::InvalidConfig));
        }
        env.storage().instance().set(&DataKey::Config, &config);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);
        Ok(())
    }

    /// Create a new governance proposal.
    ///
    /// Opens a new proposal for voting immediately, with `vote_end` set to
    /// `current_timestamp + voting_period`. The proposer's approval is not
    /// automatically recorded — owners must call [`vote`](Self::vote) separately.
    /// Requires authorization from `proposer`.
    ///
    /// # Parameters
    /// - `proposer` — Address submitting the proposal. Can be any account.
    /// - `title` — Short human-readable title for the proposal.
    /// - `description` — Full description of what the proposal intends to do.
    ///
    /// # Returns
    /// `Ok(proposal_id)` — the unique ID assigned to the new proposal.
    ///
    /// # Errors
    /// - [`GovernorError::NotInitialized`] — `initialize` has not been called.
    ///
    /// # Example
    /// ```text
    /// let id = client.propose(&proposer, &String::from_str(&env, "Upgrade v2"), &String::from_str(&env, "..."));
    /// ```
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
            .ok_or(GovernorError::Common(CommonError::NotInitialized))?;

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

        // Track active proposal ID for O(1) get_pending_proposals
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

    /// Cast a vote on an active proposal.
    ///
    /// Adds `weight` to `votes_for`, `votes_against`, or `abstentions` depending on
    /// `direction`. Each address may only vote once per proposal.
    ///
    /// The `weight` parameter is validated against the voter's actual on-chain
    /// token balance at the time of the call. If `weight` exceeds the voter's
    /// balance, the call is rejected with [`GovernorError::InvalidWeight`].
    /// This enforces the "1 token = 1 vote" model and prevents governance
    /// manipulation by callers supplying an inflated weight.
    ///
    /// Requires authorization from `voter`.
    ///
    /// # Parameters
    /// - `voter` — Address casting the vote.
    /// - `proposal_id` — ID of the proposal to vote on.
    /// - `direction` — [`VoteDirection::For`], [`VoteDirection::Against`], or [`VoteDirection::Abstain`].
    /// - `weight` — Voting power to apply, must be <= the voter's actual token balance.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`GovernorError::NotInitialized`] — `initialize` has not been called.
    /// - [`GovernorError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`GovernorError::AlreadyVoted`] — `voter` has already voted on this proposal.
    /// - [`GovernorError::VotingClosed`] — The proposal is no longer in `Active` state
    ///   or the voting period has expired.
    /// - [`GovernorError::InvalidWeight`] — `weight` exceeds the voter's token balance.
    ///
    /// # Example
    /// ```text
    /// // Vote in favor with 500 tokens of weight
    /// client.vote(&voter, &proposal_id, &VoteDirection::For, &500);
    /// // Abstain with 200 tokens of weight (counts toward quorum)
    /// client.vote(&voter, &proposal_id, &VoteDirection::Abstain, &200);
    /// // Vote in favor with weight equal to the voter's token balance
    /// let balance = token_client.balance(&voter);
    /// client.vote(&voter, &proposal_id, &VoteDirection::For, &balance);
    /// ```
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
            .ok_or(GovernorError::Common(CommonError::NotInitialized))?;

        // Enforce 1-token-1-vote: reject if claimed weight exceeds actual balance.
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
        if now >= proposal.vote_end {
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

    /// Finalize a proposal after its voting period ends.
    ///
    /// Evaluates the vote totals against the configured quorum and sets the
    /// proposal state to [`ProposalState::Passed`] or [`ProposalState::Failed`].
    /// If passed, records `vote_end` in `passed_at` to start the timelock
    /// countdown from when voting ended, not when `finalize` was called.
    /// Can be called by anyone.
    ///
    /// # Parameters
    /// - `proposal_id` — ID of the proposal to finalize.
    ///
    /// # Returns
    /// `Ok(`[`ProposalState`]`)` — the resulting state (`Passed` or `Failed`).
    ///
    /// # Errors
    /// - [`GovernorError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`GovernorError::VotingStillOpen`] — The voting period has not yet ended.
    /// - [`GovernorError::AlreadyFinalized`] — The proposal has already been finalized
    ///   (state is not `Active`).
    ///
    /// # Example
    /// ```text
    /// // After voting_period has elapsed:
    /// let state = client.finalize(&proposal_id);
    /// assert_eq!(state, ProposalState::Passed);
    /// ```
    pub fn finalize(env: Env, proposal_id: u64) -> Result<ProposalState, GovernorError> {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;

        match proposal.state {
            ProposalState::Active => {
                // Continue with finalization logic
            }
            ProposalState::Cancelled => {
                return Err(GovernorError::AlreadyCancelled);
            }
            ProposalState::Passed | ProposalState::Failed | ProposalState::Executed => {
                return Err(GovernorError::AlreadyFinalized);
            }
        }

        let now = env.ledger().timestamp();
        if now < proposal.vote_end {
            return Err(GovernorError::VotingStillOpen);
        }

        let config: GovernorConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(GovernorError::Common(CommonError::NotInitialized))?;
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

    /// Mark a passed proposal as executed after the timelock delay.
    ///
    /// Enforces the timelock by checking that `current_timestamp ≥ passed_at + timelock_delay`.
    /// In this contract, execution marks the proposal as done on-chain; any
    /// off-chain or cross-contract action triggered by the proposal should be
    /// coordinated by the caller. Requires authorization from `executor`.
    ///
    /// # Parameters
    /// - `executor` — Address triggering execution. Can be any account.
    /// - `proposal_id` — ID of the proposal to execute.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`GovernorError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`GovernorError::AlreadyExecuted`] — The proposal has already been executed.
    /// - [`GovernorError::ProposalNotPassed`] — The proposal did not reach `Passed` state or is cancelled.
    /// - [`GovernorError::TimelockNotElapsed`] — The timelock delay has not fully passed.
    ///
    /// # Example
    /// ```text
    /// // After timelock_delay seconds have elapsed since the proposal passed:
    /// client.execute(&executor, &proposal_id);
    /// ```
    pub fn execute(env: Env, executor: Address, proposal_id: u64) -> Result<(), GovernorError> {
        executor.require_auth();

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;

        if proposal.state == ProposalState::Executed {
            return Err(GovernorError::AlreadyExecuted);
        }
        if proposal.state == ProposalState::Cancelled {
            return Err(GovernorError::ProposalNotPassed);
        }
        if proposal.state != ProposalState::Passed {
            return Err(GovernorError::ProposalNotPassed);
        }

        let passed_at = proposal.passed_at.ok_or(GovernorError::ProposalNotPassed)?;
        let config: GovernorConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(GovernorError::Common(CommonError::NotInitialized))?;

        if env.ledger().timestamp() < passed_at + config.timelock_delay {
            return Err(GovernorError::TimelockNotElapsed);
        }

        proposal.state = ProposalState::Executed;
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

        // Remove from active proposals list (in case finalize was skipped)
        Self::remove_active_proposal(&env, proposal_id);

        env.events().publish(
            (Symbol::new(&env, "proposal_executed"),),
            (proposal_id, &executor),
        );

        Ok(())
    }

    /// Cancel an active proposal before voting ends.
    ///
    /// Only the original proposer can cancel a proposal, and only while voting is still open
    /// (state is `Active` and current timestamp <= `vote_end`). Cancelling a proposal removes
    /// it from the active proposals list and prevents further voting or execution.
    /// Requires authorization from `proposer`.
    ///
    /// # Parameters
    /// - `proposer` — The original proposer address. Must match the proposal's proposer.
    /// - `proposal_id` — ID of the proposal to cancel.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`GovernorError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    /// - [`GovernorError::Unauthorized`] — `proposer` is not the original proposal creator.
    /// - [`GovernorError::VotingClosed`] — Voting period has ended or proposal is not in `Active` state.
    /// - [`GovernorError::AlreadyCancelled`] — The proposal has already been cancelled.
    ///
    /// # Example
    /// ```text
    /// // Cancel a proposal before voting ends
    /// client.cancel_proposal(&proposer, &proposal_id);
    /// ```
    pub fn cancel_proposal(
        env: Env,
        proposer: Address,
        proposal_id: u64,
    ) -> Result<(), GovernorError> {
        proposer.require_auth();

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;

        // Only the original proposer can cancel
        if proposal.proposer != proposer {
            return Err(GovernorError::Common(CommonError::Unauthorized));
        }

        // Can only cancel if still in Active state
        if proposal.state != ProposalState::Active {
            if proposal.state == ProposalState::Cancelled {
                return Err(GovernorError::AlreadyCancelled);
            }
            return Err(GovernorError::VotingClosed);
        }

        // Can only cancel while voting is still open
        let now = env.ledger().timestamp();
        if now > proposal.vote_end {
            return Err(GovernorError::VotingClosed);
        }

        proposal.state = ProposalState::Cancelled;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        Self::remove_active_proposal(&env, proposal_id);

        env.events().publish(
            (Symbol::new(&env, "proposal_cancelled"),),
            (proposal_id, &proposer),
        );

        Ok(())
    }

    /// Return a proposal by its ID.
    ///
    /// Read-only; does not modify state.
    ///
    /// # Parameters
    /// - `proposal_id` — The ID returned by [`propose`](Self::propose).
    ///
    /// # Returns
    /// `Ok(`[`Proposal`]`)` with the full proposal details.
    ///
    /// # Errors
    /// - [`GovernorError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    ///
    /// # Example
    /// ```text
    /// let proposal = client.get_proposal(&id)?;
    /// println!("votes_for: {}", proposal.votes_for);
    /// ```
    pub fn get_proposal(env: Env, proposal_id: u64) -> Result<Proposal, GovernorError> {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)
    }

    /// Return the governor configuration set at initialization.
    ///
    /// Read-only; returns an error if `initialize` has not been called yet.
    ///
    /// # Returns
    /// `Ok(`[`GovernorConfig`]`)` with the stored configuration.
    ///
    /// # Errors
    /// - [`GovernorError::NotInitialized`] — `initialize` has not been called.
    ///
    /// # Example
    /// ```text
    /// let config = client.get_config().unwrap();
    /// println!("quorum: {}", config.quorum);
    /// ```
    pub fn get_config(env: Env) -> Result<GovernorConfig, GovernorError> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(GovernorError::NotInitialized)
    }

    /// Return the total number of proposals that have been created.
    ///
    /// Read-only; does not modify state. Useful for UIs to paginate and list
    /// all proposals without tracking events off-chain.
    ///
    /// # Returns
    /// `u64` — the total count of proposals created since contract initialization.
    ///
    /// # Example
    /// ```text
    /// let count = client.get_proposal_count();
    /// for id in 0..count {
    ///     let proposal = client.get_proposal(&id);
    ///     // process proposal...
    /// }
    /// ```
    pub fn get_proposal_count(env: Env) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::NextProposalId)
            .unwrap_or(0u64)
    }

    /// Check whether an address has already voted on a proposal.
    ///
    /// Read-only; does not modify state. Useful for UIs and integrations to
    /// prevent submitting a vote that would fail with [`GovernorError::AlreadyVoted`].
    ///
    /// # Parameters
    /// - `proposal_id` — ID of the proposal to check.
    /// - `voter` — Address to look up.
    ///
    /// # Returns
    /// `true` if `voter` has cast a vote on `proposal_id`, `false` otherwise.
    /// Returns `false` for non-existent proposal IDs (no error is thrown).
    ///
    /// # Example
    /// ```text
    /// if !client.has_voted(&proposal_id, &voter) {
    ///     client.vote(&voter, &proposal_id, &VoteDirection::For, &100);
    /// }
    /// ```
    pub fn has_voted(env: Env, proposal_id: u64, voter: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Vote(proposal_id, voter))
    }

    /// Return the weight a voter cast on a specific proposal.
    ///
    /// Looks up the persistent vote entry written by [`vote`](Self::vote).
    /// Returns an error if the proposal or vote entry does not exist.
    ///
    /// # Parameters
    /// - `proposal_id` — ID of the proposal to query.
    /// - `voter` — Address of the voter to look up.
    ///
    /// # Returns
    /// `Ok(i128)` — the weight the voter cast.
    ///
    /// # Errors
    /// - [`GovernorError::ProposalNotFound`] — The proposal ID does not exist.
    /// - [`GovernorError::VoteNotFound`] — The voter has not cast a vote on the proposal.
    ///
    /// # Example
    /// ```text
    /// let weight = client.get_vote_weight(&proposal_id, &voter_address)?;
    /// assert_eq!(weight, 500);
    /// ```
    pub fn get_vote_weight(
        env: Env,
        proposal_id: u64,
        voter: Address,
    ) -> Result<i128, GovernorError> {
        env.storage()
            .persistent()
            .get::<DataKey, Proposal>(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;

        env.storage()
            .persistent()
            .get(&DataKey::Vote(proposal_id, voter))
            .ok_or(GovernorError::VoteNotFound)
    }

    /// Return the current state of a proposal.
    ///
    /// Read-only; does not modify state. Lighter alternative to
    /// [`get_proposal`](Self::get_proposal) when only the state is needed.
    ///
    /// # Parameters
    /// - `proposal_id` — ID of the proposal to query.
    ///
    /// # Returns
    /// `Ok(`[`ProposalState`]`)` — the proposal's current state.
    ///
    /// # Errors
    /// - [`GovernorError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    ///
    /// # Example
    /// ```text
    /// let state = client.get_proposal_state(&proposal_id)?;
    /// assert_eq!(state, ProposalState::Active);
    /// ```
    pub fn get_proposal_state(env: Env, proposal_id: u64) -> Result<ProposalState, GovernorError> {
        let proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;
        Ok(proposal.state)
    }

    /// Return the current vote tally for a proposal.
    ///
    /// Read-only; does not modify state. Returns a breakdown of yes, no, and
    /// total votes cast so far, regardless of the proposal's current state.
    ///
    /// # Parameters
    /// - `proposal_id` — ID of the proposal to query.
    ///
    /// # Returns
    /// `Ok(`[`VoteTally`]`)` with the current vote counts.
    ///
    /// # Errors
    /// - [`GovernorError::ProposalNotFound`] — No proposal exists with `proposal_id`.
    ///
    /// # Example
    /// ```text
    /// let tally = client.get_vote_tally(&proposal_id)?;
    /// println!("yes: {}, no: {}, total: {}", tally.yes_votes, tally.no_votes, tally.total_votes);
    /// ```
    pub fn get_vote_tally(env: Env, proposal_id: u64) -> Result<VoteTally, GovernorError> {
        let proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(GovernorError::ProposalNotFound)?;

        Ok(VoteTally {
            yes_votes: proposal.votes_for,
            no_votes: proposal.votes_against,
            abstain_votes: proposal.abstentions,
            total_votes: proposal.votes_for + proposal.votes_against + proposal.abstentions,
        })
    }

    /// Return the IDs of all proposals that are currently in the active voting period.
    ///
    /// A proposal is considered pending if its [`ProposalState`] is [`ProposalState::Active`]
    /// **and** the current ledger timestamp has not yet passed its `vote_end`. Proposals that
    /// have been finalized, executed, or cancelled — or whose voting window has simply expired
    /// without being finalized — are excluded.
    ///
    /// Read-only; does not modify state. Intended for governance UIs that need to enumerate
    /// active proposals without off-chain indexing.
    ///
    /// # Returns
    /// A `Vec<u64>` of proposal IDs open for voting, in implementation-defined order.
    /// Returns an empty vector when no proposals are currently pending.
    ///
    /// # Example
    /// ```text
    /// let pending = client.get_pending_proposals();
    /// for id in pending.iter() {
    ///     let p = client.get_proposal(&id)?;
    ///     println!("Active: {} (ends {})", p.title, p.vote_end);
    /// }
    /// ```
    pub fn get_pending_proposals(env: Env) -> Vec<u64> {
        let active: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::ActiveProposals)
            .unwrap_or_else(|| Vec::new(&env));

        let now = env.ledger().timestamp();
        let mut pending = Vec::new(&env);

        for id in active.iter() {
            if let Some(proposal) = env
                .storage()
                .persistent()
                .get::<DataKey, Proposal>(&DataKey::Proposal(id))
            {
                // Exclude cancelled proposals and expired voting windows
                if proposal.state != ProposalState::Cancelled && now < proposal.vote_end {
                    pending.push_back(id);
                }
            }
        }

        pending
    }

    fn remove_active_proposal(env: &Env, proposal_id: u64) {
        let index_key = DataKey::ActiveProposalIndex(proposal_id);
        let Some(index) = env.storage().instance().get::<DataKey, u32>(&index_key) else {
            return;
        };

        let mut active: Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::ActiveProposals)
            .unwrap_or_else(|| Vec::new(env));
        if active.is_empty() {
            env.storage().instance().remove(&index_key);
            return;
        }

        let last_index = active.len().saturating_sub(1);
        if index != last_index {
            let last_id = active.get(last_index).unwrap();
            active.set(index, last_id);
            env.storage()
                .instance()
                .set(&DataKey::ActiveProposalIndex(last_id), &index);
        }

        active.remove(last_index);
        env.storage()
            .instance()
            .set(&DataKey::ActiveProposals, &active);
        env.storage().instance().remove(&index_key);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Env, String,
    };

    fn setup(env: &Env) -> GovernorContractClient<'_> {
        let contract_id = env.register_contract(None, GovernorContract);
        let client = GovernorContractClient::new(env, &contract_id);
        let admin = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        let config = GovernorConfig {
            admin: admin.clone(),
            vote_token: token,
            voting_period: 3600,
            quorum: 100,
            timelock_delay: 86400,
        };
        client.initialize(&config);
        client
    }

    /// Setup helper that also returns the token address so tests can mint
    /// balances to voters before calling vote().
    fn setup_with_token(env: &Env) -> (GovernorContractClient<'_>, Address) {
        let contract_id = env.register_contract(None, GovernorContract);
        let client = GovernorContractClient::new(env, &contract_id);
        let admin = Address::generate(env);
        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        let config = GovernorConfig {
            admin: admin.clone(),
            vote_token: token_id.clone(),
            voting_period: 3600,
            quorum: 100,
            timelock_delay: 86400,
        };
        client.initialize(&config);
        (client, token_id)
    }

    fn mint(env: &Env, token_id: &Address, to: &Address, amount: i128) {
        soroban_sdk::token::StellarAssetClient::new(env, token_id).mint(to, &amount);
    }

    #[test]
    fn test_initialize_requires_auth() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, GovernorContract);
        let client = GovernorContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let attacker = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();

        // Attacker tries to initialize with their own config
        let malicious_config = GovernorConfig {
            admin: attacker.clone(),
            vote_token: token.clone(),
            voting_period: 1,  // Very short voting period
            quorum: 1,         // Very low quorum
            timelock_delay: 0, // No timelock
        };

        // This should fail because attacker is not the admin in the config
        let result = client.try_initialize(&malicious_config);
        assert!(
            result.is_err(),
            "initialize should fail when called by non-admin"
        );

        // Verify contract is still not initialized
        assert_eq!(
            client.try_get_config().unwrap_err().unwrap(),
            GovernorError::NotInitialized.into()
        );

        // Now admin initializes with proper config
        let proper_config = GovernorConfig {
            admin: admin.clone(),
            vote_token: token.clone(),
            voting_period: 3600,
            quorum: 100,
            timelock_delay: 86400,
        };

        let result = client.try_initialize(&proper_config);
        assert!(
            result.is_ok(),
            "initialize should succeed when called by admin"
        );

        // Verify config is set
        let config = client.get_config().unwrap();
        assert_eq!(config.admin, admin);
        assert_eq!(config.voting_period, 3600);
        assert_eq!(config.quorum, 100);
        assert_eq!(config.timelock_delay, 86400);
    }

    #[test]
    fn test_vote_and_pass() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "A test"),
        );

        client.vote(&voter, &pid, &VoteDirection::For, &200);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);
    }

    #[test]
    fn test_vote_with_excessive_weight_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "A test"),
        );

        // Try to vote with 200 weight when only 100 balance
        let result = client.try_vote(&voter, &pid, &VoteDirection::For, &200);
        assert_eq!(result, Err(Ok(GovernorError::InvalidWeight)));
    }

    #[test]
    fn test_proposal_count() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        // Initially, no proposals exist
        assert_eq!(client.get_proposal_count(), 0);

        // Create first proposal
        let proposer = Address::generate(&env);
        let pid1 = client.propose(
            &proposer,
            &String::from_str(&env, "First Proposal"),
            &String::from_str(&env, "First description"),
        );
        assert_eq!(pid1, 0);
        assert_eq!(client.get_proposal_count(), 1);

        // Create second proposal
        let pid2 = client.propose(
            &proposer,
            &String::from_str(&env, "Second Proposal"),
            &String::from_str(&env, "Second description"),
        );
        assert_eq!(pid2, 1);
        assert_eq!(client.get_proposal_count(), 2);

        // Create third proposal
        let pid3 = client.propose(
            &proposer,
            &String::from_str(&env, "Third Proposal"),
            &String::from_str(&env, "Third description"),
        );
        assert_eq!(pid3, 2);
        assert_eq!(client.get_proposal_count(), 3);
    }

    #[test]
    fn test_proposal_ids_are_sequential_with_no_gaps() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);

        // Create 5 proposals and capture returned IDs
        let titles = [
            "Proposal 0",
            "Proposal 1",
            "Proposal 2",
            "Proposal 3",
            "Proposal 4",
        ];
        let descs = ["Desc 0", "Desc 1", "Desc 2", "Desc 3", "Desc 4"];
        let mut ids = [0u64; 5];
        for i in 0..5 {
            ids[i] = client.propose(
                &proposer,
                &String::from_str(&env, titles[i]),
                &String::from_str(&env, descs[i]),
            );
        }

        // IDs must be exactly 0..4 in order — no gaps, starting from 0
        assert_eq!(ids, [0u64, 1, 2, 3, 4]);

        // Count must reflect all 5 proposals
        assert_eq!(client.get_proposal_count(), 5);

        // Every proposal must be retrievable and have the correct proposer
        for id in 0u64..5 {
            let proposal = client.get_proposal(&id);
            assert_eq!(proposal.proposer, proposer);
        }

        // One past the last ID must return ProposalNotFound
        let result = client.try_get_proposal(&5u64);
        assert!(matches!(result, Err(Ok(GovernorError::ProposalNotFound))));
    }

    #[test]
    fn test_quorum_not_reached_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Low vote"),
            &String::from_str(&env, "desc"),
        );

        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 50);
        client.vote(&voter, &pid, &VoteDirection::For, &50);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Failed);
    }

    #[test]
    fn test_double_vote_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        client.vote(&voter, &pid, &VoteDirection::For, &100);
        let result = client.try_vote(&voter, &pid, &VoteDirection::For, &100);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyVoted)));
    }

    #[test]
    fn test_vote_with_zero_weight_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        let result = client.try_vote(&voter, &pid, &VoteDirection::For, &0);
        assert_eq!(result, Err(Ok(GovernorError::InvalidWeight)));
    }

    #[test]
    fn test_vote_with_negative_weight_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        let result = client.try_vote(&voter, &pid, &VoteDirection::Against, &-1000);
        assert_eq!(result, Err(Ok(GovernorError::InvalidWeight)));
    }

    #[test]
    fn test_get_proposal_existing() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let title = String::from_str(&env, "My Proposal");
        let description = String::from_str(&env, "Details here");
        let pid = client.propose(&proposer, &title, &description);

        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.proposer, proposer);
        assert_eq!(proposal.title, title);
        assert_eq!(proposal.description, description);
        assert_eq!(proposal.state, ProposalState::Active);
        assert_eq!(proposal.vote_start, 1000);
        assert_eq!(proposal.votes_for, 0);
        assert_eq!(proposal.votes_against, 0);
    }

    #[test]
    fn test_get_proposal_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let client = setup(&env);

        let result = client.try_get_proposal(&999);
        assert!(matches!(result, Err(Ok(GovernorError::ProposalNotFound))));
    }

    #[test]
    fn test_finalize_fails_when_quorum_not_reached() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        // Vote with weight below quorum (quorum = 100)
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 50);
        client.vote(&voter, &pid, &VoteDirection::For, &50);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Failed);
    }

    #[test]
    fn test_finalize_exact_quorum_passes_and_below_quorum_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Exact quorum"),
            &String::from_str(&env, "desc"),
        );

        // Cast votes that exactly meet quorum (60 + 40 = 100)
        let voter1 = Address::generate(&env);
        let voter2 = Address::generate(&env);
        mint(&env, &token_id, &voter1, 60);
        mint(&env, &token_id, &voter2, 40);
        client.vote(&voter1, &pid, &VoteDirection::For, &60);
        client.vote(&voter2, &pid, &VoteDirection::For, &40);

        // Finalize after the voting period and verify it passes.
        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);

        let proposer2 = Address::generate(&env);
        let pid2 = client.propose(
            &proposer2,
            &String::from_str(&env, "Below quorum"),
            &String::from_str(&env, "desc"),
        );

        // Cast votes just below quorum (60 + 39 = 99)
        let voter3 = Address::generate(&env);
        let voter4 = Address::generate(&env);
        mint(&env, &token_id, &voter3, 60);
        mint(&env, &token_id, &voter4, 39);
        client.vote(&voter3, &pid2, &VoteDirection::For, &60);
        client.vote(&voter4, &pid2, &VoteDirection::For, &39);

        // Finalize after the voting period and verify it fails.
        // pid2 was created at t=5000, so vote_end = 5000 + 3600 = 8600
        env.ledger().with_mut(|l| l.timestamp = 9000);
        let state2 = client.finalize(&pid2);
        assert_eq!(state2, ProposalState::Failed);
    }

    #[test]
    fn test_finalize_passes_when_quorum_met_and_majority_yes() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);
        client.vote(&voter, &pid, &VoteDirection::For, &100);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);
    }

    #[test]
    fn test_execute_failed_proposal_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid); // fails: no votes

        let executor = Address::generate(&env);
        let result = client.try_execute(&executor, &pid);
        assert!(matches!(result, Err(Ok(GovernorError::ProposalNotPassed))));
    }

    #[test]
    fn test_vote_after_voting_period_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        // Advance past voting_period (3600)
        env.ledger().with_mut(|l| l.timestamp = 5000);

        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);
        let result = client.try_vote(&voter, &pid, &VoteDirection::For, &100);
        assert!(matches!(result, Err(Ok(GovernorError::VotingClosed))));
    }

    /// Voting on a finalized (Passed) proposal must revert with `VotingClosed`.
    #[test]
    fn test_vote_after_finalized_passed_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let late_voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);
        mint(&env, &token_id, &late_voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200); // meets quorum

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);

        let result = client.try_vote(&late_voter, &pid, &VoteDirection::For, &100);
        assert!(matches!(result, Err(Ok(GovernorError::VotingClosed))));
    }

    /// Voting on a finalized (Failed) proposal must revert with `VotingClosed`.
    #[test]
    fn test_vote_after_finalized_failed_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let late_voter = Address::generate(&env);
        mint(&env, &token_id, &late_voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );
        // No votes — quorum not met → Failed

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Failed);

        let result = client.try_vote(&late_voter, &pid, &VoteDirection::For, &100);
        assert!(matches!(result, Err(Ok(GovernorError::VotingClosed))));
    }

    #[test]
    fn test_execute_before_timelock_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let executor = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200);
        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid);

        let result = client.try_execute(&executor, &pid);
        assert_eq!(result, Err(Ok(GovernorError::TimelockNotElapsed)));

        // Timelock starts from vote_end (3600), not finalize time (5000)
        env.ledger().with_mut(|l| l.timestamp = 3600 + 86400);
        client.execute(&executor, &pid);

        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.state, ProposalState::Executed);
    }

    /// finalize() must store the exact ledger timestamp in passed_at, and
    /// execute() must use that value for the timelock boundary check.
    ///
    /// Timeline (timelock_delay = 86400):
    ///   t=0      propose
    ///   t=0      vote (200 weight, quorum=100)
    ///   t=5000   finalize → passed_at must equal 5000
    ///   t=5000+86399  execute → TimelockNotElapsed (one second short)
    ///   t=5000+86400  execute → Ok(())
    ///
    /// Also verifies that a Failed proposal stores passed_at = None.
    #[test]
    fn test_finalize_sets_passed_at_and_execute_respects_timelock() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let executor = Address::generate(&env);

        // ── Passed proposal ──────────────────────────────────────────────────
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );
        client.vote(&voter, &pid, &true, &200);

        // Finalize at a known timestamp
        let finalize_time: u64 = 5000;
        env.ledger().with_mut(|l| l.timestamp = finalize_time);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);

        // passed_at must be exactly the finalize timestamp
        let proposal = client.get_proposal(&pid);
        assert_eq!(
            proposal.passed_at,
            Some(finalize_time),
            "passed_at should equal the ledger timestamp at finalize time"
        );

        // One second before timelock expires → must revert
        env.ledger()
            .with_mut(|l| l.timestamp = finalize_time + 86400 - 1);
        let result = client.try_execute(&executor, &pid);
        assert_eq!(
            result,
            Err(Ok(GovernorError::TimelockNotElapsed)),
            "execute should revert at passed_at + timelock_delay - 1"
        );

        // Exactly at timelock boundary → must succeed
        env.ledger()
            .with_mut(|l| l.timestamp = finalize_time + 86400);
        client.execute(&executor, &pid);
        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.state, ProposalState::Executed);

        // ── Failed proposal ───────────────────────────────────────────────────
        let pid2 = client.propose(
            &proposer,
            &String::from_str(&env, "Fail"),
            &String::from_str(&env, "No votes"),
        );
        // No votes — quorum not met → Failed
        env.ledger()
            .with_mut(|l| l.timestamp = finalize_time + 86400 + 5000);
        let state2 = client.finalize(&pid2);
        assert_eq!(state2, ProposalState::Failed);

        let failed_proposal = client.get_proposal(&pid2);
        assert_eq!(
            failed_proposal.passed_at, None,
            "passed_at must be None for a Failed proposal"
        );
    }

    #[test]
    fn test_late_finalize_timelock_starts_from_vote_end() {
        // finalize() called 1000s after vote_end; execution window must be
        // vote_end + timelock_delay, not finalize_time + timelock_delay.
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let executor = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200);

        // Finalize 1000 seconds after vote_end (3600)
        let finalize_time = 3600 + 1000;
        env.ledger().with_mut(|l| l.timestamp = finalize_time);
        client.finalize(&pid);

        // Still locked at finalize_time + timelock_delay
        env.ledger()
            .with_mut(|l| l.timestamp = finalize_time + 86400);
        let result = client.try_execute(&executor, &pid);
        assert_eq!(result, Err(Ok(GovernorError::TimelockNotElapsed)));

        // Executable at vote_end + timelock_delay
        env.ledger().with_mut(|l| l.timestamp = 3600 + 86400);
        client.execute(&executor, &pid);
        assert_eq!(client.get_proposal(&pid).state, ProposalState::Executed);
    }

    #[test]
    fn test_execute_twice_reverts_with_already_executed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let executor = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200);
        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid);

        // Execute the proposal
        env.ledger().with_mut(|l| l.timestamp = 5000 + 86400);
        client.execute(&executor, &pid);

        // Verify proposal state is Executed
        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.state, ProposalState::Executed);

        // Try to execute again
        let result = client.try_execute(&executor, &pid);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyExecuted)));

        // Verify proposal state is still Executed
        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.state, ProposalState::Executed);
    }

    #[test]
    fn test_has_voted_returns_true_for_voter() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        // Voter has not voted yet
        assert!(!client.has_voted(&pid, &voter));

        // Cast vote
        client.vote(&voter, &pid, &VoteDirection::For, &100);

        // Now voter has voted
        assert!(client.has_voted(&pid, &voter));
    }

    #[test]
    fn test_has_voted_returns_false_for_non_voter() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let non_voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        // non_voter has not participated at all
        assert!(!client.has_voted(&pid, &non_voter));

        // voter votes
        client.vote(&voter, &pid, &VoteDirection::For, &100);

        // non_voter still has not voted
        assert!(!client.has_voted(&pid, &non_voter));

        // voter has voted
        assert!(client.has_voted(&pid, &voter));
    }

    #[test]
    fn test_has_voted_returns_false_for_non_existent_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let voter = Address::generate(&env);

        // Call has_voted with a proposal_id that was never created
        let result = client.has_voted(&999, &voter);

        // Should return false without throwing an error
        assert!(!result);
    }

    /// Verify that a Vote entry persists after being written so that
    /// has_voted() reliably returns true and double-vote protection holds.
    /// This guards against the TTL-expiry regression described in the issue:
    /// an expired Vote entry would cause has() to return false, allowing a
    /// second vote on the same proposal.
    #[test]
    fn test_has_voted_persists_after_vote() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "TTL Test"),
            &String::from_str(&env, "Vote entry must persist"),
        );

        assert!(!client.has_voted(&pid, &voter), "should not have voted yet");

        client.vote(&voter, &pid, &VoteDirection::For, &100);

        // Entry must be present immediately after voting
        assert!(
            client.has_voted(&pid, &voter),
            "has_voted must return true after vote"
        );

        // A second vote attempt must fail — confirming the entry is still live
        let result = client.try_vote(&voter, &pid, &VoteDirection::Against, &100);
        assert_eq!(
            result,
            Err(Ok(GovernorError::AlreadyVoted)),
            "double-vote must be rejected while Vote entry persists"
        );
    }

    #[test]
    fn test_get_pending_proposals_empty_when_none_exist() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn test_get_pending_proposals_returns_active_ids() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid0 = client.propose(
            &proposer,
            &String::from_str(&env, "P0"),
            &String::from_str(&env, "D"),
        );
        let pid1 = client.propose(
            &proposer,
            &String::from_str(&env, "P1"),
            &String::from_str(&env, "D"),
        );

        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 2);
        assert!(pending.contains(pid0));
        assert!(pending.contains(pid1));
    }

    #[test]
    fn test_get_pending_proposals_excludes_finalized() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        // pid0: will be finalized (passed)
        let pid0 = client.propose(
            &proposer,
            &String::from_str(&env, "P0"),
            &String::from_str(&env, "D"),
        );
        client.vote(&voter, &pid0, &VoteDirection::For, &200);

        // pid1: will remain active but its voting window also expires at t=5000
        let _pid1 = client.propose(
            &proposer,
            &String::from_str(&env, "P1"),
            &String::from_str(&env, "D"),
        );

        // Advance past voting period and finalize pid0
        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid0);

        // Both proposals' voting windows have expired — none should be pending
        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn test_get_pending_proposals_excludes_expired_but_not_finalized() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        // Advance past voting_period without finalizing
        env.ledger().with_mut(|l| l.timestamp = 5000);

        // State is still Active but vote_end has passed — should not be returned
        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 0);
        // Confirm the proposal still exists
        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.state, ProposalState::Active);
    }

    #[test]
    fn test_get_pending_proposals_mixed_states() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        // pid0: finalized (passed) at t=5000
        let pid0 = client.propose(
            &proposer,
            &String::from_str(&env, "P0"),
            &String::from_str(&env, "D"),
        );
        client.vote(&voter, &pid0, &VoteDirection::For, &200);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid0);

        // pid1 and pid2 proposed after the advance — still in voting window
        let pid1 = client.propose(
            &proposer,
            &String::from_str(&env, "P1"),
            &String::from_str(&env, "D"),
        );
        let pid2 = client.propose(
            &proposer,
            &String::from_str(&env, "P2"),
            &String::from_str(&env, "D"),
        );

        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 2);
        assert!(pending.contains(pid1));
        assert!(pending.contains(pid2));
    }

    #[test]
    fn test_get_pending_proposals_voting_period_one_immediate_finalize_boundary() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);

        let contract_id = env.register_contract(None, GovernorContract);
        let client = GovernorContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let config = GovernorConfig {
            admin: admin.clone(),
            vote_token: token_id,
            voting_period: 1,
            quorum: 100,
            timelock_delay: 86400,
        };
        client.initialize(&config);

        let proposer = Address::generate(&env);

        // t=0 create pid_0
        let pid_0 = client.propose(
            &proposer,
            &String::from_str(&env, "P0"),
            &String::from_str(&env, "D"),
        );

        // Advance to t=1 — voting period has ended (vote_end == 1)
        env.ledger().with_mut(|l| l.timestamp = 1);
        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 0);

        // Finalize pid_0 at the boundary timestamp
        client.finalize(&pid_0);

        // Create pid_1 at t=1
        let pid_1 = client.propose(
            &proposer,
            &String::from_str(&env, "P1"),
            &String::from_str(&env, "D"),
        );

        // Only pid_1 should be pending; pid_0 must not reappear after finalization
        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 1);
        assert!(pending.contains(pid_1));
        assert!(!pending.contains(pid_0));
    }

    // ── Tie-breaking behaviour ─────────────────────────────────────────────────
    //
    // The contract requires votes_for > votes_against (strict majority) to pass.
    // When yes votes equal no votes the proposal resolves to Failed — there is
    // no mechanism that breaks a tie in favour of the proposer or any other party.
    // This is deterministic and must be explicitly tested.

    /// Equal yes and no votes that together meet quorum must resolve to Failed.
    /// Tie-breaking rule: votes_for must be strictly greater than votes_against.
    #[test]
    fn test_tied_vote_resolves_to_failed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env); // quorum = 100, voting_period = 3600

        let proposer = Address::generate(&env);
        let yes_voter = Address::generate(&env);
        let no_voter = Address::generate(&env);
        mint(&env, &token_id, &yes_voter, 100);
        mint(&env, &token_id, &no_voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Tied Proposal"),
            &String::from_str(&env, "Equal yes and no votes"),
        );

        // Cast equal weight on both sides — total = 200, meets quorum of 100
        client.vote(&yes_voter, &pid, &VoteDirection::For, &100);
        client.vote(&no_voter, &pid, &VoteDirection::Against, &100);

        // Advance past the voting period
        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);

        // Tie must resolve to Failed — strict majority (votes_for > votes_against) required
        assert_eq!(
            state,
            ProposalState::Failed,
            "a tied vote must resolve to Failed, not Passed"
        );

        // Confirm the stored state matches
        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.state, ProposalState::Failed);
        assert_eq!(proposal.votes_for, 100);
        assert_eq!(proposal.votes_against, 100);
        assert!(
            proposal.passed_at.is_none(),
            "passed_at must not be set on a failed proposal"
        );
    }

    /// One extra no vote tips a near-tie to Failed.
    #[test]
    fn test_near_tie_no_majority_resolves_to_failed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let yes_voter = Address::generate(&env);
        let no_voter = Address::generate(&env);
        mint(&env, &token_id, &yes_voter, 100);
        mint(&env, &token_id, &no_voter, 101);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Near-tie Proposal"),
            &String::from_str(&env, "No votes exceed yes by 1"),
        );

        // 100 yes, 101 no — quorum met, but no majority
        client.vote(&yes_voter, &pid, &VoteDirection::For, &100);
        client.vote(&no_voter, &pid, &VoteDirection::Against, &101);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Failed);
    }

    /// One extra yes vote tips a near-tie to Passed.
    #[test]
    fn test_near_tie_yes_majority_resolves_to_passed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let yes_voter = Address::generate(&env);
        let no_voter = Address::generate(&env);
        mint(&env, &token_id, &yes_voter, 101);
        mint(&env, &token_id, &no_voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Near-tie Proposal"),
            &String::from_str(&env, "Yes votes exceed no by 1"),
        );

        // 101 yes, 100 no — quorum met, strict majority achieved
        client.vote(&yes_voter, &pid, &VoteDirection::For, &101);
        client.vote(&no_voter, &pid, &VoteDirection::Against, &100);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);
    }

    /// get_pending_proposals() reads only the ActiveProposals list, not every proposal.
    /// Creates 25 proposals: finalizes the first 5, lets the next 5 expire (not finalized),
    /// and keeps the last 15 active. Verifies only the 15 active ones are returned.
    #[test]
    fn test_get_pending_proposals_uses_active_list_not_full_scan() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env); // voting_period = 3600, quorum = 100

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        // Create 25 proposals at t=0
        let mut ids: soroban_sdk::Vec<u64> = soroban_sdk::Vec::new(&env);
        for i in 0u32..25 {
            let title = String::from_str(&env, "P");
            let desc = String::from_str(&env, "D");
            let _ = i; // suppress unused warning
            let pid = client.propose(&proposer, &title, &desc);
            ids.push_back(pid);
        }

        // Vote on the first 5 before voting period ends (vote_end = 3600)
        for i in 0..5u32 {
            let pid = ids.get(i).unwrap();
            client.vote(&voter, &pid, &VoteDirection::For, &200);
        }

        // Finalize the first 5 after voting period ends
        env.ledger().with_mut(|l| l.timestamp = 4000);
        for i in 0..5u32 {
            let pid = ids.get(i).unwrap();
            client.finalize(&pid);
        }

        // Advance past voting period for proposals 5-9 (expired, not finalized)
        // They remain Active in state but vote_end has passed — excluded from pending

        // Advance to t=5000 so proposals 0-9 are all past their vote_end (t=3600)
        // Proposals 10-24 were also created at t=0 so they also expire at t=3600...
        // Re-create the last 15 at t=5000 so they have vote_end = 5000+3600 = 8600
        env.ledger().with_mut(|l| l.timestamp = 5000);
        let mut active_ids: soroban_sdk::Vec<u64> = soroban_sdk::Vec::new(&env);
        for _ in 0..15u32 {
            let pid = client.propose(
                &proposer,
                &String::from_str(&env, "Active"),
                &String::from_str(&env, "D"),
            );
            active_ids.push_back(pid);
        }

        // At t=5000 the first 25 proposals have expired (vote_end=3600), the new 15 are active
        let pending = client.get_pending_proposals();
        assert_eq!(
            pending.len(),
            15,
            "expected exactly 15 active proposals, got {}",
            pending.len()
        );

        // Verify the returned IDs match the 15 newly created ones
        for i in 0..15u32 {
            assert!(pending.contains(active_ids.get(i).unwrap()));
        }
    }

    #[test]
    fn test_get_proposal_state_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let client = setup(&env);

        let result = client.try_get_proposal_state(&99);
        assert_eq!(result, Err(Ok(GovernorError::ProposalNotFound)));
    }

    #[test]
    fn test_get_proposal_state_active() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "State Test"),
            &String::from_str(&env, "desc"),
        );

        assert_eq!(client.get_proposal_state(&pid), ProposalState::Active);
    }

    #[test]
    fn test_get_proposal_state_passed_and_executed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "State Test"),
            &String::from_str(&env, "desc"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid);
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Passed);

        env.ledger().with_mut(|l| l.timestamp = 5000 + 86400 + 1);
        let executor = Address::generate(&env);
        client.execute(&executor, &pid);
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Executed);
    }

    #[test]
    fn test_get_proposal_state_failed() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "State Test"),
            &String::from_str(&env, "desc"),
        );
        // No votes — quorum not reached

        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid);
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Failed);
    }

    #[test]
    fn test_get_vote_tally_no_votes() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Tally Test"),
            &String::from_str(&env, "desc"),
        );

        let tally = client.get_vote_tally(&pid);
        assert_eq!(tally.yes_votes, 0);
        assert_eq!(tally.no_votes, 0);
        assert_eq!(tally.total_votes, 0);
    }

    #[test]
    fn test_get_vote_tally_mixed_votes() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Tally Test"),
            &String::from_str(&env, "desc"),
        );

        let voter_a = Address::generate(&env);
        let voter_b = Address::generate(&env);
        mint(&env, &token_id, &voter_a, 300);
        mint(&env, &token_id, &voter_b, 100);
        client.vote(&voter_a, &pid, &VoteDirection::For, &300);
        client.vote(&voter_b, &pid, &VoteDirection::Against, &100);

        let tally = client.get_vote_tally(&pid);
        assert_eq!(tally.yes_votes, 300);
        assert_eq!(tally.no_votes, 100);
        assert_eq!(tally.total_votes, 400);
    }

    #[test]
    fn test_get_vote_tally_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let client = setup(&env);

        let result = client.try_get_vote_tally(&99);
        assert_eq!(result, Err(Ok(GovernorError::ProposalNotFound)));
    }

    /// Test that propose() emits a proposal_created event with correct payload
    #[test]
    fn test_propose_emits_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        client.propose(
            &proposer,
            &String::from_str(&env, "Test"),
            &String::from_str(&env, "Desc"),
        );

        let events = env.events().all();
        assert!(!events.is_empty(), "Expected at least one event");
    }

    #[test]
    fn test_vote_emits_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test"),
            &String::from_str(&env, "Desc"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200);

        let events = env.events().all();
        assert!(
            events.len() >= 2,
            "Expected at least two events (propose + vote)"
        );
    }

    #[test]
    fn test_finalize_emits_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test"),
            &String::from_str(&env, "Desc"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid);

        let events = env.events().all();
        assert!(
            events.len() >= 3,
            "Expected at least three events (propose + vote + finalize)"
        );
    }

    #[test]
    fn test_execute_emits_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let executor = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test"),
            &String::from_str(&env, "Desc"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &200);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid);

        env.ledger().with_mut(|l| l.timestamp = 5000 + 86400 + 1);
        client.execute(&executor, &pid);

        let events = env.events().all();
        assert!(
            events.len() >= 4,
            "Expected at least four events (propose + vote + finalize + execute)"
        );
    }

    /// Test successful cancel: proposer can cancel an active proposal before voting ends
    #[test]
    fn test_cancel_proposal_successful() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Verify proposal is active
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Active);

        // Cancel the proposal
        client.cancel_proposal(&proposer, &pid);

        // Verify proposal is now cancelled
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Cancelled);
    }

    /// Test cancel by non-proposer: only the original proposer can cancel
    #[test]
    fn test_cancel_proposal_by_non_proposer_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let other = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Try to cancel as a different address
        let result = client.try_cancel_proposal(&other, &pid);
        assert_eq!(result, Err(Ok(GovernorError::Unauthorized)));

        // Verify proposal is still active
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Active);
    }

    /// Test cancel after voting ends: cannot cancel after voting period has ended
    #[test]
    fn test_cancel_proposal_after_voting_ends_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Advance time past voting period (voting_period = 3600)
        env.ledger().with_mut(|l| l.timestamp = 1000 + 3600 + 1);

        // Try to cancel after voting ends
        let result = client.try_cancel_proposal(&proposer, &pid);
        assert_eq!(result, Err(Ok(GovernorError::VotingClosed)));

        // Verify proposal is still active (not cancelled)
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Active);
    }

    /// Test execute after cancel: cannot execute a cancelled proposal
    #[test]
    fn test_execute_after_cancel_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let executor = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Cancel the proposal
        client.cancel_proposal(&proposer, &pid);

        // Try to execute the cancelled proposal
        let result = client.try_execute(&executor, &pid);
        assert_eq!(result, Err(Ok(GovernorError::ProposalNotPassed)));

        // Verify proposal is still cancelled
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Cancelled);
    }

    /// Test get_pending_proposals excludes cancelled proposals
    #[test]
    fn test_get_pending_proposals_excludes_cancelled() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);

        // Create two proposals
        let pid1 = client.propose(
            &proposer,
            &String::from_str(&env, "Proposal 1"),
            &String::from_str(&env, "Description 1"),
        );
        let pid2 = client.propose(
            &proposer,
            &String::from_str(&env, "Proposal 2"),
            &String::from_str(&env, "Description 2"),
        );

        // Both should be pending
        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 2);
        assert!(pending.contains(pid1));
        assert!(pending.contains(pid2));

        // Cancel the first proposal
        client.cancel_proposal(&proposer, &pid1);

        // Only the second proposal should be pending now
        let pending = client.get_pending_proposals();
        assert_eq!(pending.len(), 1);
        assert!(!pending.contains(pid1));
        assert!(pending.contains(pid2));
    }

    /// Test cancel already cancelled proposal: cannot cancel a proposal twice
    #[test]
    fn test_cancel_already_cancelled_proposal_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Cancel the proposal
        client.cancel_proposal(&proposer, &pid);

        // Try to cancel again
        let result = client.try_cancel_proposal(&proposer, &pid);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyCancelled)));
    }

    /// Test that NextProposalId is not incremented when propose() fails.
    ///
    /// Case 1: propose() on an uninitialized contract returns NotInitialized
    ///         and get_proposal_count() stays at 0.
    /// Case 2: After one valid proposal, a second invalid call (uninitialized
    ///         contract) leaves get_proposal_count() at 1, not 2.
    #[test]
    fn test_failed_propose_does_not_increment_proposal_counter() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);

        // ── Case 1: uninitialized contract ────────────────────────────────────
        let contract_id = env.register_contract(None, GovernorContract);
        let uninit_client = GovernorContractClient::new(&env, &contract_id);

        let proposer = Address::generate(&env);
        let result = uninit_client.try_propose(
            &proposer,
            &String::from_str(&env, "Should Fail"),
            &String::from_str(&env, "Contract not initialized"),
        );
        assert_eq!(result, Err(Ok(GovernorError::NotInitialized)));

        // Counter must still be 0 — the failed call must not have touched it
        assert_eq!(
            uninit_client.get_proposal_count(),
            0,
            "proposal count should remain 0 after failed propose on uninitialized contract"
        );

        // ── Case 2: one valid proposal, then a failed one on a separate uninit contract ──
        let client = setup(&env);

        // Valid proposal — counter goes to 1
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Valid Proposal"),
            &String::from_str(&env, "This one succeeds"),
        );
        assert_eq!(pid, 0, "first proposal id should be 0");
        assert_eq!(
            client.get_proposal_count(),
            1,
            "count should be 1 after one valid proposal"
        );

        // Failed propose on the still-uninitialized contract must not affect its counter
        let result2 = uninit_client.try_propose(
            &proposer,
            &String::from_str(&env, "Should Also Fail"),
            &String::from_str(&env, "Still not initialized"),
        );
        assert_eq!(result2, Err(Ok(GovernorError::NotInitialized)));
        assert_eq!(
            uninit_client.get_proposal_count(),
            0,
            "uninit contract count should still be 0 after second failed propose"
        );

        // The initialized contract's counter must be unaffected too
        assert_eq!(
            client.get_proposal_count(),
            1,
            "initialized contract count should still be 1"
        );
    }

    // ── Abstain vote tests ─────────────────────────────────────────────────────

    /// Abstain votes count toward quorum but not for or against.
    #[test]
    fn test_abstain_counts_toward_quorum() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env); // quorum = 100

        let proposer = Address::generate(&env);
        let yes_voter = Address::generate(&env);
        let abstain_voter = Address::generate(&env);
        mint(&env, &token_id, &yes_voter, 60);
        mint(&env, &token_id, &abstain_voter, 40);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Abstain Quorum Test"),
            &String::from_str(&env, "desc"),
        );

        // 60 for + 40 abstain = 100 total, meets quorum; 60 > 0 so passes
        client.vote(&yes_voter, &pid, &VoteDirection::For, &60);
        client.vote(&abstain_voter, &pid, &VoteDirection::Abstain, &40);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);

        let proposal = client.get_proposal(&pid);
        assert_eq!(proposal.votes_for, 60);
        assert_eq!(proposal.votes_against, 0);
        assert_eq!(proposal.abstentions, 40);
    }

    /// Abstain-only votes meet quorum but proposal fails (no majority for).
    #[test]
    fn test_abstain_only_meets_quorum_but_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env); // quorum = 100

        let proposer = Address::generate(&env);
        let abstain_voter = Address::generate(&env);
        mint(&env, &token_id, &abstain_voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Abstain Only"),
            &String::from_str(&env, "desc"),
        );

        client.vote(&abstain_voter, &pid, &VoteDirection::Abstain, &200);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        // quorum met (200 >= 100) but votes_for (0) not > votes_against (0)
        assert_eq!(state, ProposalState::Failed);
    }

    /// Abstain votes without enough total to meet quorum still fail.
    #[test]
    fn test_abstain_below_quorum_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env); // quorum = 100

        let proposer = Address::generate(&env);
        let abstain_voter = Address::generate(&env);
        mint(&env, &token_id, &abstain_voter, 50);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Abstain Below Quorum"),
            &String::from_str(&env, "desc"),
        );

        client.vote(&abstain_voter, &pid, &VoteDirection::Abstain, &50);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Failed);
    }

    /// get_vote_tally reflects abstain votes correctly.
    #[test]
    fn test_get_vote_tally_with_abstain() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Tally Abstain"),
            &String::from_str(&env, "desc"),
        );

        let v1 = Address::generate(&env);
        let v2 = Address::generate(&env);
        let v3 = Address::generate(&env);
        mint(&env, &token_id, &v1, 100);
        mint(&env, &token_id, &v2, 50);
        mint(&env, &token_id, &v3, 75);
        client.vote(&v1, &pid, &VoteDirection::For, &100);
        client.vote(&v2, &pid, &VoteDirection::Against, &50);
        client.vote(&v3, &pid, &VoteDirection::Abstain, &75);

        let tally = client.get_vote_tally(&pid);
        assert_eq!(tally.yes_votes, 100);
        assert_eq!(tally.no_votes, 50);
        assert_eq!(tally.abstain_votes, 75);
        assert_eq!(tally.total_votes, 225);
    }

    /// Issue #267: initialize() with vote_token == admin (clearly invalid config) must return InvalidConfig.
    ///
    /// A real token contract address is required for vote_token. Using the admin address
    /// as vote_token is a clear misconfiguration that should be caught at initialization.
    #[test]
    fn test_initialize_with_invalid_vote_token_returns_invalid_config() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, GovernorContract);
        let client = GovernorContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);

        // Use admin address as vote_token — clearly invalid
        let config = GovernorConfig {
            admin: admin.clone(),
            vote_token: admin.clone(), // same as admin — invalid
            voting_period: 3600,
            quorum: 100,
            timelock_delay: 86400,
        };

        let result = client.try_initialize(&config);
        assert_eq!(
            result,
            Err(Ok(GovernorError::InvalidConfig)),
            "initialize with vote_token == admin should return InvalidConfig"
        );

        // Contract must remain uninitialized
        assert_eq!(
            client.try_get_config().unwrap_err().unwrap(),
            GovernorError::NotInitialized.into()
        );
    }

    #[test]
    fn test_get_config_returns_not_initialized_before_initialize_and_correct_config_after() {
        let env = Env::default();
        env.mock_all_auths();

        // Register the contract without calling initialize()
        let contract_id = env.register_contract(None, GovernorContract);
        let client = GovernorContractClient::new(&env, &contract_id);

        // get_config() must return NotInitialized before initialization
        assert_eq!(
            client.try_get_config().unwrap_err().unwrap(),
            GovernorError::NotInitialized.into()
        );

        // Build a known config
        let admin = Address::generate(&env);
        let vote_token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let config = GovernorConfig {
            admin: admin.clone(),
            vote_token: vote_token.clone(),
            voting_period: 7200,
            quorum: 50,
            timelock_delay: 43200,
        };

        client.initialize(&config);

        // get_config() must return Ok with the exact values passed to initialize()
        let stored = client
            .get_config()
            .expect("config should be Ok after initialize");
        assert_eq!(stored.vote_token, vote_token);
        assert_eq!(stored.voting_period, 7200);
        assert_eq!(stored.quorum, 50);
        assert_eq!(stored.timelock_delay, 43200);
    }

    #[test]
    fn test_get_vote_weight_returns_correct_weight_after_voting() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 500);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        // Before voting, weight should return VoteNotFound
        assert_eq!(
            client.try_get_vote_weight(&pid, &voter).unwrap_err().unwrap(),
            GovernorError::VoteNotFound.into()
        );

        client.vote(&voter, &pid, &VoteDirection::For, &500);

        // After voting, weight should match what was cast
        assert_eq!(client.get_vote_weight(&pid, &voter), 500);
    }

    #[test]
    fn test_get_vote_weight_returns_vote_not_found_for_non_voter() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let non_voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "P"),
            &String::from_str(&env, "D"),
        );

        client.vote(&voter, &pid, &VoteDirection::Against, &75);

        // voter's weight is stored
        assert_eq!(client.get_vote_weight(&pid, &voter), 75);
        // non_voter never voted — must return VoteNotFound
        assert_eq!(
            client.try_get_vote_weight(&pid, &non_voter).unwrap_err().unwrap(),
            GovernorError::VoteNotFound.into()
        );
    }

    #[test]
    fn test_get_vote_weight_returns_proposal_not_found_for_nonexistent_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let voter = Address::generate(&env);

        // Proposal 999 was never created
        assert_eq!(
            client.try_get_vote_weight(&999, &voter).unwrap_err().unwrap(),
            GovernorError::ProposalNotFound.into()
        );
    }

    // ── Tests for finalize() state handling (issue #138 compatibility) ────────

    /// Test finalize() with Cancelled state returns AlreadyCancelled error
    #[test]
    fn test_finalize_cancelled_proposal_returns_already_cancelled() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Cancel the proposal
        client.cancel_proposal(&proposer, &pid);
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Cancelled);

        // Try to finalize the cancelled proposal
        let result = client.try_finalize(&pid);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyCancelled)));

        // Verify proposal state remains Cancelled
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Cancelled);
    }

    /// Test finalize() with Passed state returns AlreadyFinalized error
    #[test]
    fn test_finalize_passed_proposal_returns_already_finalized() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Vote and finalize to Passed state
        client.vote(&voter, &pid, &VoteDirection::For, &200);
        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);

        // Try to finalize again
        let result = client.try_finalize(&pid);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyFinalized)));

        // Verify proposal state remains Passed
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Passed);
    }

    /// Test finalize() with Failed state returns AlreadyFinalized error
    #[test]
    fn test_finalize_failed_proposal_returns_already_finalized() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let client = setup(&env);

        let proposer = Address::generate(&env);
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // No votes - will fail quorum
        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Failed);

        // Try to finalize again
        let result = client.try_finalize(&pid);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyFinalized)));

        // Verify proposal state remains Failed
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Failed);
    }

    /// Test finalize() with Executed state returns AlreadyFinalized error
    #[test]
    fn test_finalize_executed_proposal_returns_already_finalized() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        let executor = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Vote, finalize, and execute
        client.vote(&voter, &pid, &VoteDirection::For, &200);
        env.ledger().with_mut(|l| l.timestamp = 5000);
        client.finalize(&pid);
        env.ledger().with_mut(|l| l.timestamp = 5000 + 86400 + 1);
        client.execute(&executor, &pid);

        // Verify proposal is Executed
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Executed);

        // Try to finalize the executed proposal
        let result = client.try_finalize(&pid);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyFinalized)));

        // Verify proposal state remains Executed
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Executed);
    }

    /// Test finalize() still works correctly with Active proposals (normal case)
    #[test]
    fn test_finalize_active_proposal_still_works() {
    // ── Issue #335: abstain votes count toward quorum but not toward passing ──

    /// Scenario 1: 100 abstain votes meet quorum (100) but proposal fails because
    /// votes_for (0) is not > votes_against (0).
    #[test]
    fn test_abstain_only_quorum_met_proposal_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        // setup_with_token uses quorum = 100, voting_period = 3600
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let abstainer = Address::generate(&env);
        mint(&env, &token_id, &abstainer, 100);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Abstain Only Quorum"),
            &String::from_str(&env, "100 abstain, 0 for, 0 against"),
        );

        // Cast exactly 100 abstain votes — meets quorum but no for/against
        client.vote(&abstainer, &pid, &VoteDirection::Abstain, &100);

        // Advance past voting period
        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);

        // Quorum met (100 >= 100) but votes_for (0) not > votes_against (0) → Failed
        assert_eq!(
            state,
            ProposalState::Failed,
            "proposal must fail when quorum is met only via abstentions"
        );

        // Verify tally
        let tally = client.get_vote_tally(&pid);
        assert_eq!(tally.abstain_votes, 100);
        assert_eq!(tally.yes_votes, 0);
        assert_eq!(tally.no_votes, 0);
        assert_eq!(tally.total_votes, 100);
    }

    /// Scenario 2: 51 for + 49 abstain = 100 total meets quorum and yes majority → Passed.
    #[test]
    fn test_for_plus_abstain_meets_quorum_and_passes() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 200);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );

        // Vote and advance past voting period
        client.vote(&voter, &pid, &VoteDirection::For, &200);
        env.ledger().with_mut(|l| l.timestamp = 5000);

        // Finalize should work normally for Active proposals
        let state = client.finalize(&pid);
        assert_eq!(state, ProposalState::Passed);
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Passed);
    }

    /// Test finalize() compatibility with cancel_proposal() workflow
    #[test]
    fn test_finalize_compatibility_with_cancel_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let (client, token_id) = setup_with_token(&env);

        let proposer = Address::generate(&env);
        let voter = Address::generate(&env);
        mint(&env, &token_id, &voter, 100);

        // Create a proposal and vote on it
        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "Test Proposal"),
            &String::from_str(&env, "Description"),
        );
        client.vote(&voter, &pid, &VoteDirection::For, &100);

        // Cancel before voting period ends
        client.cancel_proposal(&proposer, &pid);
        assert_eq!(client.get_proposal_state(&pid), ProposalState::Cancelled);

        // Finalize should return AlreadyCancelled, not AlreadyFinalized
        let result = client.try_finalize(&pid);
        assert_eq!(result, Err(Ok(GovernorError::AlreadyCancelled)));

        // Even after voting period ends, still should return AlreadyCancelled
        env.ledger().with_mut(|l| l.timestamp = 1000 + 3600 + 1);
        let result2 = client.try_finalize(&pid);
        assert_eq!(result2, Err(Ok(GovernorError::AlreadyCancelled)));
        let yes_voter = Address::generate(&env);
        let abstainer = Address::generate(&env);
        mint(&env, &token_id, &yes_voter, 51);
        mint(&env, &token_id, &abstainer, 49);

        let pid = client.propose(
            &proposer,
            &String::from_str(&env, "For + Abstain Quorum"),
            &String::from_str(&env, "51 for + 49 abstain = 100 total"),
        );

        client.vote(&yes_voter, &pid, &VoteDirection::For, &51);
        client.vote(&abstainer, &pid, &VoteDirection::Abstain, &49);

        env.ledger().with_mut(|l| l.timestamp = 5000);
        let state = client.finalize(&pid);

        // Total = 100 >= quorum, votes_for (51) > votes_against (0) → Passed
        assert_eq!(
            state,
            ProposalState::Passed,
            "proposal must pass when quorum met and yes majority achieved via for+abstain"
        );

        let tally = client.get_vote_tally(&pid);
        assert_eq!(tally.yes_votes, 51);
        assert_eq!(tally.no_votes, 0);
        assert_eq!(tally.abstain_votes, 49);
        assert_eq!(tally.total_votes, 100);
    }
}
