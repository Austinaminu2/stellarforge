use soroban_sdk::contracttype;

#[contracttype]
pub enum DataKey {
    /// Per-schedule configuration, keyed by schedule_id.
    Schedule(u64),
    /// Cumulative claimed amount per schedule, keyed by schedule_id.
    Claimed(u64),
    /// Monotonically increasing schedule counter.
    ScheduleCount,
}
