#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOK_SRC="$ROOT_DIR/.git/hooks/pre-commit"

echo "🔧 Installing Ferrite DM git pre-commit hook..."

cat << 'EOF' > "$HOOK_SRC"
#!/usr/bin/env bash
set -euo pipefail

# Ferrite DM Pre-Commit Hook (Enforces docs/CI_CD.md Section 3)
echo "🔍 [Pre-Commit] Running code hygiene and CI pre-flight checks..."

# 1. Format check
echo "   --> Checking formatting (cargo fmt)..."
cargo fmt --all -- --check || {
    echo "❌ cargo fmt check failed! Run 'cargo fmt --all' to fix formatting."
    exit 1
}

# 2. Clippy check
echo "   --> Checking lints (cargo clippy -D warnings)..."
cargo clippy --workspace --all-targets -- -D warnings || {
    echo "❌ cargo clippy check failed with warnings/errors!"
    exit 1
}

# 3. Documentation check
echo "   --> Verifying documentation (cargo doc)..."
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps || {
    echo "❌ cargo doc emitted warnings!"
    exit 1
}

echo "✅ [Pre-Commit] All local pre-flight checks passed!"
exit 0
EOF

chmod +x "$HOOK_SRC"
echo "✅ Git pre-commit hook installed successfully at $HOOK_SRC"
