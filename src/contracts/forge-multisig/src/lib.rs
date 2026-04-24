#![no_std]

//! # forge-multisig
//!
//! An N-of-M multisig treasury contract for Stellar/Soroban.

pub mod contract;
pub mod errors;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test;

pub use crate::contract::MultisigContractClient;
pub use crate::errors::MultisigError;
pub use crate::types::Proposal;
