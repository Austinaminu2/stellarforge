use soroban_sdk::{contracttype, Address};

#[contracttype]
#[derive(Clone)]
pub struct ScheduleConfig {
    pub token: Address,
    pub beneficiary: Address,
    pub admin: Address,
    pub total_amount: i128,
    pub start_time: u64,
    pub cliff_seconds: u64,
    pub duration_seconds: u64,
    pub cancelled: bool,
}

/// Status snapshot for a vesting schedule.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VestingStatus {
    pub schedule_id: u64,
    pub total_amount: i128,
    pub claimed: i128,
    pub vested: i128,
    pub claimable: i128,
    pub cliff_reached: bool,
    pub fully_vested: bool,
    pub cancelled: bool,
}
