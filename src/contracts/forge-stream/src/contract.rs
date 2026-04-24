use soroban_sdk::{contract, contractimpl, token, Address, Env, Symbol};
use crate::storage::DataKey;
use crate::types::{Stream, StreamStatus};
use crate::errors::StreamError;

#[contract]
pub struct ForgeStream;

#[contractimpl]
impl ForgeStream {
    /// Create a new token stream.
    pub fn create_stream(
        env: Env,
        sender: Address,
        token: Address,
        recipient: Address,
        rate_per_second: i128,
        duration_seconds: u64,
        min_withdrawal_amount: i128,
    ) -> Result<u64, StreamError> {
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
        let total = rate_per_second
            .checked_mul(duration_seconds as i128)
            .ok_or(StreamError::InvalidConfig)?;

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

    /// Withdraw accrued tokens.
    pub fn withdraw(env: Env, stream_id: u64) -> Result<i128, StreamError> {
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

    // ── Internal ──────────────────────────────────────────────────────────────

    fn compute_streamed(stream: &Stream, now: u64) -> i128 {
        if stream.cancelled {
            return stream.streamed_at_cancel;
        }
        let effective_now = now.min(stream.end_time);
        let elapsed = effective_now.saturating_sub(stream.start_time);
        let active_elapsed = elapsed.saturating_sub(stream.total_paused_time);
        stream.rate_per_second * active_elapsed as i128
    }

    fn active_streams_count(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ActiveStreamsCount)
            .unwrap_or(0)
    }

    fn set_active_streams_count(env: &Env, count: u64) {
        env.storage()
            .instance()
            .set(&DataKey::ActiveStreamsCount, &count);
    }
}
