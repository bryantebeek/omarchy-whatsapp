#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
coverage_boundary='crates/(ctl/src/main|daemon/src/(assets|database|main|notification))\.rs$'

if command -v mise >/dev/null 2>&1; then
  cargo_command=(mise exec rust@nightly-2026-06-16 -- cargo)
else
  cargo_command=(cargo)
fi

cd "$repo_dir"
"${cargo_command[@]}" llvm-cov --workspace --all-features --locked \
  --fail-under-lines 54
"${cargo_command[@]}" llvm-cov report \
  --ignore-filename-regex "$coverage_boundary" \
  --fail-under-lines 100 \
  --fail-uncovered-lines 0
