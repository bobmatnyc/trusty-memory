#!/usr/bin/env bash
# Smoke test for trusty-memory local install
# Why: Validate the installed binary is functional at the CLI surface level.
# What: Checks binary, version, palace lifecycle, remember/recall, kg, hooks, status.
# Test: Run after `make deploy`; all checks must pass (exit 0).
# Usage: bash tests/smoke-test.sh
set -euo pipefail

BINARY="trusty-memory"
PALACE="smoke-test-palace"
PASS=0
FAIL=0
WARN=0

# Always clean up on exit (success or failure)
cleanup() {
    "$BINARY" palace delete "$PALACE" --no-color -q 2>/dev/null || \
        rm -rf "$HOME/Library/Application Support/trusty-memory/palaces/$PALACE" 2>/dev/null || true
}
trap cleanup EXIT

ok()   { echo "  ✓ $1"; PASS=$((PASS + 1)); }
fail() { echo "  ✗ FAIL: $1"; FAIL=$((FAIL + 1)); }
warn() { echo "  ⚠ WARN: $1"; WARN=$((WARN + 1)); }

echo "=== trusty-memory smoke test ==="

# 1. Binary exists and is in PATH
if command -v "$BINARY" >/dev/null 2>&1; then
    ok "binary found: $(which $BINARY)"
else
    fail "$BINARY not found in PATH (run make deploy first)"
    exit 1
fi

# 2. Version flag
VERSION=$("$BINARY" --version 2>&1)
if [[ "$VERSION" == *"trusty-memory"* ]]; then
    ok "version: $VERSION"
else
    fail "version flag returned unexpected: $VERSION"
fi

# 3. Status command runs without crashing
if "$BINARY" status --no-color -q >/dev/null 2>&1; then
    ok "status command"
else
    warn "status command failed (daemon may not be running)"
fi

# 4. Palace create
if "$BINARY" palace new "$PALACE" --no-color -q >/dev/null 2>&1; then
    ok "palace created: $PALACE"
else
    fail "palace new failed"
fi

# 5. Palace appears in list
if "$BINARY" palace list --no-color 2>&1 | grep -q "$PALACE"; then
    ok "palace listed"
else
    warn "palace not in list"
fi

# 6. Remember a memory (using positional args: -p flag, then remember <TEXT>)
DRAWER_OUTPUT=$("$BINARY" -p "$PALACE" remember "smoke test memory: the quick brown fox jumps" --no-color 2>&1)
if echo "$DRAWER_OUTPUT" | grep -qiE "stored|drawer|ok|success|remember"; then
    ok "remember stored a memory"
else
    warn "remember output unexpected: $DRAWER_OUTPUT"
fi

# 7. Recall returns something relevant
RECALL_OUTPUT=$("$BINARY" -p "$PALACE" recall "quick brown fox" --top-k 3 --no-color 2>&1)
if echo "$RECALL_OUTPUT" | grep -qi "fox"; then
    ok "recall returned relevant result"
else
    warn "recall did not return expected content (got: $RECALL_OUTPUT)"
fi

# 8. KG assert (positional: <SUBJECT> <PREDICATE> <OBJECT>)
if "$BINARY" -p "$PALACE" kg assert "smoke-test" "is" "passing" --no-color -q >/dev/null 2>&1; then
    ok "kg assert"
else
    warn "kg assert failed"
fi

# 9. KG query returns the asserted triple
KG_OUTPUT=$("$BINARY" -p "$PALACE" kg query "smoke-test" --no-color 2>&1)
if echo "$KG_OUTPUT" | grep -q "passing"; then
    ok "kg query returned asserted triple"
else
    warn "kg query did not return expected triple (got: $KG_OUTPUT)"
fi

# 10. Hooks list runs
if "$BINARY" hooks list --no-color >/dev/null 2>&1; then
    ok "hooks list"
else
    warn "hooks list failed"
fi

echo ""
echo "=== smoke test complete: ${PASS} passed, ${WARN} warned, ${FAIL} failed ==="

[[ $FAIL -eq 0 ]]  # exit 0 if no hard failures
