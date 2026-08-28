#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$repo_dir/manifest.json"

jq -e '
  .schemaVersion == 1
  and (.id | test("^[a-z0-9]+([._-][a-z0-9]+)+$"))
  and (.id | startswith("omarchy.") | not)
  and ([.name, .version, .author, .description] | all(type == "string" and length > 0))
  and (.kinds | type == "array" and length > 0)
  and (.entryPoints | type == "object")
  and ((.kinds | index("service") | not) or (.entryPoints.service | type == "string"))
  and ((.kinds | index("bar-widget") | not) or (.entryPoints.barWidget | type == "string"))
  and ((.kinds | index("panel") | not) or (.entryPoints.panel | type == "string"))
' "$manifest" >/dev/null

while IFS= read -r entry_point; do
  if [[ $entry_point == /* || $entry_point == *..* || ! -f $repo_dir/$entry_point ]]; then
    printf 'Unsafe or missing manifest entry point: %s\n' "$entry_point" >&2
    exit 1
  fi
done < <(jq -er '.entryPoints[]' "$manifest")

if find "$repo_dir/quickshell" -type l -print -quit | grep -q .; then
  echo "Plugin runtime files must not contain symlinks." >&2
  exit 1
fi

echo "Manifest contract and entry points are valid."
