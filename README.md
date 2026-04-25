# ⚒️ StellarForge

**Reusable Soroban smart contract primitives for the Stellar ecosystem.**

StellarForge is a collection of production-ready, well-tested Soroban contracts that developers can deploy directly or use as building blocks for more complex DeFi applications on Stellar.

---

## 📊 Contract Comparison
Developers evaluating StellarForge can use this table to quickly identify the right primitive for their specific use case.

| Contract | Use Case | Admin Required | Events Emitted | Timelock |
| :--- | :--- | :--- | :--- | :--- |
| [`forge-governor`](#forge-governor) | Governance | No (Auth-based) | `proposal_created`, `vote_cast`, `proposal_finalized` | Yes (Voting/Execution delay) |
| [`forge-multisig`](#forge-multisig) | Multisig Treasury | Yes (Owners) | `proposal_created`, `proposal_approved`, `proposal_executed` | Yes (Post-approval delay) |
| [`forge-oracle`](#forge-oracle) | Price Feed | Yes (Admin) | `price_updated`, `admin_transferred` | No |
| [`forge-stream`](#forge-stream) | Real-time Payments | No (Stream-specific) | `stream_created`, `withdrawn`, `stream_cancelled`, `stream_paused`, `stream_resumed` | No |
| [`forge-vesting`](#forge-vesting) | Token Vesting | Yes (Admin) | `vesting_initialized`, `claimed`, `vesting_cancelled`, `admin_transferred`, `beneficiary_changed` | Yes (Cliff period) |
| [`forge-vesting-factory`](#forge-vesting-factory) | Multi-beneficiary Vesting | Yes (Per-schedule Admin) | `schedule_created`, `claimed`, `schedule_cancelled` | Yes (Cliff period) |
---

## 🔒 Audit Status

**Current Status: UNAUDITED**

None of the StellarForge contracts have been formally audited as of this release. While these contracts are designed with security best practices and include comprehensive test coverage, they have not undergone independent security review.

### Contract Audit Status

| Contract | Audit Status | Last Updated | Notes |
| :--- | :--- | :--- | :--- |
| `forge-governor` | ❌ Unaudited | 2026-03-26 | Not recommended for production use without prior audit |
| `forge-multisig` | ❌ Unaudited | 2026-03-26 | Not recommended for production use without prior audit |
| `forge-oracle` | ❌ Unaudited | 2026-03-26 | Not recommended for production use without prior audit |
| `forge-stream` | ❌ Unaudited | 2026-03-26 | Not recommended for production use without prior audit |
| `forge-vesting` | ❌ Unaudited | 2026-03-26 | Not recommended for production use without prior audit |

### ⚠️ Production Use Disclaimer

**IMPORTANT:** These contracts are provided as-is for educational and development purposes. Before using in production:

1. **Conduct your own security audit** or engage a professional auditing firm
2. **Test extensively** on testnet with realistic scenarios
3. **Review the code** thoroughly for your specific use case
4. **Consider the risks** of unaudited smart contracts, including potential loss of funds

We are committed to obtaining formal audits before recommending production deployment. This section will be updated when audits are completed.

---

## 🏭 Real World Use Cases

- `forge-vesting`: Issue employee token grants with a one-year cliff and multi-year linear vesting so early hires are rewarded for long-term commitment, while investor lockups enforce even longer vesting before secondary-market liquidity.
- `forge-stream`: Pay contractors in real time for on-demand work with per-second streams that stop automatically at project completion, or implement subscription billing for SaaS users where tokens accrue continuously and can be withdrawn by the service provider.
- `forge-multisig`: Manage a DAO treasury for community-approved funding requests requiring multi-owner consent, or safeguard team operational funds with 2-of-3 and 3-of-5 approval workflows to prevent single-person spending.
- `forge-governor`: Coordinate protocol upgrades by routing proposals through a token-weighted voting process and enforcing execution delays, and tune parameters like fees or collateral ratios in a transparent governance flow.
- `forge-oracle`: Feed DEX price data into AMM pools for accurate swap pricing and slippage control, or provide collateral valuation updates for lending markets so borrowing power adjusts to live market conditions.

## 📜 Contract Details

### forge-vesting
Deploy tokens on a vesting schedule with an optional cliff period. Perfect for team allocations or advisor tokens.

* **Key Function:** `initialize(token, beneficiary, admin, total_amount, cliff_seconds, duration_seconds)`
* **Action:** `claim()` withdraws all currently unlocked tokens.
* **Security:** `cancel()` allows the admin to return unvested tokens if a contributor leaves.
* **Read functions:** Three query functions serve different audiences:
  * `get_vesting_schedule()` — public-facing; returns token, beneficiary, amounts, and timing. No admin address or cancellation state.
  * `get_status()` — public-facing; returns claimable amount, vested amount, cliff status, and pause state.
  * `get_config()` — admin tooling only; returns the full internal config including admin address and cancellation flag. Prefer the two functions above for UI integrations.

### forge-vesting-factory
A single-deployment factory that manages multiple vesting schedules. Eliminates the need to deploy a separate contract per beneficiary — ideal for companies vesting tokens for many employees or investors.

* **Key Function:** `create_schedule(token, beneficiary, admin, total_amount, cliff_seconds, duration_seconds) -> u64` — creates a new schedule and returns its `schedule_id`. Transfers tokens from admin into the factory on creation.
* **Action:** `claim(schedule_id)` — beneficiary withdraws all currently unlocked tokens for that schedule.
* **Security:** `cancel(schedule_id)` — admin cancels a schedule; vested tokens go to the beneficiary, unvested tokens return to the admin.
* **Read functions:**
  * `get_status(schedule_id)` — returns vested, claimed, claimable, cliff status, and cancellation state.
  * `get_schedule_count()` — returns the total number of schedules ever created.

### forge-stream
Pay-per-second token streams. Ideal for payroll, subscriptions, or real-time contractor payments.

* **Key Function:** `create_stream(sender, token, recipient, rate_per_second, duration_seconds, min_withdrawal_amount)`
* **Action:** `withdraw(stream_id)` allows the recipient to pull accrued tokens at any time.
* **Pause/Resume:** `pause_stream(stream_id)` and `resume_stream(stream_id)` allow senders to temporarily halt or restart token accrual.
* **`is_active` vs `is_claimable`:** `get_stream_status()` returns both fields. A finished stream has `is_active = false` and `is_finished = true`, but may still have `withdrawable > 0`. Always check `is_claimable` (or `withdrawable` directly) to determine whether tokens can be pulled — do not rely on `is_active` alone.

### forge-multisig
An N-of-M treasury requiring multiple owner approvals before funds move. Essential for DAO treasuries.

* **Key Function:** `propose(proposer, to, token, amount)`
* **Action:** `execute(executor, proposal_id)` transfers funds only after the configured timelock.
* **Duplicate Owners:** If duplicate addresses are provided during initialization, they are automatically deduplicated to ensure each owner is unique and counts only once toward the threshold.

### forge-governor
Token-weighted on-chain governance with configurable quorum and voting periods.

* **Key Function:** `propose(proposer, title, description)`
* **Action:** Supports token-weighted voting and automated execution after a passed proposal.

### forge-oracle
Admin-controlled price feeds with staleness protection for DeFi protocols.

* **Key Function:** `submit_price(base, quote, price)`
* **Security:** `get_price(base, quote)` reverts if data is older than the staleness threshold.

---

## 📡 Event Reference

The tables below are verified against the current contract code in `contracts/*/src/lib.rs`.

### forge-vesting

| Event Name | Trigger | Fields |
| :--- | :--- | :--- |
| `vesting_initialized` | Emitted by `initialize(...)` after the vesting config and claimed amount are stored. | `total_amount: i128`, `cliff_seconds: u64`, `duration_seconds: u64` |
| `claimed` | Emitted by `claim()` after the beneficiary's claimed amount is updated and vested tokens are transferred. | `beneficiary: Address`, `claimable: i128` |
| `vesting_cancelled` | Emitted by `cancel()` after the vesting is marked cancelled and any unvested tokens are returned to the admin. | `admin: Address`, `to_admin: i128`, `beneficiary: Address`, `to_beneficiary: i128` |
| `admin_transferred` | Emitted by `transfer_admin(new_admin)` after admin rights move to the new admin address. | `old_admin: Address`, `new_admin: Address` |
| `beneficiary_changed` | Emitted by `change_beneficiary(new_beneficiary)` after beneficiary rights move to the new address. | `old_beneficiary: Address`, `new_beneficiary: Address` |

### forge-stream

| Event Name | Trigger | Fields |
| :--- | :--- | :--- |
| `stream_created` | Emitted by `create_stream(...)` after the stream is stored and the active stream count is incremented. | `stream_id: u64`, `recipient: Address`, `rate_per_second: i128`, `duration_seconds: u64`, `min_withdrawal_amount: i128` |
| `withdrawn` | Emitted by `withdraw(stream_id)` after the withdrawn amount is updated and accrued tokens are transferred to the recipient. | `stream_id: u64`, `recipient: Address`, `withdrawable: i128` |
| `stream_cancelled` | Emitted by `cancel_stream(stream_id)` after the stream is marked cancelled and funds are paid out/refunded. | `stream_id: u64`, `withdrawable: i128`, `returnable: i128` |
| `stream_paused` | Emitted by `pause_stream(stream_id)` after the stream is marked paused. | `stream_id: u64` |
| `stream_resumed` | Emitted by `resume_stream(stream_id)` after paused time is accounted for and streaming resumes. | `stream_id: u64` |

### forge-multisig

| Event Name | Trigger | Fields |
| :--- | :--- | :--- |
| `proposal_created` | Emitted by `propose(...)` after the proposal is stored and the proposer is marked as approved. | `proposal_id: u64`, `proposer: Address`, `to: Address`, `token: Address`, `amount: i128` |
| `proposal_approved` | Emitted by `approve(...)` after an owner approves a proposal. | `proposal_id: u64`, `owner: Address`, `approval_count: u32` |
| `proposal_executed` | Emitted by `execute(...)` after a proposal is successfully executed. | `proposal_id: u64`, `executor: Address`, `to: Address`, `amount: i128` |

### forge-governor

| Event Name | Trigger | Fields |
| :--- | :--- | :--- |
| `proposal_created` | Emitted by `propose(...)` after the proposal is stored. | `proposal_id: u64`, `proposer: Address`, `vote_end: u64` |
| `vote_cast` | Emitted by `vote(...)` after a vote is successfully cast. | `proposal_id: u64`, `voter: Address`, `direction: VoteDirection`, `weight: i128` |
| `proposal_finalized` | Emitted by `finalize(...)` after a proposal is finalized (Passed or Failed). | `proposal_id: u64`, `votes_for: i128`, `votes_against: i128` |

### forge-oracle

| Event Name | Trigger | Fields |
| :--- | :--- | :--- |
| `price_updated` | Emitted by `submit_price(base, quote, price)` after the submitted price and update timestamp are written to storage. | `base: Symbol`, `quote: Symbol`, `price: i128`, `updated_at: u64` |
| `admin_transferred` | Emitted by `transfer_admin(new_admin)` after admin rights move to the new admin address. | `old_admin: Address`, `new_admin: Address` |

### forge-vesting-factory

| Event Name | Trigger | Fields |
| :--- | :--- | :--- |
| `schedule_created` | Emitted by `create_schedule(...)` after a new schedule is created. | `id: u64`, `total_amount: i128` |
| `claimed` | Emitted by `claim(...)` after tokens are claimed for a schedule. | `schedule_id: u64`, `claimable: i128` |
| `schedule_cancelled` | Emitted by `cancel(...)` after a schedule is cancelled. | `schedule_id: u64` |

---

## 🛠️ Prerequisites & Setup

Soroban is Stellar’s smart contract platform, built for performance and developer-friendly Rust tooling. Learn more in the [official docs](https://developers.stellar.org/docs/smart-contracts/overview).

To build and test these contracts, you will need the following tools:

#### Rust Requirements
- **Rust Edition:** 2021
- **Target:** `wasm32v1-none` (v1 instruction set recommended for Soroban)

```bash
rustup target add wasm32v1-none
```

#### CLI Installation
The `stellar-cli` is essential for building, deploying, and interacting with Soroban contracts. **v25.2.0 or higher** is recommended.

```bash
cargo install --locked stellar-cli
```

#### Funding Testnet Accounts
Before deploying, you'll need a funded testnet account. You can generate and fund one easily:

```bash
stellar keys generate <identity_name> --network testnet --fund
```

### Using Make (Recommended)
This project includes a Makefile with common development commands:

| Command | Description |
| :--- | :--- |
| `make build` | Build all workspace crates |
| `make test` | Run all tests |
| `make lint` | Run clippy linter with deny warnings |
| `make fmt` | Format code |
| `make check` | Run fmt + lint + test in sequence |
| `make clean` | Clean build artifacts |

### Build all contracts

```bash
make build
# or manually:
cargo build --workspace
stellar contract build
```

### Run all tests

```bash
make test
# or manually:
cargo test --workspace
```

### Run a specific contract's tests

```bash
cargo test -p forge-vesting
cargo test -p forge-stream
cargo test -p forge-multisig
cargo test -p forge-governor
cargo test -p forge-oracle
```

---

## 🚀 Testnet Deployment

Ready to experiment with StellarForge contracts? We've deployed all contracts to Stellar testnet for easy testing and evaluation.

**Quick Start:**
```bash
# Example: Query a price from the oracle
stellar contract invoke \
  --id CDLZFC3SYJYDZT7K67VZ7SHPY775YXK4XZ4Z4Z4Z4Z4Z4Z4Z4Z4Z6 \
  --network testnet \
  -- \
  get_price --base XLM --quote USDC
```

See [`docs/testnet.md`](docs/testnet.md) for:
- Deployed contract addresses
- Network configuration (passphrase, RPC URL)
- Example commands for each contract
- Instructions for deploying your own instances

---

## 🔗 Composability Guide

A step-by-step walkthrough showing how to combine multiple StellarForge contracts is available in [`docs/composability.md`](docs/composability.md).

The guide covers a full DAO scenario using `forge-governor`, `forge-multisig`, and `forge-stream` together, plus common composability patterns for other contract combinations.

---

## 📐 State Diagrams

Visual lifecycle documentation for stateful contracts is available in [`docs/state-diagrams.md`](docs/state-diagrams.md).

| Contract | States |
| :--- | :--- |
| `forge-vesting` | Active → Cliff Reached → Fully Vested → Cancelled |
| `forge-stream` | Active → Finished / Cancelled |
| `forge-governor` | Active → Passed / Failed → Executed |

---

## 📖 Glossary

Understanding these key terms will help you work with StellarForge contracts more effectively:

**Cliff** — A waiting period before any tokens become available. In [vesting](#forge-vesting), no tokens can be claimed until the cliff period expires, even though time is accruing. Common for employee token grants (e.g., 1-year cliff).

**Vesting** — The gradual release of tokens over time according to a predefined schedule. After the cliff (if any), tokens unlock linearly until the full amount is available. See [`forge-vesting`](#forge-vesting).

**Stream** — Continuous, per-second token flow from sender to recipient. Unlike vesting, streams have no cliff and tokens accrue in real-time. Perfect for payroll or subscriptions. See [`forge-stream`](#forge-stream).

**Timelock** — A mandatory delay between approval and execution of an action. Used in [`forge-multisig`](#forge-multisig) (post-approval delay) and [`forge-governor`](#forge-governor) (voting + execution delays) to allow stakeholders time to react.

**Quorum** — The minimum amount of voting power (token weight) required for a governance proposal to be valid. In [`forge-governor`](#forge-governor), proposals fail if they don't meet quorum, even with majority support.

**Multisig** — Short for "multi-signature." A wallet or treasury that requires M-of-N owners to approve transactions before execution. See [`forge-multisig`](#forge-multisig).

**Threshold** — The minimum number of approvals required in a multisig setup. For example, a 3-of-5 multisig has a threshold of 3, meaning 3 out of 5 owners must approve.

**Price Feed** — A data source providing asset price information to smart contracts. [`forge-oracle`](#forge-oracle) allows admins to submit prices for DeFi protocols to consume.

**Staleness** — How outdated price data is. In [`forge-oracle`](#forge-oracle), the staleness threshold defines the maximum age of price data before it's considered invalid and queries revert.

**Staleness Threshold** — The maximum time (in seconds) that price data remains valid in [`forge-oracle`](#forge-oracle). After this period, the data is considered stale and cannot be used.

---

## Design Principles

- **No unsafe code** — all contracts are `#![no_std]` and fully safe Rust
- **Minimal dependencies** — only `soroban-sdk`, no external crates
- **Comprehensive tests** — every error path and state transition is covered
- **Clear error types** — typed error enums with descriptive variants
- **Event emission** — all state changes emit events for off-chain indexing

---

## 📦 Versioning

StellarForge contracts follow [Semantic Versioning](https://semver.org/) (SemVer) to help you manage upgrades safely.

### Version Format: MAJOR.MINOR.PATCH

- **MAJOR** — Breaking changes that require action from developers
- **MINOR** — New features that are backward-compatible
- **PATCH** — Bug fixes and internal improvements

### What Counts as a Breaking Change?

Breaking changes require a MAJOR version bump and include:

- **Interface Changes** — Modifying function signatures, parameter types, or return values
- **Storage Layout Changes** — Altering contract storage structure in ways that break existing deployments
- **Behavior Changes** — Changing core logic that affects expected outcomes (e.g., calculation methods, state transitions)
- **Error Changes** — Removing or renaming error types that external code may depend on
- **Event Changes** — Modifying event structures or removing events that indexers rely on

### Non-Breaking Changes

These are safe and result in MINOR or PATCH bumps:

- Adding new optional functions
- Adding new events (without modifying existing ones)
- Internal optimizations that don't affect external behavior
- Bug fixes that restore intended behavior
- Documentation improvements

### Upgrade Recommendations

- **Review the [CHANGELOG.md](CHANGELOG.md)** before upgrading to understand what changed
- **Test thoroughly** on testnet before deploying MAJOR version upgrades to production
- **Pin versions** in your deployment scripts to avoid unexpected changes
- **Subscribe to releases** on GitHub to stay informed about security patches

### Contract Independence

Each contract in StellarForge is versioned independently. A breaking change in `forge-vesting` does not affect `forge-stream` versions.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup instructions, code style requirements, and the pull request process.

## 🆘 Getting Help

Stuck on something? Here's where to go:

- **Bug reports** — Open an issue on [GitHub Issues](https://github.com/Austinaminu2/stellarforge/issues). Please include a minimal reproduction and the contract name.
- **Questions & ideas** — Start a thread in [GitHub Discussions](https://github.com/Austinaminu2/stellarforge/discussions). We have dedicated spaces for Q&A, ideas, show-and-tell, and general chat.

**Response time:** This is a community-maintained project. Maintainers aim to respond to issues and discussions within a few business days, but there are no guaranteed SLAs. For faster help, check if a similar issue or discussion already exists before opening a new one.

## Community & Discussions

Have a question, idea, or something to share? Join the conversation in [GitHub Discussions](https://github.com/soma-enyi/stellarforge/discussions) — we have dedicated spaces for Q&A, ideas, show-and-tell, and general chat.

---

<!-- WASM-SIZES-START -->
## ⚙️ WASM Binary Sizes

> Sizes are in bytes. Run `./scripts/update-wasm-sizes.sh` to regenerate after rebuilding contracts.

| Contract | WASM Size (bytes) | WASM Size (optimized) |
| :--- | ---: | ---: |
| `forge-governor` | — | — |
| `forge-multisig` | — | — |
| `forge-oracle` | — | — |
| `forge-stream` | — | — |
| `forge-vesting` | — | — |

> Run `./scripts/update-wasm-sizes.sh` after building to populate these values.

### Optimizing with wasm-opt

`wasm-opt` is part of the [Binaryen](https://github.com/WebAssembly/binaryen) toolchain.
The `-Oz` flag instructs the optimizer to minimize binary size as aggressively as possible,
trading compilation time for the smallest possible output.

#### Install wasm-opt

```bash
# macOS (Homebrew)
brew install binaryen

# npm (cross-platform)
npm install -g binaryen

# Direct binary download
# https://github.com/WebAssembly/binaryen/releases
```

#### Run optimization

```bash
wasm-opt -Oz \
  target/wasm32v1-none/release/forge_governor.wasm \
  -o target/wasm32v1-none/release/forge_governor.wasm
```

Replace `forge_governor` with the snake_case name of the contract you want to optimize.
<!-- WASM-SIZES-END -->

---

## License

MIT
