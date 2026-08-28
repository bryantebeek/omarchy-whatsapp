#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
coverage_boundary='crates/(ctl/src/main|daemon/src/(assets|database|main|notification))\.rs$'
overall_line_floor=54.8
lcov_report="$repo_dir/target/llvm-cov/lcov.info"

if command -v mise >/dev/null 2>&1; then
  cargo_command=(mise exec rust@nightly-2026-06-16 -- cargo)
else
  cargo_command=(cargo)
fi

cd "$repo_dir"
"${cargo_command[@]}" llvm-cov --workspace --all-features --locked \
  --fail-under-lines "$overall_line_floor"
"${cargo_command[@]}" llvm-cov report \
  --ignore-filename-regex "$coverage_boundary" \
  --fail-under-lines 100 \
  --fail-uncovered-lines 0
mkdir -p -- "$(dirname -- "$lcov_report")"
"${cargo_command[@]}" llvm-cov report --lcov --output-path "$lcov_report"

if [[ -n ${COVERAGE_BASE_REF:-} ]]; then
  "$repo_dir/scripts/check-diff-coverage.sh" "$lcov_report" "$COVERAGE_BASE_REF"
else
  echo "Diff coverage skipped; set COVERAGE_BASE_REF to enforce it locally."
fi
