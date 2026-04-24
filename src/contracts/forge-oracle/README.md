# Forge Oracle

## Resource Usage

> **Note:** Resource usage estimates are approximate and may vary based on contract state and input sizes. Run `stellar contract invoke` with `--cost` flag to measure actual usage for your specific use case.

### Function Resource Estimates

| Function | CPU Instructions | Memory (bytes) | Ledger Reads | Ledger Writes | Notes |
| :--- | :---: | :---: | :---: | :---: | :--- |
| `initialize` | ~40,000 | ~1,500 | 0 | 1 | Stores admin address |
| `submit_price` | ~55,000 | ~2,000 | 1 | 1 | Most expensive - validates admin, updates price |
| `get_price` | ~25,000 | ~1,500 | 1 | 0 | Validates staleness, returns price data |
| `get_price_data` | ~20,000 | ~1,200 | 1 | 0 | Read-only query (no staleness check) |
| `transfer_admin` | ~35,000 | ~1,500 | 1 | 1 | Transfers admin rights |

### Most Expensive Functions

1. **`submit_price`** (~55,000 CPU instructions)
   - Why: Validates admin authorization, encodes price data, and updates ledger storage
   - Optimization tip: Batch price updates for multiple pairs when possible

2. **`get_price`** (~25,000 CPU instructions)
   - Why: Performs staleness validation and decodes price data
   - Optimization tip: Cache frequently accessed prices in your application

### Cost Estimation

Soroban charges fees based on:
- **CPU Instructions:** ~0.0001 XLM per 10,000 instructions
- **Memory:** ~0.00001 XLM per byte
- **Ledger Entries:** ~0.001 XLM per read/write

**Example:** Submitting a price update costs approximately:
- CPU: 55,000 instructions × 0.0001 XLM / 10,000 = 0.00055 XLM
- Memory: 2,000 bytes × 0.00001 XLM = 0.02 XLM
- Ledger: 2 operations × 0.001 XLM = 0.002 XLM
- **Total:** ~0.023 XLM per price update

---

## Known Limitations

- Single admin
- No decentralized feed