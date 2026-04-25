#![no_std]

//! # forge-errors
//!
//! Common error types shared across StellarForge contracts.
//!
//! This crate provides standardized error variants that are used across multiple
//! contracts in the StellarForge suite, reducing duplication and enabling
//! integrators to handle common error scenarios with shared logic.
//!
//! ## Common Error Variants
//!
//! - `AlreadyInitialized` - Contract has already been initialized
//! - `NotInitialized` - Contract has not been initialized
//! - `Unauthorized` - Caller is not authorized to perform the action
//!
//! ## Usage
//!
//! ```rust,ignore
//! use forge_errors::CommonError;
//! use soroban_sdk::{contracterror, contractimpl};
//!
//! #[contracterror]
//! #[derive(Copy, Clone, Debug, PartialEq)]
//! pub enum ContractError {
//!     #[from(CommonError)]
//!     Common(CommonError),
//!     // Contract-specific errors...
//! }
//! ```

use soroban_sdk::{contracterror, contracttype};

/// Common error variants shared across StellarForge contracts.
///
/// These errors represent fundamental failure modes that can occur
/// in multiple contract types, providing a consistent interface
/// for integrators building against multiple StellarForge contracts.
///
/// The error codes (1, 2, 3) match the existing conventions
/// used across all StellarForge contracts to maintain compatibility.
#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CommonError {
    /// Contract has already been initialized.
    ///
    /// This error occurs when an initialization function is called
    /// on a contract that has already been set up.
    AlreadyInitialized = 1,

    /// Contract has not been initialized.
    ///
    /// This error occurs when a function requiring initialization
    /// is called on a contract that hasn't been set up yet.
    NotInitialized = 2,

    /// Caller is not authorized to perform the action.
    ///
    /// This error occurs when the caller lacks the required
    /// permissions to execute the requested operation.
    Unauthorized = 3,
}
