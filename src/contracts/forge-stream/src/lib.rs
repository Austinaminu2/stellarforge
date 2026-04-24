#![no_std]

//! # forge-stream
//!
//! Real-time token streaming — pay-per-second token transfers on Soroban.

pub mod contract;
pub mod errors;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test;

pub use crate::contract::ForgeStreamClient;
pub use crate::errors::StreamError;
pub use crate::types::{Stream, StreamStatus};
