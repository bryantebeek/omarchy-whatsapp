#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
qml_files=(
  "$repo_dir/quickshell/BarWidget.qml"
  "$repo_dir/quickshell/LicensesPopup.qml"
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

# Hosted CI does not include Omarchy or Quickshell's runtime QML modules. A
# bare qmllint invocation therefore reports every imported type as unresolved
# and exits unsuccessfully even for valid plugin QML. qmlformat uses the same
# Qt parser without pretending that semantic type checking was performed.
if command -v qmlformat >/dev/null 2>&1; then
  qmlformat_command=$(command -v qmlformat)
elif [[ -x /usr/lib/qt6/bin/qmlformat ]]; then
  qmlformat_command=/usr/lib/qt6/bin/qmlformat
else
  echo "qmlformat is required; install the Qt 6 declarative development tools." >&2
  exit 1
fi

"$qmlformat_command" -n "${qml_files[@]}" >/dev/null
echo "QML syntax passed; semantic lint requires an Omarchy host and was skipped."
