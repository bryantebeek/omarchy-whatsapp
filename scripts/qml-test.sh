#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

if command -v qmltestrunner >/dev/null 2>&1; then
  qmltestrunner_command=$(command -v qmltestrunner)
elif [[ -x /usr/lib/qt6/bin/qmltestrunner ]]; then
  qmltestrunner_command=/usr/lib/qt6/bin/qmltestrunner
else
  echo "qmltestrunner is required; install the Qt 6 Quick Test module." >&2
  exit 1
fi

export QT_QPA_PLATFORM=${QML_TEST_PLATFORM:-offscreen}
export QT_QUICK_BACKEND=${QT_QUICK_BACKEND:-software}
export QSG_RHI_BACKEND=${QSG_RHI_BACKEND:-software}

exec "$qmltestrunner_command" \
  -input "$repo_dir/tests/qml" \
  -import "$repo_dir/tests/qml/imports" \
  -import "$repo_dir/quickshell" \
  "$@"
