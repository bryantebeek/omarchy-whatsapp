#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

cd "$repo_dir"
./scripts/cargo.sh fmt --all -- --check
./scripts/cargo.sh clippy --workspace --all-targets --all-features --locked -- -D warnings
./scripts/cargo.sh test --workspace --all-features --locked
./scripts/check-version.sh
./scripts/check-manifest.sh
./scripts/generate-license-report.sh --check
./tests/diff-coverage.sh
/usr/lib/qt6/bin/qmlformat -n quickshell/*.qml >/dev/null
jq -e . manifest.json quickshell/licenses.json >/dev/null
./scripts/qml-lint.sh
./scripts/qml-coverage.sh
./scripts/qml-mutation.sh

if command -v shellcheck >/dev/null 2>&1; then
  shellcheck install.sh uninstall.sh scripts/*.sh tests/*.sh
else
  echo "shellcheck is unavailable; CI will enforce shell linting." >&2
fi

if command -v omarchy >/dev/null 2>&1; then
  omarchy plugin validate .
fi
