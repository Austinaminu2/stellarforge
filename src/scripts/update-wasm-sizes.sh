#!/usr/bin/env bash
# update-wasm-sizes.sh — Build all StellarForge contracts, measure WASM sizes,
# and update the WASM Binary Sizes section in README.md.
#
# Usage: ./scripts/update-wasm-sizes.sh [--dry-run]
#   --dry-run   Print the generated section to stdout instead of writing README.md

set -euo pipefail

DRY_RUN=false
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN=true
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
README="$REPO_ROOT/README.md"
WASM_DIR="$REPO_ROOT/target/wasm32v1-none/release"

CONTRACTS=(
  "forge-governor"
  "forge-multisig"
  "forge-oracle"
  "forge-stream"
  "forge-vesting"
)

# Convert hyphenated contract name to snake_case wasm filename
wasm_path() {
  local name="${1//-/_}"
  echo "$WASM_DIR/${name}.wasm"
}

# Check for wasm-opt
WASM_OPT_AVAILABLE=false
if command -v wasm-opt &>/dev/null; then
  WASM_OPT_AVAILABLE=true
else
  echo "⚠️  wasm-opt not found — optimized sizes will show N/A." >&2
  echo "   Install: brew install binaryen  |  npm install -g binaryen" >&2
  echo "   Or download from: https://github.com/WebAssembly/binaryen/releases" >&2
fi

# Check README exists
if [[ ! -f "$README" ]]; then
  echo "❌ README.md not found at $README" >&2
  exit 1
fi

# Stage 1: Build all contracts
echo "🔨 Building contracts..."
for contract in "${CONTRACTS[@]}"; do
  echo "  → $contract"
  if ! stellar contract build --package "$contract" 2>/tmp/build_err; then
    echo "❌ Build failed for $contract:" >&2
    cat /tmp/build_err >&2
    exit 1
  fi
done
echo "✅ All contracts built."

# Stage 2 & 3: Measure raw and optimized sizes
declare -A RAW_SIZES
declare -A OPT_SIZES

for contract in "${CONTRACTS[@]}"; do
  wasm="$(wasm_path "$contract")"

  # Raw size
  if [[ -f "$wasm" ]]; then
    RAW_SIZES[$contract]=$(wc -c < "$wasm" | tr -d ' ')
  else
    RAW_SIZES[$contract]="N/A"
    echo "⚠️  Binary not found for $contract: $wasm" >&2
  fi

  # Optimized size
  if [[ "$WASM_OPT_AVAILABLE" == true && -f "$wasm" ]]; then
    if wasm-opt -Oz "$wasm" -o "$wasm" 2>/tmp/opt_err; then
      OPT_SIZES[$contract]=$(wc -c < "$wasm" | tr -d ' ')
    else
      echo "⚠️  wasm-opt failed for $contract" >&2
      OPT_SIZES[$contract]="N/A"
    fi
  else
    OPT_SIZES[$contract]="N/A"
  fi
done

# Stage 4: Build the Markdown block
generate_section() {
  cat <<'MDEOF'
<!-- WASM-SIZES-START -->
## ⚙️ WASM Binary Sizes

> Sizes are in bytes. Run `./scripts/update-wasm-sizes.sh` to regenerate after rebuilding contracts.

| Contract | WASM Size (bytes) | WASM Size (optimized) |
| :--- | ---: | ---: |
MDEOF

  for contract in "${CONTRACTS[@]}"; do
    printf "| \`%s\` | %s | %s |\n" \
      "$contract" \
      "${RAW_SIZES[$contract]}" \
      "${OPT_SIZES[$contract]}"
  done

  cat <<'MDEOF'

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
MDEOF
}

SECTION="$(generate_section)"

if [[ "$DRY_RUN" == true ]]; then
  echo "$SECTION"
  exit 0
fi

# Inject into README (replace existing markers, or append on first run)
if grep -q "WASM-SIZES-START" "$README"; then
  awk -v section="$SECTION" '
    /<!-- WASM-SIZES-START -->/ { printing=1; print section; next }
    /<!-- WASM-SIZES-END -->/   { printing=0; next }
    !printing                   { print }
  ' "$README" > "$README.tmp" && mv "$README.tmp" "$README"
else
  printf "\n%s\n" "$SECTION" >> "$README"
fi

echo "✅ README.md updated with WASM sizes."
