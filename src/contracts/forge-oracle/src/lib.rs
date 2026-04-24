#![no_std]

//! # forge-oracle
//!
//! Standardized price feed interface for Stellar/Soroban contracts.

pub mod contract;
pub mod errors;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test;

pub use crate::contract::ForgeOracleClient;
pub use crate::errors::OracleError;
pub use crate::types::{PriceData, PriceEntry};
pub use crate::storage::PricePair;
