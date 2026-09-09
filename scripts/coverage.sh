#!/usr/bin/env bash
# Line coverage via cargo-llvm-cov (source-based LLVM instrumentation).
#
# Run from repo root: ./scripts/coverage.sh [--unit] [--html] [--lcov PATH]
#                                           [--summary PATH] [--floor N]
#
# Options:
#   --unit          Unit and bin tests only; skip the integration suite
#   --html          Also write a browsable report to target/llvm-cov/html
#   --lcov PATH     Also write lcov output to PATH
#   --summary PATH  Copy the summary table to PATH as well as stdout
#   --floor N       Fail if line coverage is below N percent
#
# The integration suite drives the gateway as a child process. `show-env` puts
# the instrumented build under CARGO_TARGET_DIR and the test harness resolves
# the binary from there, so the process that runs is the instrumented one and
# the request pipeline is measured. Requires the WASM plugins (`make plugins`).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

unit_only=0
want_html=0
lcov_path=""
summary_path=""
floor=""

usage() {
 sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --unit) unit_only=1; shift ;;
    --html) want_html=1; shift ;;
    --lcov) lcov_path="$2"; shift 2 ;;
    --summary) summary_path="$2"; shift 2 ;;
    --floor) floor="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

if ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "cargo-llvm-cov is not installed. Install it with:" >&2
  echo "  cargo install cargo-llvm-cov" >&2
  echo "  rustup component add llvm-tools-preview" >&2
  exit 1
fi

# Integration targets are discovered so a new suite is measured automatically.
# `security` is excluded: it needs PostgreSQL and the control-plane binary.
suites=()
targets=()
while IFS= read -r suite; do
  suites+=("$suite")
  targets+=(--test "$suite")
done < <(
  find crates/barbacane-test/tests -maxdepth 1 -name '*.rs' -exec basename {} .rs \; \
    | grep -vx security | sort
)

# shellcheck disable=SC1090
source <(cargo llvm-cov show-env --export-prefix)
cargo llvm-cov clean --workspace

cargo test --workspace --lib --bins --exclude barbacane-test

if [[ "$unit_only" -eq 0 ]]; then
  # Build every binary instrumented before the suite runs. The CLI tests and
  # the gateway harness both invoke `barbacane` as a subprocess, and
  # `cargo test` alone builds bin targets only as test harnesses, leaving no
  # such executable for them to find.
  cargo build --workspace --all-targets

  echo "Integration targets: ${suites[*]}"
  cargo test -p barbacane-test --lib "${targets[@]}" --no-fail-fast -- --test-threads=2
fi

if [[ -n "$lcov_path" ]]; then
  cargo llvm-cov report --lcov --output-path "$lcov_path"
fi

if [[ "$want_html" -eq 1 ]]; then
  cargo llvm-cov report --html
  echo "Report: target/llvm-cov/html/index.html"
fi

if [[ -n "$summary_path" ]]; then
  cargo llvm-cov report --summary-only | tee "$summary_path"
else
  cargo llvm-cov report --summary-only
fi

if [[ -n "$floor" ]]; then
  cargo llvm-cov report --fail-under-lines "$floor"
fi
