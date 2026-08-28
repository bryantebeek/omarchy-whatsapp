#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

cargo_version=$(cd "$repo_dir" && ./scripts/cargo.sh metadata --locked --no-deps \
  --format-version 1 | jq -er '.packages[] | select(.name == "omarchy-whatsapp-protocol") | .version')
manifest_version=$(jq -er '.version' "$repo_dir/manifest.json")
pkgbuild_version=$(sed -n 's/^pkgver=//p' "$repo_dir/packaging/arch/PKGBUILD")
license_version=$(jq -er '.entries[] | select(.kind == "project") | .version' \
  "$repo_dir/quickshell/licenses.json")

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

if [[ ${GITHUB_REF_TYPE:-} == tag && ${GITHUB_REF_NAME:-} != "v$cargo_version" ]]; then
  printf 'tag %s does not match package version v%s\n' "$GITHUB_REF_NAME" "$cargo_version" >&2
  exit 1
fi

printf 'All package versions match %s.\n' "$cargo_version"
