use soroban_sdk::{contracttype, Address};

#[contracttype]
pub enum DataKey {
    /// Per-stream data (token, sender, recipient, rate, timestamps, state).
    Stream(u64),
    /// Monotonically increasing counter used to assign the next stream ID.
    NextId,
    /// Count of streams that are currently active (not cancelled/finished).
    ActiveStreamsCount,
    /// List of stream IDs created by a given sender address.
    SenderStreams(Address),
    /// List of stream IDs where a given address is the recipient.
    RecipientStreams(Address),
}
