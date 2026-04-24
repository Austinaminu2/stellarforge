# Forge Vesting

## Resource Usage

> **Note:** Resource usage estimates are approximate and may vary based on contract state and input sizes. Run `stellar contract invoke` with `--cost` flag to measure actual usage for your specific use case.

### Function Resource Estimates

| Function | CPU Instructions | Memory (bytes) | Ledger Reads | Ledger Writes | Notes |
| :--- | :---: | :---: | :---: | :---: | :--- |
| `initialize` | ~75,000 | ~3,000 | 1 | 3 | Most expensive - validates inputs, creates vesting schedule |
| `claim` | ~60,000 | ~2,500 | 2 | 2 | Calculates vested amount, transfers tokens |
| `cancel` | ~55,000 | ~2,200 | 2 | 2 | Calculates unvested amount, refunds to admin |
| `transfer_admin` | ~35,000 | ~1,500 | 1 | 1 | Transfers admin rights |
| `get_vesting` | ~20,000 | ~1,500 | 2 | 0 | Read-only query |
| `get_claimable` | ~25,000 | ~1,800 | 2 | 0 | Calculates current claimable amount |

### Most Expensive Functions

1. **`initialize`** (~75,000 CPU instructions)
   - Why: Validates beneficiary/admin, encodes vesting parameters, creates multiple storage entries
   - Optimization tip: Reuse vesting configurations for similar grants

2. **`claim`** (~60,000 CPU instructions)
   - Why: Calculates vested amount based on time, updates claimed amount, transfers tokens
   - Optimization tip: Batch claims for multiple vesting schedules when possible

### Cost Estimation

Soroban charges fees based on:
- **CPU Instructions:** ~0.0001 XLM per 10,000 instructions
- **Memory:** ~0.00001 XLM per byte
- **Ledger Entries:** ~0.001 XLM per read/write

**Example:** Initializing a vesting schedule costs approximately:
- CPU: 75,000 instructions × 0.0001 XLM / 10,000 = 0.00075 XLM
- Memory: 3,000 bytes × 0.00001 XLM = 0.03 XLM
- Ledger: 4 operations × 0.001 XLM = 0.004 XLM
- **Total:** ~0.035 XLM per vesting initialization

---

## Known Limitations

- No partial cancellation
- Single beneficiary only