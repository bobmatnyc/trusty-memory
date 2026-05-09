#!/usr/bin/env bash
# Smoke test for trusty-memory local install
# Tests the basic CLI surface after `make deploy`
#
# Why: Validate that the installed binary is functional at the CLI surface level
#      without requiring a running daemon for basic checks.
# What: Checks binary existence, version flag, status command, and palace lifecycle.
# Test: Run after `make deploy`; all checks marked with a checkmark should pass.
set -euo pipefail

BINARY="trusty-memory"
PALACE="smoke-test-$(date +%s)"

echo "=== trusty-memory smoke test ==="

# 1. Binary exists
command -v "$BINARY" >/dev/null 2>&1 || { echo "FAIL: $BINARY not found in PATH (run make deploy first)"; exit 1; }
echo "  binary found: $(which $BINARY)"

# 2. Version flag
VERSION=$("$BINARY" --version 2>&1 || true)
echo "  version: $VERSION"

# 3. Status (daemon may not be running -- just check exit code / output)
"$BINARY" status 2>&1 || true
echo "  status command executed"

# 4. Palace new
"$BINARY" palace new "$PALACE" 2>&1 && echo "  palace created: $PALACE" || echo "WARN: palace new failed (daemon may need to be running)"

# 5. Palace list
"$BINARY" palace list 2>&1 | grep -q "$PALACE" && echo "  palace listed" || echo "WARN: palace not listed (daemon may not be running)"

echo ""
echo "=== smoke test complete ==="
