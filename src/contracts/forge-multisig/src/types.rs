use soroban_sdk::{contracttype, Address};

/// A pending treasury transaction proposal.
#[contracttype]
#[derive(Clone)]
pub struct Proposal {
    /// Who proposed this transaction.
    pub proposer: Address,
    /// Destination address for the transfer.
    pub to: Address,
    /// Token address.
    pub token: Address,
    /// Amount to transfer.
    pub amount: i128,
    /// Number of approvals recorded for this proposal.
    pub approval_count: u32,
    /// Number of rejections recorded for this proposal.
    pub rejection_count: u32,
    /// Ledger timestamp when approval threshold was reached.
    pub approved_at: Option<u64>,
    /// Whether the proposal has been executed.
    pub executed: bool,
    /// Whether the proposal has been cancelled.
    pub cancelled: bool,
    /// Whether this is a native XLM transfer proposal.
    pub is_native: bool,
}
