use soroban_sdk::{contracttype, Address};

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
    /// `true` when `withdrawable > 0`.
    pub is_claimable: bool,
}
