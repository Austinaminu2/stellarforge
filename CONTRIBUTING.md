# Contributing to StellarForge

Thank you for your interest in contributing to StellarForge! We welcome contributions that help make this collection of Soroban smart contract primitives more robust and easier to use.

## 🛠️ Prerequisites

To contribute to this project, you will need:
- **Rust:** Latest stable version (2021 edition)
- **Target:** `wasm32v1-none`
- **Stellar CLI:** v25.2.0 or higher
- **Make:** Optional, but recommended for running development commands

## 🚀 Getting Started

1. **Fork the repository** on GitHub.
2. **Clone your fork** locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/stellarforge.git
   cd stellarforge
   ```
3. **Set up the pre-commit hook** (recommended):
   ```bash
   cp src/scripts/pre-commit .git/hooks/pre-commit
   chmod +x .git/hooks/pre-commit
   ```

### Pre-commit Hook (Optional but Recommended)

We provide a git pre-commit hook that automatically checks code formatting and linting before each commit. This helps catch issues early.

By default, the hook runs `cargo fmt` and `cargo clippy`. To also run the full test suite before each commit, set the `FORGE_PRECOMMIT_TESTS` environment variable to `1`:

```bash
# Run tests on this commit only
FORGE_PRECOMMIT_TESTS=1 git commit -m "your message"
```

## 📦 Shared Error Crate (forge-errors)

When adding new common error variants to `forge-errors`:

1. **Consider if the error is truly common** across multiple contracts.
2. **Add descriptive documentation** to the variant in `crates/forge-errors/src/lib.rs`.
3. **Update error codes** to avoid conflicts with existing variants.
4. **Test the change** across all affected contracts.

## 📜 Development Workflow

### Building
Build all contracts in the workspace:
```bash
make build
# or
cargo build --workspace
```

### Testing
Run the full test suite:
```bash
make test
# or
cargo test --workspace
```

### Linting & Formatting
Ensure your code follows the project's style:
```bash
make check
# which runs:
# cargo fmt --all -- --check
# cargo clippy --all-targets -- -D warnings
```

## 🏗️ Pull Request Process

1. Create a new branch for your feature or bug fix.
2. Ensure all tests pass and the code is correctly formatted.
3. Update the documentation (`README.md`, `docs/`) if you've changed contract interfaces or added new features.
4. Submit a Pull Request targeting the `main` branch.
5. Use the provided PR template to describe your changes and testing.

## 🏷️ Issue Labels

- `good first issue` — Great for newcomers!
- `bug` — Something isn't working correctly.
- `enhancement` — New features or improvements.
- `documentation` — Improvements to the docs.

## 🔒 Security

Security is our top priority. If you discover a security vulnerability:

1. **Do NOT open a public issue.**
2. **Email us privately**: security@stellarforge.org
3. **Include details**: Impact, reproduction steps, affected versions.
4. **Allow time for response**: We'll acknowledge within 48 hours.

## 🤝 Community

- **GitHub Discussions**: Use for questions, ideas, and general discussion.
- **Issues**: Bug reports and feature requests.
- **Discord**: [Join our community](https://discord.gg/stellarforge) for real-time chat.

## 📜 License

By contributing, you agree that your contributions will be licensed under the same [MIT License](LICENSE) as the project.

---

Thank you for contributing to StellarForge! 🚀
