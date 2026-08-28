#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)

cleanup() {
  rm -rf -- "$test_dir"
}
trap cleanup EXIT

archive=$($repo_dir/scripts/package-release.sh "$test_dir")
version=$(jq -er .version "$repo_dir/manifest.json")
archive_root="omarchy-whatsapp-$version"
plugin_root="$archive_root/usr/share/omarchy/shell/plugins/whatsapp"
contents="$test_dir/archive-contents.txt"
tar -tzf "$archive" >"$contents"

grep -Fx "$plugin_root/manifest.json" "$contents" >/dev/null
for plugin_file in BarWidget.qml Service.qml Panel.qml Model.js licenses.json; do
  grep -Fx "$plugin_root/quickshell/$plugin_file" "$contents" >/dev/null
done
grep -Fx "$plugin_root/quickshell/icons/brand-whatsapp-filled.svg" "$contents" >/dev/null
if grep -F "/usr/share/omarchy-whatsapp/" "$contents" >/dev/null; then
  echo "Release archive still contains the unscanned legacy plugin path." >&2
  exit 1
fi

grep -Fq 'usr/share/omarchy/shell/plugins/whatsapp/manifest.json' \
  "$repo_dir/packaging/arch/PKGBUILD"

echo "Packaged Omarchy plugin layout passed."
