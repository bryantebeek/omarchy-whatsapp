#!/usr/bin/env bash
set -euo pipefail

if (($# != 1)); then
  echo "Usage: $0 OUTPUT_DIRECTORY" >&2
  exit 2
fi

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output_dir=$(realpath -m -- "$1")
version=$(jq -er .version "$repo_dir/manifest.json")
archive_name="omarchy-whatsapp-${version}-x86_64-linux.tar.gz"
staging_dir=$(mktemp -d)
package_root="$staging_dir/omarchy-whatsapp-$version"

cleanup() {
  rm -rf -- "$staging_dir"
}
trap cleanup EXIT

for binary in omarchy-whatsappd omarchy-whatsappctl; do
  [[ -x "$repo_dir/target/release/$binary" ]] || {
    echo "Missing release binary: target/release/$binary" >&2
    exit 1
  }
  install -Dm755 "$repo_dir/target/release/$binary" \
    "$package_root/usr/bin/$binary"
done

packaged_service="$staging_dir/omarchy-whatsapp.service"
sed 's|%h/.local/bin/omarchy-whatsappd|/usr/bin/omarchy-whatsappd|' \
  "$repo_dir/packaging/systemd/omarchy-whatsapp.service" >"$packaged_service"
install -Dm644 "$packaged_service" \
  "$package_root/usr/lib/systemd/user/omarchy-whatsapp.service"
install -Dm644 "$repo_dir/packaging/applications/com.omarchy.WhatsApp.desktop" \
  "$package_root/usr/share/applications/com.omarchy.WhatsApp.desktop"
install -Dm644 "$repo_dir/packaging/icons/com.omarchy.WhatsApp.svg" \
  "$package_root/usr/share/icons/hicolor/scalable/apps/com.omarchy.WhatsApp.svg"

install -Dm644 "$repo_dir/manifest.json" \
  "$package_root/usr/share/omarchy/shell/plugins/whatsapp/manifest.json"
for plugin_file in BarWidget.qml Service.qml Panel.qml Model.js licenses.json; do
  install -Dm644 "$repo_dir/quickshell/$plugin_file" \
    "$package_root/usr/share/omarchy/shell/plugins/whatsapp/quickshell/$plugin_file"
done
install -Dm644 "$repo_dir/quickshell/icons/brand-whatsapp-filled.svg" \
  "$package_root/usr/share/omarchy/shell/plugins/whatsapp/quickshell/icons/brand-whatsapp-filled.svg"
install -Dm644 "$repo_dir/quickshell/icons/LICENSE.tabler" \
  "$package_root/usr/share/licenses/omarchy-whatsapp/LICENSE.tabler"
install -Dm644 "$repo_dir/LICENSE" \
  "$package_root/usr/share/licenses/omarchy-whatsapp/LICENSE"
for document in README.md ARCHITECTURE.md SECURITY.md; do
  install -Dm644 "$repo_dir/$document" \
    "$package_root/usr/share/doc/omarchy-whatsapp/$document"
done

mkdir -p -- "$output_dir"
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo_dir" log -1 --format=%ct)}
tar --sort=name --mtime="@$source_date_epoch" --owner=0 --group=0 \
  --numeric-owner -cf - -C "$staging_dir" "omarchy-whatsapp-$version" \
  | gzip -n >"$output_dir/$archive_name"
(
  cd "$output_dir"
  sha256sum "$archive_name" >"$archive_name.sha256"
)

printf '%s\n' "$output_dir/$archive_name"
