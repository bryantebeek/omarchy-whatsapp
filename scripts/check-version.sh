#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

cargo_version=$(cd "$repo_dir" && ./scripts/cargo.sh metadata --locked --no-deps \
  --format-version 1 | jq -er '.packages[] | select(.name == "omarchy-whatsapp-protocol") | .version')
manifest_version=$(jq -er '.version' "$repo_dir/manifest.json")
pkgbuild_version=$(sed -n 's/^pkgver=//p' "$repo_dir/packaging/arch/PKGBUILD")
license_version=$(jq -er '.entries[] | select(.kind == "project") | .version' \
  "$repo_dir/quickshell/licenses.json")
protocol_version=$(sed -n 's/^pub const PROTOCOL_VERSION: u16 = \([0-9][0-9]*\);$/\1/p' \
  "$repo_dir/crates/protocol/src/lib.rs")
qml_protocol_version=$(sed -n \
  's/^[[:space:]]*readonly property int protocolVersion: \([0-9][0-9]*\)$/\1/p' \
  "$repo_dir/quickshell/Service.qml")

for entry in \
  "manifest.json:$manifest_version" \
  "packaging/arch/PKGBUILD:$pkgbuild_version" \
  "quickshell/licenses.json:$license_version"; do
  file=${entry%%:*}
  version=${entry#*:}
  if [[ $version != "$cargo_version" ]]; then
    printf '%s has version %s; expected %s\n' "$file" "$version" "$cargo_version" >&2
    exit 1
  fi
done

if [[ -z $protocol_version || $qml_protocol_version != "$protocol_version" ]]; then
  printf 'quickshell/Service.qml has protocol version %s; expected %s\n' \
    "${qml_protocol_version:-missing}" "${protocol_version:-missing}" >&2
  exit 1
fi

if [[ ${GITHUB_REF_TYPE:-} == tag && ${GITHUB_REF_NAME:-} != "v$cargo_version" ]]; then
  printf 'tag %s does not match package version v%s\n' "$GITHUB_REF_NAME" "$cargo_version" >&2
  exit 1
fi

printf 'All package versions match %s.\n' "$cargo_version"
printf 'Daemon and shell protocol versions match %s.\n' "$protocol_version"
