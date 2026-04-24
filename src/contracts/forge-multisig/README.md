# Forge Multisig

## Resource Usage

> **Note:** Resource usage estimates are approximate and may vary based on contract state and input sizes. Run `stellar contract invoke` with `--cost` flag to measure actual usage for your specific use case.

### Function Resource Estimates

| Function       | CPU Instructions | Memory (bytes) | Ledger Reads | Ledger Writes | Notes                                                                  |
| :------------- | :--------------: | :------------: | :----------: | :-----------: | :--------------------------------------------------------------------- |
| `initialize`   |     ~60,000      |     ~2,500     |      0       |       3       | Stores owners, threshold, and timelock                                 |
| `propose`      |     ~70,000      |     ~3,000     |      2       |       2       | Validates proposer, creates proposal                                   |
| `approve`      |     ~50,000      |     ~2,000     |      3       |       1       | Records approval, checks threshold                                     |
| `execute`      |     ~90,000      |     ~3,500     |      4       |       2       | Most expensive - validates threshold, checks timelock, transfers funds |
| `get_proposal` |     ~20,000      |     ~1,500     |      2       |       0       | Read-only query                                                        |
| `is_approved`  |     ~15,000      |     ~1,000     |      1       |       0       | Read-only query                                                        |

### Most Expensive Functions

1. **`execute`** (~90,000 CPU instructions)
    - Why: Validates approval threshold, checks timelock expiration, and executes token transfer
    - Optimization tip: Ensure all required approvals are collected before attempting execution

2. **`propose`** (~70,000 CPU instructions)
    - Why: Validates proposer eligibility and creates new proposal storage
    - Optimization tip: Batch similar proposals when possible

### Cost Estimation

Soroban charges fees based on:

- **CPU Instructions:** ~0.0001 XLM per 10,000 instructions
- **Memory:** ~0.00001 XLM per byte
- **Ledger Entries:** ~0.001 XLM per read/write

**Example:** Executing a multisig proposal costs approximately:

- CPU: 90,000 instructions × 0.0001 XLM / 10,000 = 0.0009 XLM
- Memory: 3,500 bytes × 0.00001 XLM = 0.035 XLM
- Ledger: 6 operations × 0.001 XLM = 0.006 XLM
- **Total:** ~0.042 XLM per execution

---

## API Design

All `get_*` query functions that look up a resource by ID return `Result<T, Error>` rather than `Option<T>`. For example, `get_proposal()` returns `Err(MultisigError::ProposalNotFound)` when no proposal exists for the given ID. This is consistent with `forge-governor` and other Forge contracts, so integrators can handle missing resources uniformly across the suite.

---

## Known Limitations

- Owner list is fixed after initialization
- **Threshold is immutable after initialization.** If a future version adds `update_threshold()`, the `approved_at` logic in `approve()` must be reviewed carefully. Lowering the threshold could cause a proposal to retroactively meet the new threshold at an earlier `approved_at` timestamp, potentially allowing premature execution. Any threshold mutation must preserve the original `approved_at` timestamp or implement additional safeguards.

## Native XLM Support

`forge-multisig` supports native XLM transfers alongside regular Soroban token transfers.

**How it works:**
On Soroban, native XLM is accessed through the Stellar Asset Contract (SAC) for the native asset. The SAC exposes the same `token::Client` interface as any other Soroban token, so execution is handled identically — the only difference is the proposal is flagged with `is_native: true` for clarity.

**Creating a native XLM proposal:**

```text
// Transfer 10 XLM (100_000_000 stroops) to a recipient
let id = client.propose_xlm(&owner, &recipient, &xlm_sac_address, &100_000_000);
```

**Approval and execution** work exactly the same as token proposals — use `approve()` and `execute()` with the returned proposal ID.

**Getting the native XLM SAC address:**

- Testnet/Mainnet: use `stellar contract id asset --asset native --network <network>`
- In tests: register a SAC with `env.register_stellar_asset_contract_v2(...)` and mint XLM into the contract with `StellarAssetClient::mint()`

**Identifying proposal type:**

```text
let proposal = client.get_proposal(&id).unwrap();
if proposal.is_native {
    // native XLM transfer
} else {
    // Soroban token transfer
}
```
