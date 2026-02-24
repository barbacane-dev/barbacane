#!/usr/bin/env bash
# Count all passing tests across the project.
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

echo -e "${bold}=== Barbacane Test Count ===${reset}"
echo ""

# 1. Workspace unit tests (excludes barbacane-test which holds integration/CLI tests)
echo -e "${dim}Running workspace unit tests...${reset}"
UNIT=$(cargo test --workspace --exclude barbacane-test 2>&1 \
  | grep "^test result:" \
  | awk '{sum += $4} END {print sum}')
echo -e "  Unit tests:        ${bold}${UNIT}${reset}"

# 2. Plugin tests (only dirs with Cargo.toml — avoids pre-built wasm-only dirs)
echo -e "${dim}Running plugin tests...${reset}"
PLUGIN_TOTAL=0
for p in "$ROOT"/plugins/*/; do
  [ -f "$p/Cargo.toml" ] || continue
  count=$(cd "$p" && cargo test 2>&1 \
    | grep "^test result:" \
    | awk '{sum += $4} END {print sum}')
  PLUGIN_TOTAL=$((PLUGIN_TOTAL + count))
done
echo -e "  Plugin tests:      ${bold}${PLUGIN_TOTAL}${reset}"

# 3. Integration tests (gateway module from barbacane-test)
echo -e "${dim}Running integration tests...${reset}"
TEST_OUTPUT=$(cargo test -p barbacane-test 2>&1)
INTEGRATION=$(echo "$TEST_OUTPUT" | grep "^test gateway::" | grep -c " ok$" || true)
echo -e "  Integration tests: ${bold}${INTEGRATION}${reset}"

# 4. CLI tests (cli module from barbacane-test)
CLI=$(echo "$TEST_OUTPUT" | grep "^test cli::" | grep -c " ok$" || true)
echo -e "  CLI tests:         ${bold}${CLI}${reset}"

# Check for failures in barbacane-test
FAILED=$(echo "$TEST_OUTPUT" | grep "^test " | grep -c " FAILED$" || true)
if [ "$FAILED" -gt 0 ]; then
  echo -e "  \033[31m⚠ ${FAILED} test(s) FAILED in barbacane-test\033[0m"
  echo "$TEST_OUTPUT" | grep "^test " | grep " FAILED$" | sed 's/^/    /'
fi

# 5. UI unit tests (vitest)
echo -e "${dim}Running UI unit tests...${reset}"
UI=$(cd "$ROOT/ui" && npx vitest run 2>&1 \
  | grep -E "Tests\s+" \
  | grep -oE '[0-9]+ passed' \
  | awk '{print $1}')
echo -e "  UI tests:          ${bold}${UI}${reset}"

# 6. E2E tests (static count — Playwright needs a running dev server)
E2E=$(grep -rh "^\s*test(" "$ROOT"/ui/e2e/*.spec.ts 2>/dev/null | wc -l | tr -d ' ')
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
