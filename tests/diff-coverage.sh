#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
fixture_dir=$(mktemp -d)

cleanup() {
  rm -rf -- "$fixture_dir"
}
trap cleanup EXIT

git -C "$fixture_dir" init --quiet --initial-branch=main
git -C "$fixture_dir" config user.name Coverage Test
git -C "$fixture_dir" config user.email coverage@example.invalid
git -C "$fixture_dir" config commit.gpgsign false
git -C "$fixture_dir" config diff.mnemonicPrefix true

mkdir -p -- "$fixture_dir/crates/demo/src"
printf '%s\n' 'pub fn existing() {}' >"$fixture_dir/crates/demo/src/lib.rs"
git -C "$fixture_dir" add crates/demo/src/lib.rs
git -C "$fixture_dir" commit --quiet --message baseline
printf '%s\n' 'pub fn changed() {}' >>"$fixture_dir/crates/demo/src/lib.rs"

lcov_file="$fixture_dir/lcov.info"
printf 'SF:%s\nDA:1,1\nDA:2,1\nend_of_record\n' \
  "$fixture_dir/crates/demo/src/lib.rs" >"$lcov_file"
COVERAGE_REPO_DIR="$fixture_dir" \
  "$repo_dir/scripts/check-diff-coverage.sh" "$lcov_file" HEAD >/dev/null

printf 'SF:%s\nDA:1,1\nDA:2,0\nend_of_record\n' \
  "$fixture_dir/crates/demo/src/lib.rs" >"$lcov_file"
if COVERAGE_REPO_DIR="$fixture_dir" \
  "$repo_dir/scripts/check-diff-coverage.sh" "$lcov_file" HEAD >/dev/null 2>&1; then
  echo "Diff coverage accepted an uncovered changed line." >&2
  exit 1
fi

echo "Diff coverage regression test passed."
