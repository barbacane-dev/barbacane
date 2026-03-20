#!/usr/bin/env bash
# Test runner for the Barbacane vacuum ruleset.
# Usage: ./docs/rulesets/tests/run-tests.sh
#
# Requires: vacuum (https://quobix.com/vacuum/)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd -P)"
RULESET="$SCRIPT_DIR/../barbacane.yaml"
FUNCTIONS_DIR="$SCRIPT_DIR/../functions"
PASS=0
FAIL=0

# Count barbacane-specific violations using spectral-report JSON output.
# vacuum outputs `null` (not `[]`) when there are no violations, so jq receives
# empty input and exits 0 with no output — handle that with ${count:-0}.
count_barbacane_violations() {
  local spec="$1"
  local count
  count=$(vacuum spectral-report -r "$RULESET" --functions "$FUNCTIONS_DIR" -o "$spec" 2>/dev/null \
    | sed -n '/^\[/,$p' \
    | jq '[.[] | select(.code | startswith("barbacane-"))] | length' 2>/dev/null)
  echo "${count:-0}"
}

assert_zero_violations() {
  local spec="$1"
  local label="$2"
  local count
  count=$(count_barbacane_violations "$spec")
  if [ "$count" -eq 0 ]; then
    echo "  PASS  $label (0 barbacane violations)"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $label (expected 0 barbacane violations, got $count)"
    FAIL=$((FAIL + 1))
  fi
}

assert_has_violations() {
  local spec="$1"
  local label="$2"
  local expected_min="$3"
  local count
  count=$(count_barbacane_violations "$spec")
  if [ "$count" -ge "$expected_min" ]; then
    echo "  PASS  $label ($count barbacane violations, expected >= $expected_min)"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $label ($count barbacane violations, expected >= $expected_min)"
    FAIL=$((FAIL + 1))
  fi
}

echo "Running Barbacane vacuum ruleset tests..."
echo ""

# Valid specs should produce zero barbacane violations
echo "--- Valid specs (expect 0 barbacane violations) ---"
assert_zero_violations "$SCRIPT_DIR/valid-complete.yaml" "valid-complete"
assert_zero_violations "$ROOT_DIR/tests/fixtures/minimal.yaml" "fixtures/minimal"
assert_zero_violations "$ROOT_DIR/tests/fixtures/jwt-auth.yaml" "fixtures/jwt-auth"
assert_zero_violations "$ROOT_DIR/tests/fixtures/rate-limit.yaml" "fixtures/rate-limit"
assert_zero_violations "$ROOT_DIR/tests/fixtures/cors.yaml" "fixtures/cors"
assert_zero_violations "$ROOT_DIR/tests/fixtures/http-upstream.yaml" "fixtures/http-upstream"
assert_zero_violations "$SCRIPT_DIR/valid-wildcard-paths.yaml" "valid-wildcard-paths"
echo ""

# Invalid specs should produce violations
echo "--- Invalid specs (expect barbacane violations) ---"
assert_has_violations "$SCRIPT_DIR/invalid-dispatch.yaml" "invalid-dispatch" 3
assert_has_violations "$SCRIPT_DIR/invalid-middleware.yaml" "invalid-middleware" 3
assert_has_violations "$SCRIPT_DIR/invalid-upstream-secrets.yaml" "invalid-upstream-secrets" 2
assert_has_violations "$ROOT_DIR/tests/fixtures/invalid-missing-dispatch.yaml" "fixtures/invalid-missing-dispatch" 1
assert_has_violations "$ROOT_DIR/tests/fixtures/invalid-unknown-extension.yaml" "fixtures/invalid-unknown-extension" 1
assert_has_violations "$SCRIPT_DIR/invalid-wildcard-paths.yaml" "invalid-wildcard-paths" 2
echo ""

echo "Results: $PASS passed, $FAIL failed"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
