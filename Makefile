# Makefile for StellarForge

# Default target
.PHONY: all
all: check

# Build all workspace crates
.PHONY: build
build:
	cargo build --workspace

# Run all tests
.PHONY: test
test:
	cargo test --workspace

# Run clippy linter with deny warnings
.PHONY: lint
lint:
	cargo clippy --all-targets -- -D warnings

# Format code
.PHONY: fmt
fmt:
	cargo fmt --all

# Run fmt, lint, and test in sequence
.PHONY: check
check: fmt lint test

# Clean build artifacts
.PHONY: clean
clean:
	cargo clean

# Run contract benchmarks (Soroban budget: CPU instructions + memory bytes)
# .PHONY: bench
# bench:
# 	cargo run -p forge-benches