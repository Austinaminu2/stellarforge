#![no_std]

//! # forge-vesting
//!
//! Token vesting contract with configurable cliff and linear release schedule.

pub mod contract;
pub mod errors;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test;

pub use crate::contract::ForgeVestingClient;
pub use crate::errors::VestingError;
pub use crate::types::{VestingConfig, VestingStatus, VestingSchedule};
