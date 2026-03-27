#!/usr/bin/env bash
# Count all tests across the project by scanning source annotations.
# Instant — no compilation or test execution needed.
#
# Run from repo root: ./scripts/count-tests.sh
#
# Options:
#   --update-readme   Update the badge numbers in README.md automatically
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

UPDATE_README=false
if [[ "${1:-}" == "--update-readme" ]]; then
  UPDATE_README=true
fi

bold="\033[1m"
dim="\033[2m"
reset="\033[0m"

# Count #[test] and #[tokio::test] annotations in Rust files under a directory.
count_rust_tests() {
  local n
  n=$(grep -rch '#\[test\]\|#\[tokio::test\]' "$@" --include='*.rs' 2>/dev/null | awk '{s+=$1} END {print s+0}') || true
  echo "${n:-0}"
}

echo -e "${bold}=== Barbacane Test Count ===${reset}"
echo ""

# 1. Workspace unit tests (all crates except barbacane-test)
UNIT=0
for crate in "$ROOT"/crates/*/src/; do
  [[ "$crate" == *barbacane-test* ]] && continue
  count=$(count_rust_tests "$crate")
  UNIT=$((UNIT + count))
done
echo -e "  Unit tests:        ${bold}${UNIT}${reset}"

# 2. Plugin tests
PLUGIN_TOTAL=0
for p in "$ROOT"/plugins/*/; do
  [ -f "$p/Cargo.toml" ] || continue
  [ -d "$p/src" ] || continue
  count=$(count_rust_tests "$p/src/")
  PLUGIN_TOTAL=$((PLUGIN_TOTAL + count))
done
echo -e "  Plugin tests:      ${bold}${PLUGIN_TOTAL}${reset}"

# 3. Integration tests (barbacane-test tests/ directory)
INTEGRATION=$(count_rust_tests "$ROOT/crates/barbacane-test/tests/")
echo -e "  Integration tests: ${bold}${INTEGRATION}${reset}"

# 4. CLI tests (barbacane-test src/ directory)
CLI=$(count_rust_tests "$ROOT/crates/barbacane-test/src/")
echo -e "  CLI tests:         ${bold}${CLI}${reset}"

# 5. UI unit tests (vitest — count `it(` and `test(` in .test.* files)
UI=$(grep -rh '^\s*\(it\|test\)(' "$ROOT/ui/src/" --include='*.test.*' 2>/dev/null | wc -l | tr -d ' ')
echo -e "  UI tests:          ${bold}${UI}${reset}"

# 6. E2E tests (Playwright — count `test(` in .spec.ts files)
E2E=$(grep -rh '^\s*test(' "$ROOT"/ui/e2e/*.spec.ts 2>/dev/null | wc -l | tr -d ' ')
echo -e "  E2E tests:         ${bold}${E2E}${reset}"

TOTAL=$((UNIT + PLUGIN_TOTAL + INTEGRATION + CLI + UI + E2E))
echo ""
echo -e "${bold}Total:               ${TOTAL}${reset}"

# Update README badges if requested
if [ "$UPDATE_README" = true ]; then
  echo ""
  echo -e "${dim}Updating README.md badges...${reset}"
  sed -i '' \
    -e "s/unit%20tests-[0-9]*%20passing/unit%20tests-${UNIT}%20passing/" \
    -e "s/plugin%20tests-[0-9]*%20passing/plugin%20tests-${PLUGIN_TOTAL}%20passing/" \
    -e "s/integration%20tests-[0-9]*%20passing/integration%20tests-${INTEGRATION}%20passing/" \
    -e "s/cli%20tests-[0-9]*%20passing/cli%20tests-${CLI}%20passing/" \
    -e "s/ui%20tests-[0-9]*%20passing/ui%20tests-${UI}%20passing/" \
    -e "s/e2e%20tests-[0-9]*%20passing/e2e%20tests-${E2E}%20passing/" \
    "$ROOT/README.md"
  echo "  README.md updated."
fi
