#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)

cleanup() {
  rm -rf -- "$test_dir"
}
trap cleanup EXIT

archive=$("$repo_dir"/scripts/package-release.sh "$test_dir")
version=$(jq -er .version "$repo_dir/manifest.json")
archive_root="omarchy-whatsapp-$version"
plugin_root="$archive_root/usr/share/omarchy/shell/plugins/whatsapp"
contents="$test_dir/archive-contents.txt"
tar -tzf "$archive" >"$contents"

grep -Fx "$plugin_root/manifest.json" "$contents" >/dev/null
# Every runtime file present in the repository must reach the archive: a panel
# component that is only in the checkout would break the packaged plugin.
for plugin_file in "$repo_dir"/quickshell/*.qml "$repo_dir"/quickshell/*.js \
    "$repo_dir"/quickshell/*.json "$repo_dir"/quickshell/icons/*; do
  relative_file=${plugin_file#"$repo_dir/"}
  grep -Fx "$plugin_root/$relative_file" "$contents" >/dev/null || {
    echo "Release archive is missing $relative_file." >&2
    exit 1
  }
done
if grep -F "/usr/share/omarchy-whatsapp/" "$contents" >/dev/null; then
  echo "Release archive still contains the unscanned legacy plugin path." >&2
  exit 1
fi

grep -Fq 'plugin_root="$pkgdir/usr/share/omarchy/shell/plugins/whatsapp"' \
  "$repo_dir/packaging/arch/PKGBUILD"
grep -Fq 'install -Dm644 manifest.json "$plugin_root/manifest.json"' \
  "$repo_dir/packaging/arch/PKGBUILD"
# The Arch package must derive the same set instead of listing files by hand.
grep -Fq 'for plugin_file in quickshell/*.qml quickshell/*.js quickshell/*.json' \
  "$repo_dir/packaging/arch/PKGBUILD"

echo "Packaged Omarchy plugin layout passed."
