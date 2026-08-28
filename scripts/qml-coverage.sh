#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

"$repo_dir/scripts/qml-test.sh" -silent
TZ=UTC LC_ALL=C.UTF-8 "$repo_dir/scripts/qml-test.sh" \
  -input "$repo_dir/tests/qml/tst_model.qml" -silent
TZ=Pacific/Auckland LC_ALL=C.UTF-8 "$repo_dir/scripts/qml-test.sh" \
  -input "$repo_dir/tests/qml/tst_model.qml" -silent
exec python3 "$repo_dir/scripts/qml-coverage.py"
