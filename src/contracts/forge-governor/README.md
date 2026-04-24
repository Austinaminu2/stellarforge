# Forge Governor

## Resource Usage

> **Note:** Resource usage estimates are approximate and may vary based on contract state and input sizes. Run `stellar contract invoke` with `--cost` flag to measure actual usage for your specific use case.

### Function Resource Estimates

| Function       | CPU Instructions | Memory (bytes) | Ledger Reads | Ledger Writes | Notes                                                                 |
| :------------- | :--------------: | :------------: | :----------: | :-----------: | :-------------------------------------------------------------------- |
| `initialize`   |     ~50,000      |     ~2,000     |      0       |       2       | Stores admin and config                                               |
| `propose`      |     ~80,000      |     ~3,000     |      2       |       3       | Validates proposer, creates proposal                                  |
| `vote`         |     ~60,000      |     ~2,500     |      3       |       2       | Updates vote tally, records voter                                     |
| `execute`      |     ~100,000     |     ~3,500     |      4       |       2       | Most expensive - validates quorum, checks timelock, executes proposal |
| `get_proposal` |     ~20,000      |     ~1,500     |      2       |       0       | Read-only query                                                       |
| `get_vote`     |     ~15,000      |     ~1,000     |      1       |       0       | Read-only query                                                       |

### Most Expensive Functions

1. **`execute`** (~100,000 CPU instructions)
    - Why: Validates quorum requirements, checks timelock expiration, and executes the proposal action
    - Optimization tip: Ensure proposals are well-formed before submission to avoid failed executions

2. **`propose`** (~80,000 CPU instructions)
    - Why: Validates proposer eligibility, creates new proposal storage, and initializes vote tracking
    - Optimization tip: Reuse proposal templates for similar governance actions

### Cost Estimation

Soroban charges fees based on:

- **CPU Instructions:** ~0.0001 XLM per 10,000 instructions
- **Memory:** ~0.00001 XLM per byte
- **Ledger Entries:** ~0.001 XLM per read/write

**Example:** Executing a proposal costs approximately:

- CPU: 100,000 instructions × 0.0001 XLM / 10,000 = 0.001 XLM
- Memory: 3,500 bytes × 0.00001 XLM = 0.035 XLM
- Ledger: 6 operations × 0.001 XLM = 0.006 XLM
- **Total:** ~0.042 XLM per execution

---

## Known Limitations

- No vote delegation
- **Front-running protection:** The `initialize()` function requires authorization from the `admin` address specified in the `GovernorConfig`. This prevents an attacker from monitoring the mempool and front-running the deployer's initialization with a malicious configuration (e.g., quorum = 1, timelock = 0). The admin address must authorize the initialization call via `require_auth()`.

## Pending Proposal Performance

- `propose()` appends the new ID to `ActiveProposals` and records its slot in `ActiveProposalIndex`, so activation remains O(1).
- `cancel_proposal()`, `finalize()`, and the `execute()` cleanup path remove active IDs with swap-remove plus the index map, so removal remains O(1) even when many proposals are active.
- `get_pending_proposals()` reads only the current `ActiveProposals` vector and filters out expired or cancelled entries, so enumeration cost is O(n_active), not O(total_proposals_created).
- `get_pending_proposals()` does not guarantee sorted output; callers should treat the returned IDs as an unordered active set.

---

## TTL Strategy

Stellar persistent storage entries expire when their TTL (time-to-live) reaches zero. Without explicit TTL management, expired entries silently return `false` from `has()`, which would break double-vote protection — an expired `Vote` entry would allow a voter to cast a second vote on the same proposal.

### Constants

| Constant | Ledgers | Approx. time | Applied to |
| :--- | :---: | :---: | :--- |
| `INSTANCE_TTL_THRESHOLD` | 17 280 | ~1 day | Contract instance (bump trigger) |
| `INSTANCE_TTL_EXTEND` | 34 560 | ~2 days | Contract instance (bump target) |
| `PROPOSAL_TTL_EXTEND` | 1 036 800 | ~60 days | `DataKey::Proposal` entries |
| `VOTE_TTL_EXTEND` | 1 036 800 | ~60 days | `DataKey::Vote` entries |

### Rationale

- `DataKey::Proposal` entries are extended on every write (`propose`, `vote`, `finalize`, `execute`) to ensure the proposal remains readable throughout its full lifecycle.
- `DataKey::Vote` entries are extended immediately after being written in `vote()`. The 60-day ceiling covers any realistic `voting_period + timelock_delay` configuration with a large safety buffer.
- The contract instance TTL is bumped on every mutating call using a threshold/extend pattern so it is only extended when it is actually close to expiry, avoiding unnecessary ledger writes.

---

## Tie-Breaking Behaviour

When `finalize()` is called after the voting period ends, the contract requires a **strict majority** to pass a proposal:

```
votes_for > votes_against
```

If `votes_for == votes_against` (a tie), the proposal resolves to **`Failed`**. There is no mechanism that breaks a tie in favour of the proposer, the quorum, or any other party. This behaviour is deterministic and unconditional.

### Summary table

| Condition                                                | Outcome  |
| :------------------------------------------------------- | :------- |
| `total_votes < quorum`                                   | `Failed` |
| `total_votes >= quorum` and `votes_for > votes_against`  | `Passed` |
| `total_votes >= quorum` and `votes_for == votes_against` | `Failed` |
| `total_votes >= quorum` and `votes_for < votes_against`  | `Failed` |

### Rationale

Requiring a strict majority means the status quo is preserved on a tie. A proposal must actively win — not merely draw — to advance to execution. This is the safest default for an on-chain treasury governor where an ambiguous outcome should never result in a token transfer.
