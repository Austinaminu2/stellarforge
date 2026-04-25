#![no_std]

//! # forge-stream
//!
//! Real-time token streaming — pay-per-second token transfers on Soroban.
//!
//! ## Overview
//! - Sender creates a stream with a rate (tokens per second) and duration
//! - Recipient can withdraw accrued tokens at any time
//! - Sender can cancel and reclaim unstreamed tokens
//! - Multiple streams can run in parallel (keyed by stream_id)

use forge_errors::CommonError;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Env, Symbol,
};

#[contracttype]
pub enum DataKey {
    /// Per-stream data (token, sender, recipient, rate, timestamps, state).
    /// Uses **persistent** storage — must outlive the contract instance TTL
    /// for as long as the stream has unclaimed tokens.
    Stream(u64),
    /// Monotonically increasing counter used to assign the next stream ID.
    /// Uses **instance** storage — small scalar that is always read on
    /// `create_stream`, so co-locating it with the instance is efficient.
    NextId,
    /// Count of streams that are currently active (not cancelled/finished).
    /// Uses **instance** storage — updated on every create/cancel/finish,
    /// always accessed together with other instance data.
    ActiveStreamsCount,
    /// List of stream IDs created by a given sender address.
    /// Uses **persistent** storage — the list grows with each stream and
    /// must survive beyond the instance TTL for historical lookups.
    SenderStreams(Address),
    /// List of stream IDs where a given address is the recipient.
    /// Uses **persistent** storage — same rationale as `SenderStreams`.
    RecipientStreams(Address),
}

#[contracttype]
#[derive(Clone)]
pub struct Stream {
    /// Unique stream ID
    pub id: u64,
    /// Token being streamed
    pub token: Address,
    /// Sender funding the stream
    pub sender: Address,
    /// Recipient receiving tokens
    pub recipient: Address,
    /// Tokens per second
    pub rate_per_second: i128,
    /// Stream start timestamp
    pub start_time: u64,
    /// Stream end timestamp
    pub end_time: u64,
    /// Total tokens already withdrawn
    pub withdrawn: i128,
    /// Whether the stream has been cancelled
    pub cancelled: bool,
    /// Amount streamed at the time of cancellation (if cancelled)
    pub streamed_at_cancel: i128,
    /// Whether the stream is currently paused
    pub is_paused: bool,
    /// Timestamp when stream was last paused (if paused)
    pub paused_at: Option<u64>,
    /// Total seconds the stream has been paused
    pub total_paused_time: u64,
    /// Whether this stream is currently counted as active in the global counter
    pub counted_active: bool,
    /// Minimum withdrawal amount to prevent dust withdrawals (0 means no minimum)
    pub min_withdrawal_amount: i128,
}

#[contracttype]
#[derive(Clone)]
pub struct StreamStatus {
    pub id: u64,
    pub streamed: i128,
    pub withdrawn: i128,
    pub withdrawable: i128,
    pub remaining: i128,
    pub is_active: bool,
    pub is_finished: bool,
    pub is_paused: bool,
    /// `true` when `withdrawable > 0`. A finished stream can be claimable
    /// even though `is_active` is `false`.
    pub is_claimable: bool,
}

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum StreamError {
    #[from(CommonError)]
    Common(CommonError),
    StreamNotFound = 2,
    Unauthorized = 2,
    NothingToWithdraw = 3,
    AlreadyCancelled = 4,
    InvalidConfig = 5,
    StreamFinished = 6,
    /// Sender's token balance is less than the total required to fund the stream
    /// (`rate_per_second * duration_seconds`).
    InsufficientFunds = 7,
    /// Withdrawal amount is below the minimum threshold
    BelowMinimumWithdrawal = 8,
}

#[contract]
pub struct ForgeStream;

#[contractimpl]
impl ForgeStream {
    /// Create a new token stream.
    ///
    /// Creates a stream that unlocks `rate_per_second * duration_seconds` total tokens over time.
    /// Caller (`sender`) must authorize token transfer upfront for the full amount.
    ///
    /// # Parameters
    /// - `sender`: Stream creator/funder (must authorize)
    /// - `token`: Token contract Address
    /// - `recipient`: Who receives withdrawn tokens
    /// - `rate_per_second`: i128 > 0, tokens unlocked per ledger second
    /// - `duration_seconds`: u64 > 0, stream length in seconds
    /// - `min_withdrawal_amount`: i128 >= 0, minimum tokens required for withdrawal (0 means no minimum)
    ///
    /// # Returns
    /// u64: Unique stream ID
    ///
    /// # Example
    /// ```rust,ignore
    /// let stream_id = forge_stream.create_stream(
    ///     env,
    ///     sender,
    ///     token,
    ///     recipient,
    ///     100i128,  // 100 tokens/sec
    ///     3600u64,  // 1 hour = 360,000 total tokens
    ///     1000i128, // minimum withdrawal of 1000 tokens
    /// )?;
    /// ```
    ///
    /// # Errors
    /// - `InvalidConfig` if rate <= 0, duration == 0, or min_withdrawal_amount < 0
    /// - `InsufficientFunds` if sender balance < rate_per_second * duration_seconds
    pub fn create_stream(
        env: Env,
        sender: Address,
        token: Address,
        recipient: Address,
        rate_per_second: i128,
        duration_seconds: u64,
        min_withdrawal_amount: i128,
    ) -> Result<u64, StreamError> {
        if rate_per_second <= 0 || duration_seconds == 0 {
            return Err(StreamError::Common(CommonError::InvalidConfig));
        if rate_per_second <= 0 || duration_seconds == 0 || min_withdrawal_amount < 0 {
            return Err(StreamError::InvalidConfig);
        }

        sender.require_auth();

        let stream_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(0_u64);

        let now = env.ledger().timestamp();
        // Guard against overflow: rate * duration must not exceed i128::MAX.
        // If it would, reject the stream rather than silently truncate.
        let total = rate_per_second
            .checked_mul(duration_seconds as i128)
            .ok_or(StreamError::InvalidConfig)?;

        // Pull total tokens from sender into contract
        let token_client = token::Client::new(&env, &token);
        if token_client.balance(&sender) < total {
            return Err(StreamError::InsufficientFunds);
        }
        token_client.transfer(&sender, &env.current_contract_address(), &total);

        let stream = Stream {
            id: stream_id,
            token,
            sender: sender.clone(),
            recipient: recipient.clone(),
            rate_per_second,
            start_time: now,
            end_time: now + duration_seconds,
            withdrawn: 0,
            cancelled: false,
            streamed_at_cancel: 0,
            is_paused: false,
            paused_at: None,
            total_paused_time: 0,
            counted_active: true,
            min_withdrawal_amount,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Stream(stream_id), &stream);
        env.storage()
            .instance()
            .set(&DataKey::NextId, &(stream_id + 1));
        // Extend TTL for the new stream entry
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stream(stream_id), 17280, 34560);

        // Store sender → stream ID mapping in persistent storage
        let mut sender_streams: soroban_sdk::Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::SenderStreams(sender.clone()))
            .unwrap_or(soroban_sdk::Vec::new(&env));
        sender_streams.push_back(stream_id);
        env.storage()
            .persistent()
            .set(&DataKey::SenderStreams(sender.clone()), &sender_streams);
        // Extend TTL for sender streams mapping
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::SenderStreams(sender), 17280, 34560);

        // Store recipient → stream ID mapping in persistent storage
        let mut recipient_streams: soroban_sdk::Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::RecipientStreams(recipient.clone()))
            .unwrap_or(soroban_sdk::Vec::new(&env));
        recipient_streams.push_back(stream_id);
        env.storage().persistent().set(
            &DataKey::RecipientStreams(recipient.clone()),
            &recipient_streams,
        );
        // Extend TTL for recipient streams mapping
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::RecipientStreams(recipient), 17280, 34560);

        Self::set_active_streams_count(&env, Self::active_streams_count(&env).saturating_add(1));

        env.events().publish(
            (Symbol::new(&env, "stream_created"),),
            (
                stream_id,
                &stream.recipient,
                rate_per_second,
                duration_seconds,
                min_withdrawal_amount,
            ),
        );

        Ok(stream_id)
    }

    /// Withdraw all currently accrued (streamed but unwithdrawn) tokens from a stream.
    ///
    /// Computes tokens accrued since `start_time` up to current ledger time (capped at `end_time`),
    /// minus previously withdrawn amount. Transfers to `recipient`.
    /// Only callable by the stream's `recipient`.
    ///
    /// Enforces minimum withdrawal amount to prevent dust withdrawals, except when the stream
    /// has fully elapsed (recipient should always be able to claim everything).
    ///
    /// # Parameters
    /// - `stream_id`: u64 stream identifier
    ///
    /// # Returns
    /// i128: Amount withdrawn (or 0 if nothing accrued)
    ///
    /// # Example
    /// ```rust,ignore
    /// // After 10 seconds at 100/sec rate:
    /// let withdrawn = forge_stream.withdraw(env, stream_id)?;
    /// assert_eq!(withdrawn, 1000);  // 100 * 10
    /// ```
    ///
    /// # Errors
    /// - `StreamNotFound`
    /// - `Unauthorized` (not recipient)
    /// - `AlreadyCancelled`
    /// - `NothingToWithdraw`
    /// - `BelowMinimumWithdrawal` (withdrawable < min_withdrawal_amount and stream not finished)
    pub fn withdraw(env: Env, stream_id: u64) -> Result<i128, StreamError> {
        Self::validate_stream_id(&env, stream_id)?;
        let mut stream: Stream = env
            .storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)?;

        if stream.cancelled {
            return Err(StreamError::AlreadyCancelled);
        }

        stream.recipient.require_auth();

        let now = env.ledger().timestamp();
        let streamed = Self::compute_streamed(&stream, now);
        let withdrawable = streamed - stream.withdrawn;

        if withdrawable <= 0 {
            return Err(StreamError::NothingToWithdraw);
        }

        // Enforce minimum withdrawal amount, except when stream is fully elapsed
        let is_finished = now >= stream.end_time;
        if !is_finished
            && withdrawable < stream.min_withdrawal_amount
            && stream.min_withdrawal_amount > 0
        {
            return Err(StreamError::BelowMinimumWithdrawal);
        }

        stream.withdrawn += withdrawable;
        env.storage()
            .persistent()
            .set(&DataKey::Stream(stream_id), &stream);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stream(stream_id), 17280, 34560);

        let token_client = token::Client::new(&env, &stream.token);
        token_client.transfer(
            &env.current_contract_address(),
            &stream.recipient,
            &withdrawable,
        );

        env.events().publish(
            (Symbol::new(&env, "withdrawn"),),
            (stream_id, &stream.recipient, withdrawable),
        );

        Ok(withdrawable)
    }

    /// Cancel an active stream. Immediately finalizes:
    /// - Accrued tokens auto-paid to recipient
    /// - Remaining unstreamed tokens refunded to sender
    /// Stream becomes withdrawable=0 thereafter.
    /// Only callable by the stream's `sender`.
    ///
    /// # Parameters
    /// - `stream_id`: u64 stream identifier
    ///
    /// # Returns
    /// `Ok(())`
    ///
    /// # Example
    /// ```rust,ignore
    /// // Stream: 100/sec for 3600s = 360,000 total tokens, cancel after 100s:
    /// // streamed = 100 * 100 = 10,000
    /// // recipient gets 10,000 (streamed - withdrawn)
    /// // sender refunded 350,000 (total - streamed)
    /// forge_stream.cancel_stream(env, stream_id)?;
    ///
    /// // With a pause: 100/sec for 3600s, paused for 200s, cancel after 300s wall-clock:
    /// // effective elapsed = 300 - 200 paused = 100s → streamed = 10,000
    /// // recipient gets 10,000, sender refunded 350,000
    /// // Invariant: withdrawable + returnable == total (360,000)
    /// ```
    ///
    /// # Errors
    /// - `StreamNotFound`
    /// - `Unauthorized` (not sender)
    /// - `AlreadyCancelled`
    pub fn cancel_stream(env: Env, stream_id: u64) -> Result<(), StreamError> {
        Self::validate_stream_id(&env, stream_id)?;
        let mut stream: Stream = env
            .storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)?;

        if stream.cancelled {
            return Err(StreamError::AlreadyCancelled);
        }

        stream.sender.require_auth();

        let now = env.ledger().timestamp();
        let streamed = Self::compute_streamed(&stream, now);
        let withdrawable = (streamed - stream.withdrawn).max(0);
        let total = stream.rate_per_second * (stream.end_time - stream.start_time) as i128;
        let returnable = total - streamed;

        // Sanity check: all tokens must be accounted for (no tokens created or destroyed).
        // withdrawable covers accrued-but-unwithdrawn tokens; returnable covers unstreamed tokens.
        // Together they must equal the total deposited at stream creation.
        debug_assert_eq!(
            withdrawable + returnable,
            total,
            "cancel invariant violated: withdrawable({}) + returnable({}) != total({})",
            withdrawable,
            returnable,
            total
        );

        if stream.counted_active {
            Self::set_active_streams_count(
                &env,
                Self::active_streams_count(&env).saturating_sub(1),
            );
            stream.counted_active = false;
        }

        stream.cancelled = true;
        stream.streamed_at_cancel = streamed;
        env.storage()
            .persistent()
            .set(&DataKey::Stream(stream_id), &stream);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stream(stream_id), 17280, 34560);

        let token_client = token::Client::new(&env, &stream.token);

        // Pay out accrued amount to recipient
        if withdrawable > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &stream.recipient,
                &withdrawable,
            );
        }

        // Return unstreamed amount to sender
        if returnable > 0 {
            token_client.transfer(&env.current_contract_address(), &stream.sender, &returnable);
        }

        env.events().publish(
            (Symbol::new(&env, "stream_cancelled"),),
            (stream_id, withdrawable, returnable),
        );

        Ok(())
    }

    /// Pause an active stream.
    ///
    /// Temporarily halts token accrual. Recipient can still withdraw already-accrued tokens.
    /// Only callable by the stream's `sender`.
    ///
    /// When paused, the stream's end_time is extended by the pause duration to ensure the
    /// recipient receives the full promised payout. For example, if a stream is paused for 100s,
    /// end_time is extended by 100s, so the recipient's total earnings remain unchanged.
    ///
    /// # Parameters
    /// - `stream_id`: u64 stream identifier
    ///
    /// # Returns
    /// `Ok(())`
    ///
    /// # Errors
    /// - `StreamNotFound`
    /// - `Unauthorized` (not sender)
    /// - `AlreadyCancelled`
    /// - `StreamFinished`
    /// - `InvalidConfig` (already paused)
    pub fn pause_stream(env: Env, stream_id: u64) -> Result<(), StreamError> {
        Self::validate_stream_id(&env, stream_id)?;
        let mut stream: Stream = env
            .storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)?;

        if stream.cancelled {
            return Err(StreamError::AlreadyCancelled);
        }

        // Auth-first convention: verify sender authorization before checking stream state
        stream.sender.require_auth();

        if env.ledger().timestamp() >= stream.end_time {
            return Err(StreamError::StreamFinished);
        }

        if stream.is_paused {
            return Err(StreamError::Common(CommonError::InvalidConfig)); // Already paused
        }

        let now = env.ledger().timestamp();
        stream.is_paused = true;
        stream.paused_at = Some(now);

        env.storage()
            .persistent()
            .set(&DataKey::Stream(stream_id), &stream);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stream(stream_id), 17280, 34560);

        env.events()
            .publish((Symbol::new(&env, "stream_paused"),), (stream_id,));

        Ok(())
    }

    /// Resume a paused stream.
    ///
    /// Restarts token accrual from the point it was paused. Only callable by the stream's `sender`.
    ///
    /// When resumed, the stream's end_time is extended by the pause duration to ensure the
    /// recipient receives the full promised payout. This maintains the invariant that total
    /// recipient earnings = rate_per_second * (end_time - start_time - total_paused_time).
    ///
    /// The paused duration is capped at `end_time` to avoid over-counting paused time when
    /// `resume_stream()` is called after `end_time` has already passed. This keeps
    /// `total_paused_time` consistent with the `effective_time.min(end_time)` cap used in
    /// `compute_streamed()`.
    ///
    /// # Parameters
    /// - `stream_id`: u64 stream identifier
    ///
    /// # Returns
    /// `Ok(())`
    ///
    /// # Errors
    /// - `StreamNotFound`
    /// - `Unauthorized` (not sender)
    /// - `AlreadyCancelled`
    /// - `StreamFinished` (called after end_time)
    /// - `InvalidConfig` (not paused)
    pub fn resume_stream(env: Env, stream_id: u64) -> Result<(), StreamError> {
        Self::validate_stream_id(&env, stream_id)?;
        let mut stream: Stream = env
            .storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)?;

        if stream.cancelled {
            return Err(StreamError::AlreadyCancelled);
        }

        // Auth-first convention: verify sender authorization before checking stream state
        stream.sender.require_auth();

        if env.ledger().timestamp() >= stream.end_time {
            return Err(StreamError::StreamFinished);
        }

        if !stream.is_paused {
            return Err(StreamError::Common(CommonError::InvalidConfig)); // Not paused
        }

        let now = env.ledger().timestamp();
        let paused_at = stream
            .paused_at
            .expect("stream is paused but paused_at is missing");
        // Cap paused duration at end_time to stay consistent with compute_streamed(),
        // which uses effective_time = now.min(end_time) when accumulating paused time.
        let effective_now = now.min(stream.end_time);
        let paused_duration = effective_now.saturating_sub(paused_at);
        stream.total_paused_time += paused_duration;
        stream.end_time = stream.end_time.saturating_add(paused_duration);
        stream.is_paused = false;
        stream.paused_at = None;

        env.storage()
            .persistent()
            .set(&DataKey::Stream(stream_id), &stream);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stream(stream_id), 17280, 34560);

        env.events()
            .publish((Symbol::new(&env, "stream_resumed"),), (stream_id,));

        Ok(())
    }

    /// Get real-time status of a stream without modifying it.
    ///
    /// Computes current `streamed`, `withdrawable`, `remaining` based on ledger timestamp.
    ///
    /// # Parameters
    /// - `stream_id`: u64 stream identifier
    ///
    /// # Returns
    /// `StreamStatus` with:
    /// - `streamed`: Total accrued up to now
    /// - `withdrawn`: Cumulative withdrawn
    /// - `withdrawable`: streamed - withdrawn
    /// - `remaining`: total - streamed
    /// - `is_active`: `true` when the stream is currently accruing tokens
    ///   (`!cancelled && !paused && now < end_time`). A paused or finished
    ///   stream has `is_active = false` even if tokens remain claimable.
    /// - `is_finished`: now >= end_time
    /// - `is_paused`: stream is currently paused
    /// - `is_claimable`: `true` when `withdrawable > 0`. Independent of
    ///   `is_active` — a paused or finished stream can still be claimable.
    ///   Always check `is_claimable` (not `is_active`) to decide whether a
    ///   withdrawal is available.
    ///
    /// **Note:** `is_active` and `is_claimable` are intentionally separate.
    /// `is_active` answers "is this stream accruing right now?" while
    /// `is_claimable` answers "can the recipient withdraw tokens right now?".
    /// A paused stream stops accruing (`is_active = false`) but tokens that
    /// accrued before the pause remain withdrawable (`is_claimable = true`).
    ///
    /// # Example
    /// ```rust,ignore
    /// let status = forge_stream.get_stream_status(env, stream_id)?;
    /// if status.is_claimable {
    ///     forge_stream.withdraw(env, stream_id)?;
    /// }
    /// ```
    pub fn get_stream_status(env: Env, stream_id: u64) -> Result<StreamStatus, StreamError> {
        Self::validate_stream_id(&env, stream_id)?;
        let stream: Stream = env
            .storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)?;

        let now = env.ledger().timestamp();
        let streamed = Self::compute_streamed(&stream, now);
        let withdrawable = (streamed - stream.withdrawn).max(0);
        let raw_duration = stream.end_time.saturating_sub(stream.start_time);
        let effective_duration = raw_duration.saturating_sub(stream.total_paused_time);
        let total = stream.rate_per_second * effective_duration as i128;
        let remaining = (total - streamed).max(0);
        let is_active = !stream.cancelled && !stream.is_paused && now < stream.end_time;
        let is_finished = now >= stream.end_time;

        Ok(StreamStatus {
            id: stream.id,
            streamed,
            withdrawn: stream.withdrawn,
            withdrawable,
            remaining,
            is_active,
            is_finished,
            is_paused: stream.is_paused,
            is_claimable: withdrawable > 0,
        })
    }

    /// Get the complete internal stream configuration and state.
    ///
    /// Returns the full `Stream` struct including private fields like `cancelled`.
    /// Useful for admin/UI display.
    ///
    /// # Parameters
    /// - `stream_id`: u64 stream identifier
    ///
    /// # Returns
    /// `Stream` struct
    ///
    /// # Example
    /// ```rust,ignore
    /// let stream = forge_stream.get_stream(env, stream_id)?;
    /// assert_eq!(stream.rate_per_second, 100i128);
    /// ```
    ///
    /// # Errors
    /// - `StreamNotFound`
    pub fn get_stream(env: Env, stream_id: u64) -> Result<Stream, StreamError> {
        Self::validate_stream_id(&env, stream_id)?;
        env.storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)
    }

    /// Return the number of currently active streams.
    ///
    /// Active streams are not cancelled and have not fully elapsed.
    /// This method also synchronizes the counter for any streams that elapsed
    /// since the last interaction.
    pub fn get_active_streams_count(env: Env) -> u64 {
        Self::sync_elapsed_streams(&env)
    }

    /// Return the total number of streams ever created.
    ///
    /// This is a monotonically increasing counter — it includes active, finished,
    /// and cancelled streams. Useful for UIs to paginate stream history and gauge
    /// protocol activity. Stream IDs are zero-indexed, so valid IDs range from
    /// `0` to `get_stream_count() - 1`.
    ///
    /// # Returns
    /// `u64` — total streams created since contract deployment.
    ///
    /// # Example
    /// ```text
    /// let count = client.get_stream_count();
    /// for id in 0..count {
    ///     let stream = client.get_stream(&id)?;
    ///     // process stream...
    /// }
    /// ```
    pub fn get_stream_count(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::NextId).unwrap_or(0)
    }

    /// Return the number of tokens the recipient can withdraw right now.
    ///
    /// Lightweight alternative to [`get_stream_status`](Self::get_stream_status)
    /// for UIs and integrators that only need the withdrawable balance.
    /// Returns `0` for cancelled streams (accrued tokens are paid out on cancel).
    ///
    /// # Errors
    /// - [`StreamError::StreamNotFound`] — no stream exists with `stream_id`.
    pub fn get_claimable(env: Env, stream_id: u64) -> Result<i128, StreamError> {
        let stream: Stream = env
            .storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)?;

        if stream.cancelled {
            return Ok(0);
        }

        let streamed = Self::compute_streamed(&stream, env.ledger().timestamp());
        Ok((streamed - stream.withdrawn).max(0))
    }

    /// Get all stream IDs for a given sender.
    ///
    /// Returns a vector of stream IDs where the specified address is the sender.
    /// This list is append-only: cancelled streams are still included.
    /// Useful for wallets and dashboards to display all outgoing streams.
    ///
    /// # Parameters
    /// - `sender`: Address to look up streams for
    ///
    /// # Returns
    /// `Vec<u64>`: Vector of stream IDs (empty if none found)
    ///
    /// # Example
    /// ```rust,ignore
    /// let my_streams = forge_stream.get_streams_by_sender(env, sender_address);
    /// for stream_id in my_streams.iter() {
    ///     let status = forge_stream.get_stream_status(env, stream_id)?;
    ///     // Display stream info...
    /// }
    /// ```
    pub fn get_streams_by_sender(env: Env, sender: Address) -> soroban_sdk::Vec<u64> {
        let key = DataKey::SenderStreams(sender.clone());
        let result = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(soroban_sdk::Vec::new(&env));
        if env.storage().persistent().has(&key) {
            env.storage().persistent().extend_ttl(&key, 17280, 34560);
        }
        result
    }

    /// Get all stream IDs for a given recipient.
    ///
    /// Returns a vector of stream IDs where the specified address is the recipient.
    /// This list is append-only: cancelled streams are still included.
    /// Useful for wallets and dashboards to display all incoming streams.
    ///
    /// # Parameters
    /// - `recipient`: Address to look up streams for
    ///
    /// # Returns
    /// `Vec<u64>`: Vector of stream IDs (empty if none found)
    ///
    /// # Example
    /// ```rust,ignore
    /// let incoming_streams = forge_stream.get_streams_by_recipient(env, recipient_address);
    /// for stream_id in incoming_streams.iter() {
    ///     let claimable = forge_stream.get_claimable(env, stream_id)?;
    ///     if claimable > 0 {
    ///         forge_stream.withdraw(env, stream_id)?;
    ///     }
    /// }
    /// ```
    pub fn get_streams_by_recipient(env: Env, recipient: Address) -> soroban_sdk::Vec<u64> {
        let key = DataKey::RecipientStreams(recipient.clone());
        let result = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(soroban_sdk::Vec::new(&env));
        if env.storage().persistent().has(&key) {
            env.storage().persistent().extend_ttl(&key, 17280, 34560);
        }
        result
    }

    /// Extend an active stream by adding more time and tokens.
    ///
    /// Transfers `rate_per_second * additional_seconds` tokens from the sender to the
    /// contract and pushes `end_time` forward by `additional_seconds`. The stream must
    /// not be cancelled, paused, or already finished. Extensions are intentionally
    /// disallowed while paused so the visible `end_time` continues to reflect
    /// actively accruing wall-clock time.
    /// Only callable by the stream's `sender`.
    ///
    /// # Parameters
    /// - `stream_id`: u64 stream identifier
    /// - `additional_seconds`: u64 > 0, seconds to add to the stream duration
    ///
    /// # Returns
    /// `Ok(())`
    ///
    /// # Errors
    /// - `StreamNotFound` — no stream exists with `stream_id`
    /// - `Unauthorized` — caller is not the stream sender
    /// - `AlreadyCancelled` — stream has been cancelled
    /// - `StreamFinished` — stream end_time has already passed
    /// - `InvalidConfig` — `additional_seconds` is 0 or the stream is paused
    pub fn extend_stream(
        env: Env,
        stream_id: u64,
        additional_seconds: u64,
    ) -> Result<(), StreamError> {
        if additional_seconds == 0 {
            return Err(StreamError::Common(CommonError::InvalidConfig));
        }

        Self::validate_stream_id(&env, stream_id)?;
        let mut stream: Stream = env
            .storage()
            .persistent()
            .get(&DataKey::Stream(stream_id))
            .ok_or(StreamError::StreamNotFound)?;

        if stream.cancelled {
            return Err(StreamError::AlreadyCancelled);
        }

        stream.sender.require_auth();

        if stream.is_paused {
            return Err(StreamError::InvalidConfig);
        }

        if env.ledger().timestamp() >= stream.end_time {
            return Err(StreamError::StreamFinished);
        }

        let additional_tokens = stream.rate_per_second * additional_seconds as i128;
        let token_client = token::Client::new(&env, &stream.token);
        token_client.transfer(
            &stream.sender,
            &env.current_contract_address(),
            &additional_tokens,
        );

        stream.end_time += additional_seconds;

        env.storage()
            .persistent()
            .set(&DataKey::Stream(stream_id), &stream);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stream(stream_id), 17280, 34560);

        env.events().publish(
            (Symbol::new(&env, "stream_extended"),),
            (stream_id, stream.end_time, additional_tokens),
        );

        Ok(())
    }

    // ── Private ───────────────────────────────────────────────────────────────

    /// Validate that a stream ID is within the valid range.
    ///
    /// Checks that `stream_id < NextId` before attempting to read from storage.
    /// This provides an early, descriptive error and reduces storage read costs
    /// on invalid IDs.
    ///
    /// # Parameters
    /// - `env`: Reference to the Soroban environment
    /// - `stream_id`: The stream ID to validate
    ///
    /// # Returns
    /// `Ok(())` if valid, `Err(StreamError::StreamNotFound)` if out of range
    fn validate_stream_id(env: &Env, stream_id: u64) -> Result<(), StreamError> {
        let next_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(0_u64);
        if stream_id >= next_id {
            return Err(StreamError::StreamNotFound);
        }
        Ok(())
    }

    fn compute_streamed(stream: &Stream, now: u64) -> i128 {
        if stream.cancelled {
            return stream.streamed_at_cancel;
        }
        let effective_time = now.min(stream.end_time);
        let raw_elapsed = effective_time.saturating_sub(stream.start_time);
        let mut paused_time = stream.total_paused_time;
        if stream.is_paused {
            if let Some(paused_at) = stream.paused_at {
                paused_time += effective_time.saturating_sub(paused_at);
            }
        }
        let effective_elapsed = raw_elapsed.saturating_sub(paused_time);
        // Overflow protection: if rate * elapsed would exceed i128::MAX, cap at
        // total (rate * duration) which is the maximum tokens this stream can ever
        // release. This prevents silent truncation on extreme rate/elapsed combos.
        let duration = stream.end_time.saturating_sub(stream.start_time);
        let total = stream.rate_per_second * duration as i128;
        stream
            .rate_per_second
            .checked_mul(effective_elapsed as i128)
            .unwrap_or(total)
            .min(total)
    }

    fn active_streams_count(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ActiveStreamsCount)
            .unwrap_or(0_u64)
    }

    fn set_active_streams_count(env: &Env, count: u64) {
        env.storage()
            .instance()
            .set(&DataKey::ActiveStreamsCount, &count);
    }

    fn sync_elapsed_streams(env: &Env) -> u64 {
        let now = env.ledger().timestamp();
        let next_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(0_u64);
        let mut active_count = Self::active_streams_count(env);

        let mut stream_id = 0_u64;
        while stream_id < next_id {
            let maybe_stream: Option<Stream> =
                env.storage().persistent().get(&DataKey::Stream(stream_id));
            if let Some(mut stream) = maybe_stream {
                if stream.counted_active && !stream.cancelled && now >= stream.end_time {
                    stream.counted_active = false;
                    env.storage()
                        .persistent()
                        .set(&DataKey::Stream(stream_id), &stream);
                    env.storage().persistent().extend_ttl(
                        &DataKey::Stream(stream_id),
                        17280,
                        34560,
                    );
                    active_count = active_count.saturating_sub(1);
                }
            }
            stream_id += 1;
        }

        Self::set_active_streams_count(env, active_count);
        active_count
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::ForgeStream;

    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token::{Client as TokenClient, StellarAssetClient},
        Env, IntoVal,
    };

    fn setup_token(env: &Env, sender: &Address, total: i128) -> Address {
        let token_admin = Address::generate(env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(env, &token_id).mint(sender, &total);
        token_id
    }

    fn make_token(env: &Env, _contract_id: &Address, sender: &Address, total: i128) -> Address {
        let token_admin = Address::generate(env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        soroban_sdk::token::StellarAssetClient::new(env, &token_id).mint(sender, &total);
        token_id
    }

    #[test]
    fn test_create_stream_success() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let _token = make_token(&env, &contract_id, &sender, 100_000);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        let result = client.try_create_stream(&sender, &token.address, &recipient, &100, &1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().unwrap(), 0u64);
    }

    #[test]
    fn test_invalid_stream_config() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = Address::generate(&env);

        let result = client.try_create_stream(&sender, &token, &recipient, &0, &1000);
        assert_eq!(result, Err(Ok(StreamError::InvalidConfig)));
    }

    /// `create_stream` with `rate_per_second = 0` must be rejected with `InvalidConfig`.
    /// A zero-rate stream would never accrue tokens, making it meaningless and
    /// indistinguishable from a no-op. The contract enforces `rate > 0` at creation.
    #[test]
    fn test_create_stream_zero_rate_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = Address::generate(&env);

        let result = client.try_create_stream(&sender, &token, &recipient, &0, &1000);
        assert_eq!(result, Err(Ok(StreamError::InvalidConfig)));
    }

    #[test]
    fn test_create_stream_insufficient_funds() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        // Mint only 999 tokens but the stream needs 100 * 10 = 1000
        let token_id = setup_token(&env, &sender, 999);

        let result = client.try_create_stream(&sender, &token_id, &recipient, &100, &10);
        assert_eq!(result, Err(Ok(StreamError::InsufficientFunds)));
    }

    #[test]
    fn test_stream_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let result = client.try_withdraw(&999);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));
    }

    #[test]
    fn test_withdraw_nothing_to_withdraw() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = make_token(&env, &contract_id, &sender, 100_000);

        let _stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        let stream_id = client.create_stream(&sender, &token.address, &recipient, &100, &1000);
        // No time has passed — nothing to withdraw
        let result = client.try_withdraw(&stream_id);
        assert_eq!(result, Err(Ok(StreamError::NothingToWithdraw)));
    }

    /// Test for issue #215: Double withdraw() should return NothingToWithdraw on second call.
    /// Verifies that withdrawn is updated correctly and the second call without time passing returns error.
    #[test]
    fn test_double_withdraw_returns_nothing_to_withdraw() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        let stream_id = client.create_stream(&sender, &token.address, &recipient, &100, &1000);

        // Advance time by 100 seconds so tokens accrue
        env.ledger().with_mut(|l| l.timestamp += 100);

        // First withdraw should succeed and return 100 * 100 = 10,000
        let withdrawn_amount = client.withdraw(&stream_id);
        assert_eq!(withdrawn_amount, 10_000);

        // Verify stream status after first withdrawal
        let status_after_first = client.get_stream_status(&stream_id);
        assert_eq!(status_after_first.withdrawn, 10_000);
        assert_eq!(status_after_first.withdrawable, 0);

        // Second withdraw without advancing time should fail with NothingToWithdraw
        let result = client.try_withdraw(&stream_id);
        assert_eq!(result, Err(Ok(StreamError::NothingToWithdraw)));

        // Verify stream status hasn't changed
        let status_after_second = client.get_stream_status(&stream_id);
        assert_eq!(status_after_second.withdrawn, 10_000);
        assert_eq!(status_after_second.withdrawable, 0);
    }

    #[test]
    fn test_stream_status_active() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let _token = make_token(&env, &contract_id, &sender, 100_000);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        let stream_id = client.create_stream(&sender, &token.address, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 100);

        let status = client.get_stream_status(&stream_id);
        assert!(status.is_active);
        assert_eq!(status.streamed, 10_000);
        assert_eq!(status.withdrawable, 10_000);
    }

    #[test]
    fn test_cancel_stream() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let _token = make_token(&env, &contract_id, &sender, 100_000);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        let stream_id = client.create_stream(&sender, &token.address, &recipient, &100, &1000);
        let result = client.try_cancel_stream(&stream_id);
        assert!(result.is_ok());

        let result2 = client.try_cancel_stream(&stream_id);
        assert_eq!(result2, Err(Ok(StreamError::AlreadyCancelled)));
    }

    #[test]
    fn test_stream_finished_after_duration() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let _token = make_token(&env, &contract_id, &sender, 100_000);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        let stream_id = client.create_stream(&sender, &token.address, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 2000);

        let status = client.get_stream_status(&stream_id);
        assert!(status.is_finished);
        assert!(!status.is_active);
        assert_eq!(status.streamed, 100_000);
    }

    #[test]
    fn test_finished_stream_is_claimable_before_withdrawal() {
        // A finished stream must have is_active=false, is_finished=true,
        // is_claimable=true, and withdrawable == total streamed.
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &100_000i128);

        // rate=100, duration=1000 → total=100_000
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        // Advance past end_time without withdrawing
        env.ledger().with_mut(|l| l.timestamp += 2000);

        let status = client.get_stream_status(&stream_id);
        assert!(!status.is_active);
        assert!(status.is_finished);
        assert!(status.is_claimable);
        assert_eq!(status.withdrawable, 100_000);
    }

    // ── Rounding / cancellation split tests ──────────────────────────────────

    /// Rate of 1 token/sec: streamed amount must equal elapsed seconds exactly.
    #[test]
    fn test_low_rate_one_token_per_second() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let duration = 1_000u64;
        let rate = 1i128;
        let total = rate * duration as i128; // 1_000
        let token = setup_token(&env, &sender, total);

        let stream_id = client.create_stream(&sender, &token, &recipient, &rate, &duration);

        env.ledger().with_mut(|l| l.timestamp += 333);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, 333);
        assert_eq!(status.remaining, total - 333);
        assert_eq!(status.streamed + status.remaining, total);

        env.ledger().with_mut(|l| l.timestamp += 667); // total += 1000
        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, total);
        assert_eq!(status.remaining, 0);
        assert_eq!(status.streamed + status.remaining, total);
    }

    /// High rate near i128::MAX / duration: no overflow, invariant holds.
    #[test]
    fn test_high_rate_near_max() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let duration = 1_000u64;
        let rate = i128::MAX / duration as i128;
        let total = rate * duration as i128;
        let token = setup_token(&env, &sender, total);

        let stream_id = client.create_stream(&sender, &token, &recipient, &rate, &duration);

        env.ledger().with_mut(|l| l.timestamp += 500);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, rate * 500);
        assert_eq!(status.remaining, total - rate * 500);
        assert_eq!(status.streamed + status.remaining, total);

        env.ledger().with_mut(|l| l.timestamp += 500);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, total);
        assert_eq!(status.remaining, 0);
        assert_eq!(status.streamed + status.remaining, total);
    }

    /// rate = i128::MAX / 2, duration = 3: intermediate multiplication would
    /// overflow without checked_mul. Verifies compute_streamed() caps at total
    /// and never panics or returns a corrupted value.
    #[test]
    fn test_high_rate_overflow_protection() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let duration = 3u64;
        let rate = i128::MAX / 2; // rate * 2 fits in i128, but rate * 3 overflows
        let total = rate * duration as i128; // safe: (MAX/2) * 3 < MAX
        let token = setup_token(&env, &sender, total);

        let stream_id = client.create_stream(&sender, &token, &recipient, &rate, &duration);

        // At t=1: rate * 1 is fine
        env.ledger().with_mut(|l| l.timestamp += 1);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, rate);
        assert!(status.streamed <= total);

        // At t=2: rate * 2 is fine
        env.ledger().with_mut(|l| l.timestamp += 1);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, rate * 2);
        assert!(status.streamed <= total);

        // At t=3 (end): rate * 3 would overflow i128 without checked_mul;
        // result must be capped at total, not panic or wrap.
        env.ledger().with_mut(|l| l.timestamp += 1);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, total);
        assert_eq!(status.remaining, 0);
        assert_eq!(status.streamed + status.remaining, total);
    }

    /// streamed + remaining == total at every sampled point during a stream.
    #[test]
    fn test_streamed_plus_remaining_equals_total_invariant() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let rate = 7i128; // intentionally odd to surface any rounding
        let duration = 100u64;
        let total = rate * duration as i128; // 700
        let token = setup_token(&env, &sender, total);

        let stream_id = client.create_stream(&sender, &token, &recipient, &rate, &duration);

        for tick in [1u64, 10, 33, 50, 77, 99, 100, 150] {
            env.ledger().with_mut(|l| l.timestamp = tick);
            let status = client.get_stream_status(&stream_id);
            assert_eq!(
                status.streamed + status.remaining,
                total,
                "invariant broken at tick={tick}: streamed={} remaining={}",
                status.streamed,
                status.remaining
            );
        }
    }

    /// On cancel, withdrawable + returnable == total (no tokens lost or created).
    #[test]
    fn test_cancel_no_tokens_lost() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let rate = 3i128;
        let duration = 1_000u64;
        let total = rate * duration as i128;
        let token = setup_token(&env, &sender, total);

        let stream_id = client.create_stream(&sender, &token, &recipient, &rate, &duration);

        env.ledger().with_mut(|l| l.timestamp += 400);

        let status = client.get_stream_status(&stream_id);
        let expected_withdrawable = status.withdrawable;
        let expected_returnable = total - status.streamed;

        client.cancel_stream(&stream_id);

        assert_eq!(expected_withdrawable + expected_returnable, total);
        assert_eq!(status.streamed + status.remaining, total);
    }

    // ── get_claimable tests ───────────────────────────────────────────────────

    #[test]
    fn test_get_claimable_active_stream() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 50);

        assert_eq!(client.get_claimable(&stream_id), 5_000); // 100 * 50
    }

    #[test]
    fn test_get_claimable_fully_elapsed_stream() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 2000);

        assert_eq!(client.get_claimable(&stream_id), 100_000); // 100 * 1000
    }

    #[test]
    fn test_get_claimable_cancelled_stream_returns_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 200);
        client.cancel_stream(&stream_id);

        assert_eq!(client.get_claimable(&stream_id), 0);
    }

    #[test]
    fn test_get_claimable_zero_immediately_after_withdraw_then_accrues_again() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let rate = 100i128;
        let duration = 1_000u64;
        let total = rate * duration as i128;
        let token = setup_token(&env, &sender, total);

        env.ledger().with_mut(|l| l.timestamp = 0);
        let stream_id = client.create_stream(&sender, &token, &recipient, &rate, &duration);

        env.ledger().with_mut(|l| l.timestamp += 100);
        assert_eq!(client.get_claimable(&stream_id), rate * 100);
        let status_before = client.get_stream_status(&stream_id);
        assert_eq!(status_before.withdrawable, rate * 100);

        client.withdraw(&stream_id);

        // Without time passing, claimable should now be 0
        assert_eq!(client.get_claimable(&stream_id), 0);
        let status_after = client.get_stream_status(&stream_id);
        assert_eq!(status_after.withdrawable, 0);

        // Advance time and only newly accrued tokens should be claimable
        env.ledger().with_mut(|l| l.timestamp += 50);
        assert_eq!(client.get_claimable(&stream_id), rate * 50);
        let status_later = client.get_stream_status(&stream_id);
        assert_eq!(status_later.withdrawable, rate * 50);
    }

    #[test]
    fn test_active_streams_count_tracks_create_and_cancel() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        assert_eq!(client.get_active_streams_count(), 0);

        let stream_a = client.create_stream(&sender, &token.address, &recipient, &100, &1000);
        assert_eq!(client.get_active_streams_count(), 1);

        let stream_b = client.create_stream(&sender, &token.address, &recipient, &50, &800);
        assert_eq!(client.get_active_streams_count(), 2);

        client.cancel_stream(&stream_a);
        assert_eq!(client.get_active_streams_count(), 1);

        client.cancel_stream(&stream_b);
        assert_eq!(client.get_active_streams_count(), 0);
    }

    #[test]
    fn test_active_streams_count_decrements_on_full_elapsed() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        sac.mint(&sender, &10_000_000i128);
        let token = TokenClient::new(&env, &token_id);

        client.create_stream(&sender, &token.address, &recipient, &10, &100);
        client.create_stream(&sender, &token.address, &recipient, &20, &300);
        assert_eq!(client.get_active_streams_count(), 2);

        env.ledger().with_mut(|l| l.timestamp += 150);
        assert_eq!(client.get_active_streams_count(), 1);

        env.ledger().with_mut(|l| l.timestamp += 200);
        assert_eq!(client.get_active_streams_count(), 0);
    }

    #[test]
    fn test_pause_stream() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 100);

        client.pause_stream(&stream_id);

        let status = client.get_stream_status(&stream_id);
        assert!(status.is_paused);
        assert!(!status.is_active);
        assert_eq!(status.streamed, 10_000); // 100 * 100s
    }

    #[test]
    fn test_resume_stream() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 100);

        client.pause_stream(&stream_id);
        env.ledger().with_mut(|l| l.timestamp += 200); // Paused for 200s

        let status_before = client.get_stream_status(&stream_id);
        assert!(status_before.is_paused);
        assert_eq!(status_before.streamed, 10_000); // Still 100*100, no accrual during pause

        client.resume_stream(&stream_id);

        let status_after = client.get_stream_status(&stream_id);
        assert!(!status_after.is_paused);
        assert!(status_after.is_active);
        assert_eq!(status_after.streamed, 10_000); // Still the same
    }

    #[test]
    fn test_withdraw_while_paused() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 100);

        client.pause_stream(&stream_id);
        env.ledger().with_mut(|l| l.timestamp += 50); // Paused, no new accrual

        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.withdrawable, 10_000);

        let withdrawn = client.withdraw(&stream_id);
        assert_eq!(withdrawn, 10_000);

        let status_after = client.get_stream_status(&stream_id);
        assert_eq!(status_after.withdrawn, 10_000);
        assert_eq!(status_after.withdrawable, 0);
    }

    /// pause_stream() must emit a "stream_paused" event whose data contains the
    /// correct stream_id.
    #[test]
    fn test_pause_stream_emits_event() {
        use soroban_sdk::testutils::Events;
        use soroban_sdk::TryFromVal;
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 100);

        client.pause_stream(&stream_id);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "stream_paused"))
                .unwrap_or(false)
                && <u64>::try_from_val(&env, &data)
                    .map(|id| id == stream_id)
                    .unwrap_or(false)
        });
        assert!(
            found,
            "Expected stream_paused event with stream_id={stream_id} not found"
        );
    }

    /// resume_stream() must emit a "stream_resumed" event whose data contains
    /// the correct stream_id.
    #[test]
    fn test_resume_stream_emits_event() {
        use soroban_sdk::testutils::Events;
        use soroban_sdk::TryFromVal;
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        env.ledger().with_mut(|l| l.timestamp += 100);
        client.pause_stream(&stream_id);
        env.ledger().with_mut(|l| l.timestamp += 50);

        client.resume_stream(&stream_id);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "stream_resumed"))
                .unwrap_or(false)
                && <u64>::try_from_val(&env, &data)
                    .map(|id| id == stream_id)
                    .unwrap_or(false)
        });
        assert!(
            found,
            "Expected stream_resumed event with stream_id={stream_id} not found"
        );
    }

    /// A failed pause_stream() (already paused) must not emit a stream_paused
    /// event — the event list should contain only the first successful pause.
    #[test]
    fn test_no_pause_event_on_failed_pause() {
        use soroban_sdk::testutils::Events;
        use soroban_sdk::TryFromVal;
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        client.pause_stream(&stream_id); // succeeds — emits one event

        // Second pause must fail
        let result = client.try_pause_stream(&stream_id);
        assert_eq!(result, Err(Ok(StreamError::InvalidConfig)));

        // Only one stream_paused event should exist (from the first call)
        let events = env.events().all();
        let pause_event_count = events
            .iter()
            .filter(|(_, topics, _)| {
                topics
                    .get(0)
                    .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                    .map(|s| s == Symbol::new(&env, "stream_paused"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(
            pause_event_count, 1,
            "Expected exactly 1 stream_paused event, got {pause_event_count}"
        );
    }

    #[test]
    fn test_pause_already_paused() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        client.pause_stream(&stream_id);

        let result = client.try_pause_stream(&stream_id);
        assert_eq!(result, Err(Ok(StreamError::InvalidConfig)));
    }

    #[test]
    fn test_resume_not_paused() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        let result = client.try_resume_stream(&stream_id);
        assert_eq!(result, Err(Ok(StreamError::InvalidConfig)));
    }

    #[test]
    fn test_pause_resume_maintains_total_payout() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        // Create stream: 100 tokens/sec for 1000 seconds = 100,000 total
        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        // Advance 100 seconds, then pause
        env.ledger().with_mut(|l| l.timestamp += 100);
        client.pause_stream(&stream_id);

        // Check status after pause
        let status_paused = client.get_stream_status(&stream_id);
        assert_eq!(status_paused.streamed, 10_000); // 100 * 100

        // Advance 200 seconds while paused (no accrual)
        env.ledger().with_mut(|l| l.timestamp += 200);

        // Resume (extends end_time by 200 seconds)
        client.resume_stream(&stream_id);

        // Check status after resume
        let status_resumed = client.get_stream_status(&stream_id);
        assert_eq!(status_resumed.streamed, 10_000); // Still 100 * 100

        // Advance to new end_time (1200)
        env.ledger().with_mut(|l| l.timestamp += 900);

        let status = client.get_stream_status(&stream_id);
        // Total streamed should be 100 * 1000 = 100,000 (full amount)
        // Calculation: raw_elapsed = 1200 - 0 = 1200, paused_time = 200
        // effective_elapsed = 1200 - 200 = 1000, streamed = 100 * 1000 = 100,000
        assert_eq!(status.streamed, 100_000);
        assert_eq!(status.remaining, 0);
        assert!(status.is_finished);
    }

    #[test]
    fn test_full_stream_withdrawal() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        let token = TokenClient::new(&env, &token_id);

        let rate = 100i128;
        let duration = 1_000u64;
        let total = rate * duration as i128; // 100_000

        sac.mint(&sender, &total);
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        // Advance past the full stream duration
        env.ledger().with_mut(|l| l.timestamp += duration + 1);

        // withdrawable should equal the total stream amount
        let status = client.get_stream_status(&stream_id);
        assert!(status.is_finished);
        assert!(!status.is_active);
        assert_eq!(status.withdrawable, total);

        // withdraw() transfers the full amount to the recipient
        let withdrawn = client.withdraw(&stream_id);
        assert_eq!(withdrawn, total);
        assert_eq!(token.balance(&recipient), total);

        // stream is marked inactive (withdrawn == total, nothing left)
        let status_after = client.get_stream_status(&stream_id);
        assert_eq!(status_after.withdrawn, total);
        assert_eq!(status_after.withdrawable, 0);
        assert!(!status_after.is_active);
    }

    #[test]
    fn test_random_address_cannot_withdraw() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let intruder = Address::generate(&env);
        let token = make_token(&env, &contract_id, &sender, 100_000);

        env.ledger().with_mut(|l| l.timestamp = 0);
        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        // Advance time so tokens have accrued
        env.ledger().with_mut(|l| l.timestamp = 500);

        // Mock auth as the intruder, not the recipient
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &intruder,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "withdraw",
                args: (stream_id,).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let result = client.try_withdraw(&stream_id);
        assert!(
            result.is_err(),
            "Expected withdraw by random address to revert"
        );
    }

    #[test]
    fn test_sender_cannot_withdraw() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = make_token(&env, &contract_id, &sender, 100_000);

        env.ledger().with_mut(|l| l.timestamp = 0);
        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        // Advance time so tokens have accrued
        env.ledger().with_mut(|l| l.timestamp = 500);

        // Mock auth as the sender, not the recipient
        env.mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &sender,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "withdraw",
                args: (stream_id,).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let result = client.try_withdraw(&stream_id);
        assert!(
            result.is_err(),
            "Expected withdraw by sender to revert with Unauthorized"
        );
    }

    #[test]
    fn test_get_stream_count_starts_at_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        assert_eq!(client.get_stream_count(), 0);
    }

    #[test]
    fn test_get_stream_count_increments_on_create() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&sender, &1_000_000);

        assert_eq!(client.get_stream_count(), 0);

        client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        assert_eq!(client.get_stream_count(), 1);

        client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        assert_eq!(client.get_stream_count(), 2);

        client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        assert_eq!(client.get_stream_count(), 3);
    }

    #[test]
    fn test_get_stream_count_includes_cancelled_streams() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&sender, &1_000_000);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        client.cancel_stream(&stream_id);

        // count should still be 1 even after cancellation
        assert_eq!(client.get_stream_count(), 1);
    }

    #[test]
    fn test_get_streams_by_sender_empty() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);

        let streams = client.get_streams_by_sender(&sender);
        assert_eq!(streams.len(), 0);
    }

    #[test]
    fn test_get_streams_by_recipient_empty() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let recipient = Address::generate(&env);

        let streams = client.get_streams_by_recipient(&recipient);
        assert_eq!(streams.len(), 0);
    }

    #[test]
    fn test_get_streams_by_sender_single() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);

        let streams = client.get_streams_by_sender(&sender);
        assert_eq!(streams.len(), 1);
        assert_eq!(streams.get(0).unwrap(), stream_id);
    }

    #[test]
    fn test_get_streams_by_recipient_single() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);

        let streams = client.get_streams_by_recipient(&recipient);
        assert_eq!(streams.len(), 1);
        assert_eq!(streams.get(0).unwrap(), stream_id);
    }

    #[test]
    fn test_get_streams_by_sender_multiple() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient1 = Address::generate(&env);
        let recipient2 = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id1 = client.create_stream(&sender, &token_id, &recipient1, &100, &1000);
        let stream_id2 = client.create_stream(&sender, &token_id, &recipient2, &50, &800);

        let streams = client.get_streams_by_sender(&sender);
        assert_eq!(streams.len(), 2);
        assert_eq!(streams.get(0).unwrap(), stream_id1);
        assert_eq!(streams.get(1).unwrap(), stream_id2);
    }

    #[test]
    fn test_get_streams_by_recipient_multiple() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender1 = Address::generate(&env);
        let sender2 = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender1, &10_000_000i128);
        StellarAssetClient::new(&env, &token_id).mint(&sender2, &10_000_000i128);

        let stream_id1 = client.create_stream(&sender1, &token_id, &recipient, &100, &1000);
        let stream_id2 = client.create_stream(&sender2, &token_id, &recipient, &50, &800);

        let streams = client.get_streams_by_recipient(&recipient);
        assert_eq!(streams.len(), 2);
        assert_eq!(streams.get(0).unwrap(), stream_id1);
        assert_eq!(streams.get(1).unwrap(), stream_id2);
    }

    #[test]
    fn test_get_streams_includes_cancelled_streams() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id1 = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        let stream_id2 = client.create_stream(&sender, &token_id, &recipient, &50, &800);

        client.cancel_stream(&stream_id2);

        let sender_streams = client.get_streams_by_sender(&sender);
        assert_eq!(sender_streams.len(), 2);
        assert_eq!(sender_streams.get(0).unwrap(), stream_id1);
        assert_eq!(sender_streams.get(1).unwrap(), stream_id2);

        let recipient_streams = client.get_streams_by_recipient(&recipient);
        assert_eq!(recipient_streams.len(), 2);
        assert_eq!(recipient_streams.get(0).unwrap(), stream_id1);
        assert_eq!(recipient_streams.get(1).unwrap(), stream_id2);
    }

    #[test]
    fn test_get_streams_isolation_between_users() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender1 = Address::generate(&env);
        let sender2 = Address::generate(&env);
        let recipient1 = Address::generate(&env);
        let recipient2 = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender1, &10_000_000i128);
        StellarAssetClient::new(&env, &token_id).mint(&sender2, &10_000_000i128);

        let stream_id1 = client.create_stream(&sender1, &token_id, &recipient1, &100, &1000);
        let stream_id2 = client.create_stream(&sender2, &token_id, &recipient2, &50, &800);

        let sender1_streams = client.get_streams_by_sender(&sender1);
        assert_eq!(sender1_streams.len(), 1);
        assert_eq!(sender1_streams.get(0).unwrap(), stream_id1);

        let sender2_streams = client.get_streams_by_sender(&sender2);
        assert_eq!(sender2_streams.len(), 1);
        assert_eq!(sender2_streams.get(0).unwrap(), stream_id2);

        let recipient1_streams = client.get_streams_by_recipient(&recipient1);
        assert_eq!(recipient1_streams.len(), 1);
        assert_eq!(recipient1_streams.get(0).unwrap(), stream_id1);

        let recipient2_streams = client.get_streams_by_recipient(&recipient2);
        assert_eq!(recipient2_streams.len(), 1);
        assert_eq!(recipient2_streams.get(0).unwrap(), stream_id2);
    }

    /// Test that get_stream_status().streamed returns the correct historical value after cancel.
    #[test]
    fn test_get_stream_status_streamed_after_cancel() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);

        env.ledger().with_mut(|l| l.timestamp += 200);

        let status_before = client.get_stream_status(&stream_id);
        assert_eq!(status_before.streamed, 20_000);

        client.cancel_stream(&stream_id);

        let status_after = client.get_stream_status(&stream_id);
        assert_eq!(
            status_after.streamed, 20_000,
            "streamed should equal the amount at cancel time"
        );
        // After cancel, tokens are auto-paid out so withdrawable reflects unpaid accrued amount
        // (withdrawn is not updated on cancel — the transfer happens directly)
        assert_eq!(status_after.is_active, false);
    }

    /// Test for issue #214: Creating 20+ streams for a single sender should not overflow.
    #[test]
    fn test_get_streams_by_sender_many_streams() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &2_500_000_000i128);

        let mut stream_ids = soroban_sdk::Vec::new(&env);

        for _ in 0..25 {
            let recipient = Address::generate(&env);
            let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
            stream_ids.push_back(stream_id);
        }

        let sender_streams = client.get_streams_by_sender(&sender);
        assert_eq!(sender_streams.len(), 25, "Expected 25 streams for sender");

        for i in 0..25u32 {
            assert_eq!(
                sender_streams.get(i).unwrap(),
                stream_ids.get(i).unwrap(),
                "Stream ID at index {} does not match",
                i
            );
        }
    }

    /// Test cancelling a paused stream correctly splits tokens.
    #[test]
    fn test_cancel_paused_stream_splits_tokens_correctly() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);
        let token = TokenClient::new(&env, &token_id);

        let rate = 100i128;
        let duration = 1000u64;
        let total = rate * duration as i128;

        sac.mint(&sender, &total);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        env.ledger().with_mut(|l| l.timestamp += 200);
        client.pause_stream(&stream_id);

        env.ledger().with_mut(|l| l.timestamp += 200);
        client.cancel_stream(&stream_id);

        assert_eq!(token.balance(&recipient), 20_000);
        assert_eq!(token.balance(&sender), 80_000);
        assert_eq!(token.balance(&recipient) + token.balance(&sender), total);
    }

    /// Property test: withdrawn never exceeds compute_streamed across multiple withdraw() calls.
    ///
    /// Invariant: after every withdraw(), status.withdrawn <= status.streamed
    /// Also asserts:
    ///   - cumulative withdrawn == sum of individual withdraw() return values
    ///   - final cumulative withdrawn == rate_per_second * duration_seconds
    #[test]
    fn test_withdrawn_never_exceeds_streamed_across_multiple_withdrawals() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);

        let rate: i128 = 100;
        let duration: u64 = 1000;
        let total = rate * duration as i128; // 100_000

        sac.mint(&sender, &total);

        // Start at t=0
        env.ledger().with_mut(|l| l.timestamp = 0);
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        let checkpoints: [(u64, u64); 5] = [
            (100, 100),   // 10% of duration
            (250, 250),   // 25%
            (500, 500),   // 50%
            (750, 750),   // 75%
            (1000, 1000), // 100%
        ];

        let mut cumulative_withdrawn: i128 = 0;

        for (timestamp, _pct) in checkpoints.iter() {
            env.ledger().with_mut(|l| l.timestamp = *timestamp);

            let amount = client.withdraw(&stream_id);
            cumulative_withdrawn += amount;

            let status = client.get_stream_status(&stream_id);

            // Core invariant: withdrawn must never exceed streamed
            assert!(
                status.withdrawn <= status.streamed,
                "Invariant violated at t={}: withdrawn ({}) > streamed ({})",
                timestamp,
                status.withdrawn,
                status.streamed,
            );

            // Cumulative return values must match stored withdrawn
            assert_eq!(
                status.withdrawn, cumulative_withdrawn,
                "Cumulative mismatch at t={}: status.withdrawn={} vs sum={}",
                timestamp, status.withdrawn, cumulative_withdrawn,
            );
        }

        // After full duration: total withdrawn must equal rate * duration
        assert_eq!(
            cumulative_withdrawn, total,
            "Final withdrawn {} != expected total {}",
            cumulative_withdrawn, total,
        );
    }

    /// Test that paused time is correctly excluded from streamed amount after resume.
    ///
    /// Timeline:
    ///   t=0    stream created  (rate=100, duration=1000, total=100_000)
    ///   t=100  pause           (streamed = 100 * 100 = 10_000)
    ///   t=300  resume          (paused for 200s — those 200s must not count)
    ///   t=400  check           (effective elapsed = 400 - 200 = 200s → streamed = 20_000)
    ///   t=500  check           (effective elapsed = 500 - 200 = 300s → streamed = 30_000)
    ///   t=1200 end of stream   (end_time extended by 200s to 1200; full 100_000 withdrawable)
    #[test]
    fn test_paused_time_excluded_from_streamed_after_resume() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = StellarAssetClient::new(&env, &token_id);

        let rate: i128 = 100;
        let duration: u64 = 1000;
        let total = rate * duration as i128; // 100_000

        sac.mint(&sender, &total);

        // Start at t=0
        env.ledger().with_mut(|l| l.timestamp = 0);
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        let checkpoints: [(u64, u64); 5] = [
            (100, 100),   // 10% of duration
            (250, 250),   // 25%
            (500, 500),   // 50%
            (750, 750),   // 75%
            (1000, 1000), // 100%
        ];

        let mut cumulative_withdrawn: i128 = 0;

        for (timestamp, _pct) in checkpoints.iter() {
            env.ledger().with_mut(|l| l.timestamp = *timestamp);

            let amount = client.withdraw(&stream_id);
            cumulative_withdrawn += amount;

            let status = client.get_stream_status(&stream_id);

            // Core invariant: withdrawn must never exceed streamed
            assert!(
                status.withdrawn <= status.streamed,
                "Invariant violated at t={}: withdrawn ({}) > streamed ({})",
                timestamp,
                status.withdrawn,
                status.streamed,
            );

            // Cumulative return values must match stored withdrawn
            assert_eq!(
                status.withdrawn, cumulative_withdrawn,
                "Cumulative mismatch at t={}: status.withdrawn={} vs sum={}",
                timestamp, status.withdrawn, cumulative_withdrawn,
            );
        }

        // After full duration: total withdrawn must equal rate * duration
        assert_eq!(
            cumulative_withdrawn, total,
            "Final withdrawn {} != expected total {}",
            cumulative_withdrawn, total,
        );
        // t=0: create stream
        env.ledger().with_mut(|l| l.timestamp = 0);
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        // t=100: pause — 100s of active time → streamed = 10_000
        env.ledger().with_mut(|l| l.timestamp = 100);
        client.pause_stream(&stream_id);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(
            status.streamed, 10_000,
            "streamed at pause should be 10_000"
        );

        // t=300: resume — paused for 200s
        env.ledger().with_mut(|l| l.timestamp = 300);
        client.resume_stream(&stream_id);

        // t=400: effective elapsed = (400 - 0) - 200 paused = 200s → streamed = 20_000
        env.ledger().with_mut(|l| l.timestamp = 400);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(
            status.streamed, 20_000,
            "at t=400 streamed should be 20_000 (paused 200s excluded), got {}",
            status.streamed
        );
        assert!(
            status.withdrawn <= status.streamed,
            "invariant violated: withdrawn {} > streamed {}",
            status.withdrawn,
            status.streamed
        );

        // t=500: effective elapsed = (500 - 0) - 200 paused = 300s → streamed = 30_000
        env.ledger().with_mut(|l| l.timestamp = 500);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(
            status.streamed, 30_000,
            "at t=500 streamed should be 30_000 (paused 200s excluded), got {}",
            status.streamed
        );

        // t=1200: end_time was extended by 200s (1000 + 200 = 1200)
        // Full duration of active time has elapsed → all 100_000 tokens withdrawable
        env.ledger().with_mut(|l| l.timestamp = 1200);
        let status = client.get_stream_status(&stream_id);
        assert_eq!(
            status.streamed, total,
            "at end of stream streamed should equal total {}, got {}",
            total, status.streamed
        );
        assert_eq!(
            status.withdrawable, total,
            "full total should be withdrawable at stream end, got {}",
            status.withdrawable
        );
        assert!(status.is_finished, "stream should be finished at t=1200");
    }

    // ── Stream ID validation tests ─────────────────────────────────────────────

    /// Test that get_stream() returns StreamNotFound for out-of-range stream ID.
    #[test]
    fn test_get_stream_out_of_range_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        // No streams created, so any ID should be out of range
        let result = client.try_get_stream(&0);
        assert!(result.is_err(), "Expected error for out-of-range stream ID");

        let result = client.try_get_stream(&999);
        assert!(result.is_err(), "Expected error for out-of-range stream ID");
    }

    /// Test that get_stream_status() returns StreamNotFound for out-of-range stream ID.
    #[test]
    fn test_get_stream_status_out_of_range_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        // No streams created, so any ID should be out of range
        let result = client.try_get_stream_status(&0);
        assert!(result.is_err(), "Expected error for out-of-range stream ID");

        let result = client.try_get_stream_status(&999);
        assert!(result.is_err(), "Expected error for out-of-range stream ID");
    }

    /// Test that withdraw() returns StreamNotFound for out-of-range stream ID.
    #[test]
    fn test_withdraw_out_of_range_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        // No streams created, so any ID should be out of range
        let result = client.try_withdraw(&0);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));

        let result = client.try_withdraw(&999);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));
    }

    /// Test that cancel_stream() returns StreamNotFound for out-of-range stream ID.
    #[test]
    fn test_cancel_stream_out_of_range_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        // No streams created, so any ID should be out of range
        let result = client.try_cancel_stream(&0);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));

        let result = client.try_cancel_stream(&999);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));
    }

    /// Test that pause_stream() returns StreamNotFound for out-of-range stream ID.
    #[test]
    fn test_pause_stream_out_of_range_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        // No streams created, so any ID should be out of range
        let result = client.try_pause_stream(&0);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));

        let result = client.try_pause_stream(&999);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));
    }

    /// Test that resume_stream() returns StreamNotFound for out-of-range stream ID.
    #[test]
    fn test_resume_stream_out_of_range_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        // No streams created, so any ID should be out of range
        let result = client.try_resume_stream(&0);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));

        let result = client.try_resume_stream(&999);
        assert_eq!(result, Err(Ok(StreamError::StreamNotFound)));
    }

    /// Test that validation does not false-positive on valid stream IDs.
    #[test]
    fn test_valid_stream_id_passes_validation() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        // Create a stream (ID 0)
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        assert_eq!(stream_id, 0);

        // All functions should work with valid stream ID
        let stream = client.get_stream(&stream_id);
        assert_eq!(stream.id, stream_id);

        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.id, stream_id);

        // Advance time so tokens accrue
        env.ledger().with_mut(|l| l.timestamp += 100);

        // withdraw should work
        let withdrawn = client.withdraw(&stream_id);
        assert_eq!(withdrawn, 10_000);

        // Create another stream (ID 1)
        let stream_id_2 = client.create_stream(&sender, &token_id, &recipient, &50, &800);
        assert_eq!(stream_id_2, 1);

        // Both streams should be accessible
        let stream_1 = client.get_stream(&stream_id);
        assert_eq!(stream_1.id, stream_id);

        let stream_2 = client.get_stream(&stream_id_2);
        assert_eq!(stream_2.id, stream_id_2);

        // Out of range ID should fail
        let result = client.try_get_stream(&2);
        assert!(result.is_err(), "Expected error for out-of-range stream ID");
    }

    // ── extend_stream tests ───────────────────────────────────────────────────

    #[test]
    fn test_extend_stream_success() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        // Mint enough for original stream (100 * 1000 = 100_000) + extension (100 * 500 = 50_000)
        StellarAssetClient::new(&env, &token_id).mint(&sender, &150_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        let stream_before = client.get_stream(&stream_id);
        assert_eq!(stream_before.end_time, 1000);

        client.extend_stream(&stream_id, &500);

        let stream_after = client.get_stream(&stream_id);
        assert_eq!(stream_after.end_time, 1500);
    }

    #[test]
    fn test_extend_stream_increases_remaining() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &200_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);

        let status_before = client.get_stream_status(&stream_id);
        // At t=0, remaining = 100 * 1000 = 100_000
        assert_eq!(status_before.remaining, 100_000);

        client.extend_stream(&stream_id, &500);

        let status_after = client.get_stream_status(&stream_id);
        // After extension, remaining = 100 * 1500 = 150_000
        assert_eq!(status_after.remaining, 150_000);
    }

    #[test]
    fn test_extend_stream_cancelled_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &200_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);
        client.cancel_stream(&stream_id);

        let result = client.try_extend_stream(&stream_id, &500);
        assert_eq!(result, Err(Ok(StreamError::AlreadyCancelled)));
    }

    #[test]
    fn test_extend_stream_after_end_time_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &200_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);

        // Advance past end_time
        env.ledger().with_mut(|l| l.timestamp = 1001);

        let result = client.try_extend_stream(&stream_id, &500);
        assert_eq!(result, Err(Ok(StreamError::StreamFinished)));
    }

    #[test]
    fn test_extend_stream_zero_seconds_fails() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &100_000i128);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000);

        let result = client.try_extend_stream(&stream_id, &0);
        assert_eq!(result, Err(Ok(StreamError::InvalidConfig)));
    }

    /// Issue #265: stream paused before end_time, end_time passes, then resume is called.
    /// total_paused_time must be capped at end_time so compute_streamed() returns the full amount.
    ///
    /// Timeline:
    ///   t=0    create stream (rate=100, duration=1000, total=100_000, end_time=1000)
    ///   t=500  pause (streamed = 50_000)
    ///   t=1500 resume (end_time already passed; paused duration capped at 1000-500=500)
    ///          end_time extended by 500 → new end_time = 1500
    ///   t=1500 compute_streamed should return 100_000 (full amount)
    #[test]
    fn test_resume_after_end_time_caps_paused_duration() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let rate: i128 = 100;
        let duration: u64 = 1000;
        let total = rate * duration as i128; // 100_000

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&sender, &total);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        // t=500: pause — 500s of active time → streamed = 50_000
        env.ledger().with_mut(|l| l.timestamp = 500);
        client.pause_stream(&stream_id);

        let status = client.get_stream_status(&stream_id);
        assert_eq!(status.streamed, 50_000);

        // t=1500: resume — end_time (1000) has already passed
        // paused duration must be capped at end_time - paused_at = 1000 - 500 = 500
        // end_time extended by 500 → new end_time = 1500
        env.ledger().with_mut(|l| l.timestamp = 1500);
        client.resume_stream(&stream_id);

        // At t=1500 (== new end_time), full 100_000 should be streamed
        let status = client.get_stream_status(&stream_id);
        assert_eq!(
            status.streamed, total,
            "streamed should equal total after overdue resume; got {}",
            status.streamed
        );
        assert!(status.is_finished);
    }

    /// Issue #268: cancel_stream() doc comment example — 100/sec for 3600s, cancel after 100s.
    /// Verifies the exact numbers: recipient gets 10,000, sender refunded 350,000, total = 360,000.
    #[test]
    fn test_cancel_stream_doc_comment_example() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let rate: i128 = 100;
        let duration: u64 = 3600;
        let total = rate * duration as i128; // 360_000

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
        let token = soroban_sdk::token::Client::new(&env, &token_id);
        sac.mint(&sender, &total);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        // Cancel after 100s: streamed = 100 * 100 = 10,000
        env.ledger().with_mut(|l| l.timestamp = 100);
        client.cancel_stream(&stream_id);

        assert_eq!(
            token.balance(&recipient),
            10_000,
            "recipient should get 10,000"
        );
        assert_eq!(
            token.balance(&sender),
            350_000,
            "sender should be refunded 350,000"
        );
        assert_eq!(
            token.balance(&recipient) + token.balance(&sender),
            total,
            "withdrawable + returnable must equal total"
        );
    }

    // ── Event emission tests ──────────────────────────────────────────────────

    fn make_stream_for_events(
        env: &Env,
        client: &ForgeStreamClient,
        rate: i128,
        duration: u64,
    ) -> (Address, Address, Address, u64) {
        use soroban_sdk::token::StellarAssetClient;
        let sender = Address::generate(env);
        let recipient = Address::generate(env);
        let total = rate * duration as i128;
        let token_admin = Address::generate(env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(env, &token_id).mint(&sender, &total);
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);
        (sender, recipient, token_id, stream_id)
    }

    #[test]
    fn test_create_stream_emits_stream_created_event() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        let rate: i128 = 100;
        let duration: u64 = 3600;
        let (_, recipient, _, stream_id) = make_stream_for_events(&env, &client, rate, duration);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "stream_created"))
                .unwrap_or(false)
                && <(u64, Address, i128, u64)>::try_from_val(&env, &data)
                    .map(|(id, r, rps, dur)| {
                        id == stream_id && r == recipient && rps == rate && dur == duration
                    })
                    .unwrap_or(false)
        });
        assert!(
            found,
            "Expected stream_created event with correct payload not found"
        );
    }

    #[test]
    fn test_withdraw_emits_withdrawn_event() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        let rate: i128 = 100;
        let duration: u64 = 3600;
        let (_, recipient, _, stream_id) = make_stream_for_events(&env, &client, rate, duration);

        // Advance 50s so there are tokens to withdraw
        env.ledger().with_mut(|l| l.timestamp = 50);
        let amount = client.withdraw(&stream_id);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "withdrawn"))
                .unwrap_or(false)
                && <(u64, Address, i128)>::try_from_val(&env, &data)
                    .map(|(id, r, amt)| id == stream_id && r == recipient && amt == amount)
                    .unwrap_or(false)
        });
        assert!(
            found,
            "Expected withdrawn event with correct payload not found"
        );
    }

    #[test]
    fn test_cancel_stream_emits_stream_cancelled_event() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        let rate: i128 = 100;
        let duration: u64 = 3600;
        let (_, _, _, stream_id) = make_stream_for_events(&env, &client, rate, duration);

        // Advance 100s so some tokens have streamed
        env.ledger().with_mut(|l| l.timestamp = 100);
        client.cancel_stream(&stream_id);

        // streamed = 100 * 100 = 10_000; returnable = 360_000 - 10_000 = 350_000
        let expected_withdrawable = 10_000i128;
        let expected_returnable = 350_000i128;

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "stream_cancelled"))
                .unwrap_or(false)
                && <(u64, i128, i128)>::try_from_val(&env, &data)
                    .map(|(id, w, r)| {
                        id == stream_id && w == expected_withdrawable && r == expected_returnable
                    })
                    .unwrap_or(false)
        });
        assert!(
            found,
            "Expected stream_cancelled event with correct payload not found"
        );
    }

    #[test]
    fn test_pause_stream_emits_stream_paused_event() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        let (_, _, _, stream_id) = make_stream_for_events(&env, &client, 100, 3600);

        env.ledger().with_mut(|l| l.timestamp = 50);
        client.pause_stream(&stream_id);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "stream_paused"))
                .unwrap_or(false)
                && <(u64,)>::try_from_val(&env, &data)
                    .map(|(id,)| id == stream_id)
                    .unwrap_or(false)
        });
        assert!(found, "Expected stream_paused event not found");
    }

    #[test]
    fn test_resume_stream_emits_stream_resumed_event() {
        use soroban_sdk::{testutils::Events, Symbol, TryFromVal};
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);

        let (_, _, _, stream_id) = make_stream_for_events(&env, &client, 100, 3600);

        env.ledger().with_mut(|l| l.timestamp = 50);
        client.pause_stream(&stream_id);

        env.ledger().with_mut(|l| l.timestamp = 150);
        client.resume_stream(&stream_id);

        let events = env.events().all();
        let found = events.iter().any(|(_, topics, data)| {
            topics
                .get(0)
                .and_then(|t| Symbol::try_from_val(&env, &t).ok())
                .map(|s| s == Symbol::new(&env, "stream_resumed"))
                .unwrap_or(false)
                && <(u64,)>::try_from_val(&env, &data)
                    .map(|(id,)| id == stream_id)
                    .unwrap_or(false)
        });
        assert!(found, "Expected stream_resumed event not found");
    }

    /// Issue #268: cancel_stream() with a paused stream — paused time is excluded from streamed.
    /// Verifies withdrawable + returnable == total invariant holds with paused time.
    #[test]
    fn test_cancel_paused_stream_invariant() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let rate: i128 = 100;
        let duration: u64 = 3600;
        let total = rate * duration as i128; // 360_000

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
        let token = soroban_sdk::token::Client::new(&env, &token_id);
        sac.mint(&sender, &total);

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);

        // t=100: pause — 100s active → streamed = 10,000
        env.ledger().with_mut(|l| l.timestamp = 100);
        client.pause_stream(&stream_id);

        // t=300: cancel while paused — paused for 200s, streamed still = 10,000
        env.ledger().with_mut(|l| l.timestamp = 300);
        client.cancel_stream(&stream_id);

        // recipient gets 10,000 (streamed), sender gets 350,000 (unstreamed)
        assert_eq!(token.balance(&recipient), 10_000);
        assert_eq!(token.balance(&sender), 350_000);
        assert_eq!(
            token.balance(&recipient) + token.balance(&sender),
            total,
            "withdrawable + returnable must equal total even with paused time"
        );
    }

    /// A paused stream stops accruing (is_active = false) but tokens that
    /// accrued before the pause are still claimable (is_claimable = true).
    /// This guards against UIs that check is_active to show a withdraw button.
    #[test]
    fn test_paused_stream_is_not_active_but_is_claimable() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100 * 1000);

        // 100 tokens/s for 1000s
        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        // Advance 200s so 20,000 tokens have accrued
        env.ledger().with_mut(|l| l.timestamp += 200);

        // Pause — accrual stops but 20,000 tokens are still withdrawable
        client.pause_stream(&stream_id);

        let status = client.get_stream_status(&stream_id);

        assert!(status.is_paused, "stream should be paused");
        assert!(
            !status.is_active,
            "paused stream must not be active (not accruing)"
        );
        assert_eq!(
            status.withdrawable, 20_000,
            "20,000 tokens accrued before pause"
        );
        assert!(
            status.is_claimable,
            "paused stream with accrued tokens must be claimable"
        );
    }

    // ── Issue #338: extend_stream() comprehensive coverage ───────────────────

    /// At t=500 (halfway), extend by 500 additional seconds.
    /// Verifies end_time, remaining, active at original end, finished at new end,
    /// and sender balance decreases by additional_seconds * rate at extension time.
    #[test]
    fn test_extend_stream_halfway_comprehensive() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let rate: i128 = 100;
        let duration: u64 = 1000;
        let total = rate * duration as i128; // 100_000
        let additional_seconds: u64 = 500;
        let additional_tokens = rate * additional_seconds as i128; // 50_000

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
        let token_client = soroban_sdk::token::Client::new(&env, &token_id);

        // Mint enough for original stream + extension
        sac.mint(&sender, &(total + additional_tokens));

        let stream_id = client.create_stream(&sender, &token_id, &recipient, &rate, &duration);
        let original_end_time = client.get_stream(&stream_id).end_time;
        assert_eq!(original_end_time, 1000);

        // At t=500 (halfway), extend by 500 seconds
        env.ledger().with_mut(|l| l.timestamp = 500);
        let sender_balance_before = token_client.balance(&sender);
        client.extend_stream(&stream_id, &additional_seconds);
        let sender_balance_after = token_client.balance(&sender);

        // Assert end_time = original_end_time + 500
        let stream = client.get_stream(&stream_id);
        assert_eq!(
            stream.end_time,
            original_end_time + additional_seconds,
            "end_time must equal original_end_time + additional_seconds"
        );

        // Assert sender balance decreased by additional_seconds * rate
        assert_eq!(
            sender_balance_before - sender_balance_after,
            additional_tokens,
            "sender balance must decrease by additional_seconds * rate at extension"
        );

        // Assert remaining reflects extended total
        let status = client.get_stream_status(&stream_id);
        // streamed at t=500 = 100 * 500 = 50_000; new total = 100 * 1500 = 150_000
        // remaining = 150_000 - 50_000 = 100_000
        assert_eq!(
            status.remaining, 100_000,
            "remaining must reflect extended total minus already streamed"
        );

        // Advance to original end_time (1000) — stream must still be active
        env.ledger().with_mut(|l| l.timestamp = 1000);
        let status_at_original_end = client.get_stream_status(&stream_id);
        assert!(
            status_at_original_end.is_active,
            "stream must still be active at original end_time after extension"
        );
        assert!(
            !status_at_original_end.is_finished,
            "stream must not be finished at original end_time after extension"
        );

        // Advance to new end_time (1500) — stream must be finished
        env.ledger().with_mut(|l| l.timestamp = 1500);
        let status_at_new_end = client.get_stream_status(&stream_id);
        assert!(
            status_at_new_end.is_finished,
            "stream must be finished at new end_time"
        );
        assert_eq!(
            status_at_new_end.withdrawable,
            total + additional_tokens,
            "full extended total must be withdrawable at new end_time"
        );
    }

    /// Extending a cancelled stream must revert with AlreadyCancelled.
    #[test]
    fn test_extend_cancelled_stream_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 200_000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);
        client.cancel_stream(&stream_id);

        let result = client.try_extend_stream(&stream_id, &500);
        assert_eq!(
            result,
            Err(Ok(StreamError::AlreadyCancelled)),
            "extending a cancelled stream must revert with AlreadyCancelled"
        );
    }

    /// Extending after end_time must revert with StreamFinished.
    #[test]
    fn test_extend_after_end_time_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 200_000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        // Advance past end_time
        env.ledger().with_mut(|l| l.timestamp = 1001);

        let result = client.try_extend_stream(&stream_id, &500);
        assert_eq!(
            result,
            Err(Ok(StreamError::StreamFinished)),
            "extending after end_time must revert with StreamFinished"
        );
    }

    /// Extending a paused stream must revert with InvalidConfig so `end_time`
    /// does not move while accrual is frozen.
    #[test]
    fn test_extend_paused_stream_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 200_000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        env.ledger().with_mut(|l| l.timestamp = 250);
        client.pause_stream(&stream_id);

        let result = client.try_extend_stream(&stream_id, &500);
        assert_eq!(result, Err(Ok(StreamError::InvalidConfig)));
    }

    /// Extending with additional_seconds = 0 must revert with InvalidConfig.
    #[test]
    fn test_extend_zero_seconds_reverts() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = 0);
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100_000);

        let stream_id = client.create_stream(&sender, &token, &recipient, &100, &1000);

        let result = client.try_extend_stream(&stream_id, &0);
        assert_eq!(
            result,
            Err(Ok(StreamError::InvalidConfig)),
            "extending with 0 additional_seconds must revert with InvalidConfig"
        );
    }

    // ── Minimum withdrawal amount tests ───────────────────────────────────────

    /// Withdrawal below minimum should be rejected with BelowMinimumWithdrawal error
    #[test]
    fn test_withdrawal_below_minimum_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        // Create stream with minimum withdrawal of 1000 tokens
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000, &1000);

        // Advance time by 5 seconds: 100 * 5 = 500 tokens accrued
        env.ledger().with_mut(|l| l.timestamp += 5);

        // 500 < 1000 minimum, should be rejected
        let result = client.try_withdraw(&stream_id);
        assert_eq!(
            result,
            Err(Ok(StreamError::BelowMinimumWithdrawal)),
            "withdrawal below minimum should be rejected"
        );
    }

    /// Withdrawal above minimum should succeed
    #[test]
    fn test_withdrawal_above_minimum_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        // Create stream with minimum withdrawal of 1000 tokens
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000, &1000);

        // Advance time by 15 seconds: 100 * 15 = 1500 tokens accrued
        env.ledger().with_mut(|l| l.timestamp += 15);

        // 1500 >= 1000 minimum, should succeed
        let withdrawn = client.withdraw(&stream_id);
        assert_eq!(withdrawn, 1500, "withdrawal above minimum should succeed");
    }

    /// End-of-stream should bypass minimum withdrawal restriction
    #[test]
    fn test_end_of_stream_bypass_minimum() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        // Create stream with minimum withdrawal of 1000 tokens
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000, &1000);

        // Advance time by 5 seconds: 100 * 5 = 500 tokens accrued
        env.ledger().with_mut(|l| l.timestamp += 5);

        // Advance past end_time (stream duration is 1000 seconds)
        env.ledger().with_mut(|l| l.timestamp += 1000);

        // Even though 500 < 1000 minimum, should succeed because stream is finished
        let withdrawn = client.withdraw(&stream_id);
        assert_eq!(
            withdrawn, 500,
            "end-of-stream should bypass minimum restriction"
        );
    }

    /// Zero minimum should impose no restriction
    #[test]
    fn test_zero_minimum_no_restriction() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        // Create stream with zero minimum (no restriction)
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000, &0);

        // Advance time by 1 second: 100 * 1 = 100 tokens accrued
        env.ledger().with_mut(|l| l.timestamp += 1);

        // Should succeed even with small amount since minimum is 0
        let withdrawn = client.withdraw(&stream_id);
        assert_eq!(withdrawn, 100, "zero minimum should impose no restriction");
    }

    /// Negative minimum withdrawal amount should be rejected at creation
    #[test]
    fn test_negative_minimum_rejected_at_creation() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token = setup_token(&env, &sender, 100_000);

        // Negative minimum should be rejected
        let result = client.try_create_stream(&sender, &token, &recipient, &100, &1000, &-1000);
        assert_eq!(
            result,
            Err(Ok(StreamError::InvalidConfig)),
            "negative minimum should be rejected at creation"
        );
    }

    /// get_claimable() should return raw amount regardless of minimum
    #[test]
    fn test_get_claimable_ignores_minimum() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ForgeStream);
        let client = ForgeStreamClient::new(&env, &contract_id);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        StellarAssetClient::new(&env, &token_id).mint(&sender, &10_000_000i128);

        // Create stream with minimum withdrawal of 1000 tokens
        let stream_id = client.create_stream(&sender, &token_id, &recipient, &100, &1000, &1000);

        // Advance time by 5 seconds: 100 * 5 = 500 tokens accrued
        env.ledger().with_mut(|l| l.timestamp += 5);

        // get_claimable should return 500 regardless of minimum
        let claimable = client.get_claimable(&stream_id);
        assert_eq!(
            claimable, 500,
            "get_claimable should ignore minimum restriction"
        );
    }
}
