#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
lcov_report="$repo_dir/target/llvm-cov/lcov.info"

if command -v mise >/dev/null 2>&1; then
  cargo_command=(mise exec rust@nightly-2026-06-16 -- cargo)
else
  cargo_command=(cargo)
fi

cd "$repo_dir"
"${cargo_command[@]}" llvm-cov clean --workspace
"${cargo_command[@]}" llvm-cov --workspace --all-features --locked --no-report
mkdir -p -- "$(dirname -- "$lcov_report")"
"${cargo_command[@]}" llvm-cov report \
  --lcov \
  --output-path "$lcov_report"

# LLVM's aggregate LF/LH counters include synthetic lines for macros, generic
# monomorphizations, and `?` continuations that have no DA source mapping. The
# LCOV DA records are the executable Rust source lines developers can inspect.
# Requiring every one of those records to execute is a stable, literal 100%
# source-line gate and fails with exact file:line diagnostics.
awk -F '[:,]' '
  /^SF:/ { source = substr($0, 4) }
  /^DA:/ {
    total += 1
    if ($3 == 0) {
      printf "uncovered Rust source line: %s:%s\n", source, $2 > "/dev/stderr"
      missed += 1
    }
  }
  END {
    if (total == 0) {
      print "coverage report contained no Rust source lines" > "/dev/stderr"
      exit 1
    }
    printf "Rust source line coverage: %d/%d (%.2f%%)\n", total - missed, total, 100 * (total - missed) / total
    if (missed > 0) exit 1
  }
' "$lcov_report"

if [[ -n ${COVERAGE_BASE_REF:-} ]]; then
  "$repo_dir/scripts/check-diff-coverage.sh" "$lcov_report" "$COVERAGE_BASE_REF"
else
  echo "Diff coverage skipped; set COVERAGE_BASE_REF to enforce it locally."
fi
