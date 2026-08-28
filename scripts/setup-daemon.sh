#!/usr/bin/env bash
set -euo pipefail

export CARGO_TERM_COLOR=never

source_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
action=${1:-setup}

if (( $# > 1 )); then
  echo "Usage: scripts/setup-daemon.sh [check|setup]" >&2
  exit 2
fi

manifest="$source_root/manifest.json"
daemon="$HOME/.local/bin/omarchy-whatsappd"
control="$HOME/.local/bin/omarchy-whatsappctl"
unit="$HOME/.config/systemd/user/omarchy-whatsapp.service"
source_unit="$source_root/packaging/systemd/omarchy-whatsapp.service"

expected_version() {
  jq -er '.version' "$manifest"
}

binary_matches_version() {
  local binary=$1
  local name=$2
  local version=$3

  [[ -x $binary ]] || return 1
  [[ $("$binary" --version 2>/dev/null) == "$name $version" ]]
}

runtime_is_current() {
  local version

  command -v jq >/dev/null 2>&1 || return 1
  version=$(expected_version) || return 1
  binary_matches_version "$daemon" omarchy-whatsappd "$version" || return 1
  binary_matches_version "$control" omarchy-whatsappctl "$version" || return 1
  [[ -f $unit ]] || return 1
  cmp -s -- "$source_unit" "$unit"
}

case $action in
  check)
    runtime_is_current
    ;;
  setup)
    command -v jq >/dev/null 2>&1 || {
      echo "setup: jq is required by the Omarchy plugin runtime" >&2
      exit 20
    }
    if ! command -v mise >/dev/null 2>&1 \
        && ! command -v cargo >/dev/null 2>&1; then
      echo "setup: install mise or Cargo before building the WhatsApp daemon" >&2
      exit 21
    fi

    cache_root=${XDG_CACHE_HOME:-"$HOME/.cache"}
    build_dir=${OMARCHY_WHATSAPP_BUILD_DIR:-"$cache_root/omarchy-whatsapp/build"}
    install -d -m 700 -- "$build_dir"
    export CARGO_TARGET_DIR="$build_dir"

    echo "setup: building the WhatsApp daemon; the first build can take several minutes"
    "$source_root/install.sh" --runtime-only
    runtime_is_current || {
      echo "setup: installation completed but the runtime failed verification" >&2
      exit 22
    }
    echo "setup: WhatsApp is ready"
    ;;
  -h|--help)
    echo "Usage: scripts/setup-daemon.sh [check|setup]"
    ;;
  *)
    echo "setup-daemon.sh: unknown action: $action" >&2
    echo "Usage: scripts/setup-daemon.sh [check|setup]" >&2
    exit 2
    ;;
esac
