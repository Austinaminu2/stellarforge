use soroban_sdk::{contracttype, Address};

#[contracttype]
#[derive(Clone)]
pub struct VestingConfig {
    /// Token contract address
    pub token: Address,
    /// Beneficiary who receives vested tokens
    pub beneficiary: Address,
    /// Admin who can cancel vesting
    pub admin: Address,
    /// Total tokens to vest
    pub total_amount: i128,
    /// Timestamp when vesting starts
    pub start_time: u64,
    /// Seconds before any tokens unlock
    pub cliff_seconds: u64,
    /// Total vesting duration in seconds
    pub duration_seconds: u64,
    /// Whether vesting has been cancelled
    pub cancelled: bool,
    /// Whether vesting is currently paused
    pub paused: bool,
    /// Ledger timestamp when vesting was paused (None if not paused)
    pub paused_at: Option<u64>,
}

#[contracttype]
#[derive(Clone)]
pub struct VestingStatus {
    pub total_amount: i128,
    pub claimed: i128,
    pub vested: i128,
    pub claimable: i128,
    pub cliff_reached: bool,
    pub fully_vested: bool,
    pub paused: bool,
}

/// Vesting schedule configuration (excludes admin and cancellation state).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VestingSchedule {
    /// Token contract address
    pub token: Address,
    /// Beneficiary who receives vested tokens
    pub beneficiary: Address,
    /// Total tokens to vest
    pub total_amount: i128,
    /// Seconds before any tokens unlock
    pub cliff_seconds: u64,
    /// Total vesting duration in seconds
    pub duration_seconds: u64,
    /// Timestamp when vesting starts
    pub start_time: u64,
}
