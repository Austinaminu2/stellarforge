# Forge Stream

## Storage Strategy

`forge-stream` uses two Soroban storage tiers. The rule of thumb: use
**persistent** for data that must survive beyond a single transaction or
contract instance TTL; use **instance** for small, frequently-accessed
scalars that are always read together with the contract instance.

| `DataKey` variant           | Storage type | Rationale                                                                            |
| :-------------------------- | :----------- | :----------------------------------------------------------------------------------- |
| `Stream(u64)`               | `persistent` | Stream data must outlive the instance TTL while tokens remain unclaimed              |
| `NextId`                    | `instance`   | Small scalar always read on `create_stream`; co-located with instance for efficiency |
| `ActiveStreamsCount`        | `instance`   | Updated on every create/cancel/finish; always accessed with other instance data      |
| `SenderStreams(Address)`    | `persistent` | Grows with each stream; must survive beyond instance TTL for historical lookups      |
| `RecipientStreams(Address)` | `persistent` | Same rationale as `SenderStreams`                                                    |

When adding a new `DataKey` variant, choose the storage type using this
checklist:

- Does the data need to survive after the contract instance TTL expires? → **persistent**
- Is it a small scalar read on almost every call? → **instance**
- Is it keyed per-user or per-stream (unbounded growth)? → **persistent**

---

## Resource Usage

> **Note:** Resource usage estimates are approximate and may vary based on contract state and input sizes. Run `stellar contract invoke` with `--cost` flag to measure actual usage for your specific use case.

### Function Resource Estimates

| Function           | CPU Instructions | Memory (bytes) | Ledger Reads | Ledger Writes | Notes                                             |
| :----------------- | :--------------: | :------------: | :----------: | :-----------: | :------------------------------------------------ |
| `create_stream`    |     ~85,000      |     ~3,500     |      2       |       3       | Most expensive - validates inputs, creates stream |
| `withdraw`         |     ~65,000      |     ~2,500     |      3       |       2       | Calculates accrued amount, transfers tokens       |
| `cancel_stream`    |     ~70,000      |     ~3,000     |      3       |       2       | Calculates final amounts, refunds/pays out        |
| `pause_stream`     |     ~45,000      |     ~2,000     |      2       |       1       | Marks stream as paused                            |
| `resume_stream`    |     ~50,000      |     ~2,200     |      2       |       1       | Adjusts for paused time, resumes stream           |
| `get_stream`       |     ~20,000      |     ~1,500     |      2       |       0       | Read-only query                                   |
| `get_withdrawable` |     ~25,000      |     ~1,800     |      2       |       0       | Calculates current accrued amount                 |

### Most Expensive Functions

1. **`create_stream`** (~85,000 CPU instructions)
    - Why: Validates sender/recipient, encodes stream parameters, creates multiple storage entries
    - Optimization tip: Reuse stream configurations for recurring payments

2. **`cancel_stream`** (~70,000 CPU instructions)
    - Why: Calculates final amounts, handles partial refunds, updates multiple storage entries
    - Optimization tip: Use pause/resume instead of cancel/recreate for temporary stops

### Cost Estimation

Soroban charges fees based on:

- **CPU Instructions:** ~0.0001 XLM per 10,000 instructions
- **Memory:** ~0.00001 XLM per byte
- **Ledger Entries:** ~0.001 XLM per read/write

**Example:** Creating a stream costs approximately:

- CPU: 85,000 instructions × 0.0001 XLM / 10,000 = 0.00085 XLM
- Memory: 3,500 bytes × 0.00001 XLM = 0.035 XLM
- Ledger: 5 operations × 0.001 XLM = 0.005 XLM
- **Total:** ~0.041 XLM per stream creation

---

## Known Limitations

- No mid-stream rate changes

## Overflow Protection

`forge-stream` guards against i128 multiplication overflow in two places:

- **`create_stream()`** — `rate_per_second * duration_seconds` uses `checked_mul` and returns `InvalidConfig` if the product would exceed `i128::MAX`. This prevents a stream from being created with a total that cannot be represented.
- **`compute_streamed()`** — `rate_per_second * effective_elapsed` uses `checked_mul` and falls back to `total` (the stream's maximum payout) if overflow would occur. The result is also capped at `total`, so callers always receive a value in `[0, total]` regardless of elapsed time or rate magnitude.
