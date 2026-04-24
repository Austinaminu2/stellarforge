#![no_std]

//! # forge-vesting-factory
//!
//! A factory contract that manages multiple vesting schedules in a single deployment.

pub mod contract;
pub mod errors;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test;

pub use crate::contract::ForgeVestingFactoryClient;
pub use crate::errors::FactoryError;
pub use crate::types::{ScheduleConfig, VestingStatus};
