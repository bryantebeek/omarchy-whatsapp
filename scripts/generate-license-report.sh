#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output="$repo_dir/quickshell/licenses.json"
check_only=0

if [[ ${1:-} == --check ]]; then
  check_only=1
elif (($#)); then
  echo "Usage: ./scripts/generate-license-report.sh [--check]" >&2
  exit 2
fi

report_tmp=$(mktemp "$output.tmp.XXXXXX")
package_list_tmp=$(mktemp "$output.packages.XXXXXX")
trap 'rm -f -- "$report_tmp" "$package_list_tmp"' EXIT

if command -v mise >/dev/null 2>&1; then
  cargo_command=(mise exec rust@nightly-2026-06-16 -- cargo)
else
  cargo_command=(cargo)
fi

"${cargo_command[@]}" tree --workspace --locked --edges normal,build \
  --prefix none --format '{p}' \
  | awk 'NF >= 2 { version=$2; sub(/^v/, "", version); print $1 "\t" version }' \
  | sort -u > "$package_list_tmp"
included_packages=$(jq -Rs '
  [split("\n")[] | select(length > 0) | split("\t")
    | {name: .[0], version: .[1]}]
' < "$package_list_tmp")
project_version=$("${cargo_command[@]}" metadata --locked --no-deps --format-version 1 \
  | jq -er '.packages[] | select(.name == "omarchy-whatsapp-protocol") | .version')

"${cargo_command[@]}" metadata --locked --format-version 1 \
  --filter-platform x86_64-unknown-linux-gnu \
  | jq --argjson included "$included_packages" --arg project_version "$project_version" '
  .workspace_members as $workspace
  | {
      schema: 1,
      generated_from: "Cargo.lock",
      entries: ([
        {
          kind: "project",
          name: "Omarchy WhatsApp",
          version: $project_version,
          license: "MIT",
          homepage: "https://github.com/bryantebeek/omarchy-whatsapp"
        },
        {
          kind: "asset",
          name: "Tabler Icons — brand-whatsapp-filled",
          version: "",
          license: "MIT",
          homepage: "https://github.com/tabler/tabler-icons"
        }
      ] + [
        .packages[]
        | select(.id as $id | $workspace | index($id) | not)
        | . as $package
        | select(any($included[];
            .name == $package.name and .version == $package.version))
        | {
            kind: "rust",
            name,
            version,
            license: (.license // "Not declared"),
            homepage: (.repository // .homepage // "")
          }
      ] | sort_by(.kind != "project", .kind != "asset", (.name | ascii_downcase), .version))
    }
' > "$report_tmp"
chmod 644 "$report_tmp"

if ((check_only)); then
  if ! cmp -s -- "$report_tmp" "$output"; then
    echo "quickshell/licenses.json is out of date; run ./scripts/generate-license-report.sh" >&2
    exit 1
  fi
  exit 0
fi

mv -- "$report_tmp" "$output"
trap - EXIT
rm -f -- "$package_list_tmp"
