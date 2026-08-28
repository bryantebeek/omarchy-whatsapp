#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
qml_files=(
  "$repo_dir/quickshell/BarWidget.qml"
  "$repo_dir/quickshell/Panel.qml"
  "$repo_dir/quickshell/Service.qml"
)

if command -v qmllint >/dev/null 2>&1; then
  qmllint_command=$(command -v qmllint)
elif [[ -x /usr/lib/qt6/bin/qmllint ]]; then
  qmllint_command=/usr/lib/qt6/bin/qmllint
else
  echo "qmllint is required; install the Qt 6 declarative development tools." >&2
  exit 1
fi

omarchy_root=${OMARCHY_PATH:-/usr/share/omarchy}
commons_qmldir="$omarchy_root/shell/Commons/qmldir"
ui_qmldir="$omarchy_root/shell/Ui/qmldir"

if [[ -r $commons_qmldir && -r $ui_qmldir ]]; then
  # Omarchy injects shell/service objects dynamically, so qmllint cannot infer
  # those properties or qualify their members. Keep every other category strict.
  "$qmllint_command" \
    -i "$commons_qmldir" \
    -i "$ui_qmldir" \
    --unqualified disable \
    --missing-property disable \
    --signal-handler-parameters disable \
    --unused-imports warning \
    --max-warnings 0 \
    "${qml_files[@]}"
  echo "QML lint passed with the installed Omarchy type definitions."
  exit 0
fi

# Hosted CI does not include Omarchy's private shell modules. qmllint still
# rejects malformed QML; its unresolved-import warnings do not affect status.
lint_output=$(mktemp)
trap 'rm -f -- "$lint_output"' EXIT
if ! "$qmllint_command" "${qml_files[@]}" >"$lint_output" 2>&1; then
  cat "$lint_output" >&2
  exit 1
fi
echo "QML syntax passed; Omarchy type definitions are unavailable in this environment."
