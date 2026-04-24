#![no_std]

//! # forge-governor
//!
//! On-chain governance with token-weighted voting for Stellar/Soroban.

pub mod contract;
pub mod errors;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test;

pub use crate::contract::GovernorContractClient;
pub use crate::errors::GovernorError;
pub use crate::types::{GovernorConfig, Proposal, ProposalState, VoteDirection, VoteTally};
