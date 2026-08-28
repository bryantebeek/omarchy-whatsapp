#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
skip_build=0
runtime_only=0
pending_files=()

cleanup_pending_files() {
  if ((${#pending_files[@]})); then
    rm -f -- "${pending_files[@]}"
  fi
}
trap cleanup_pending_files EXIT

# Never expose a partially copied executable or metadata file if installation is
# interrupted. The final rename stays on the destination filesystem and is
# therefore atomic.
install_atomically() {
  local mode=$1
  local source=$2
  local destination=$3
  local destination_dir temporary

  destination_dir=$(dirname -- "$destination")
  mkdir -p -- "$destination_dir"
  temporary=$(mktemp "$destination.tmp.XXXXXX")
  pending_files+=("$temporary")
  install -m "$mode" "$source" "$temporary"
  mv -f -- "$temporary" "$destination"
}

usage() {
  echo "Usage: ./install.sh [--no-build] [--runtime-only]" >&2
}

while (($#)); do
  case $1 in
    --no-build) skip_build=1 ;;
    --runtime-only) runtime_only=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

cargo_target_dir=${CARGO_TARGET_DIR:-"$repo_dir/target"}
if [[ $cargo_target_dir != /* ]]; then
  cargo_target_dir="$repo_dir/$cargo_target_dir"
fi

if ((skip_build == 0)); then
  (cd "$repo_dir" && ./scripts/cargo.sh build --release --locked --workspace)
fi

for binary in omarchy-whatsappd omarchy-whatsappctl; do
  [[ -x "$cargo_target_dir/release/$binary" ]] || {
    echo "Missing release binary: $binary" >&2
    exit 1
  }
  install_atomically 755 \
    "$cargo_target_dir/release/$binary" "$HOME/.local/bin/$binary"
done

# Versions before 0.2 shipped a standalone GTK launcher. The full interface
# now lives in omarchy-shell, so leaving that binary behind would open the
# obsolete client from old shortcuts.
pkill -f "^$HOME/.local/bin/omarchy-whatsapp( |$)" >/dev/null 2>&1 || true
rm -f -- "$HOME/.local/bin/omarchy-whatsapp"

install_atomically 644 \
  "$repo_dir/packaging/systemd/omarchy-whatsapp.service" \
  "$HOME/.config/systemd/user/omarchy-whatsapp.service"
install_atomically 644 \
  "$repo_dir/packaging/applications/com.omarchy.WhatsApp.desktop" \
  "$HOME/.local/share/applications/com.omarchy.WhatsApp.desktop"
install_atomically 644 \
  "$repo_dir/packaging/icons/com.omarchy.WhatsApp.svg" \
  "$HOME/.local/share/icons/hicolor/scalable/apps/com.omarchy.WhatsApp.svg"

if ((runtime_only == 0)); then
  legacy_plugin_id="io.github.bryantebeek.whatsapp-native"
  plugin_id="io.github.bryantebeek.whatsapp"
  shell_config="$HOME/.config/omarchy/shell.json"
  if [[ -f "$shell_config" ]] && jq -e --arg id "$legacy_plugin_id" \
      '.. | select(type == "string" and . == $id)' "$shell_config" >/dev/null; then
    shell_config_tmp=$(mktemp "$shell_config.rename.XXXXXX")
    if jq --arg old "$legacy_plugin_id" --arg new "$plugin_id" \
        '(.. | select(type == "string" and . == $old)) |= $new' \
        "$shell_config" > "$shell_config_tmp"; then
      chmod --reference="$shell_config" "$shell_config_tmp"
      mv -- "$shell_config_tmp" "$shell_config"
    else
      rm -f -- "$shell_config_tmp"
      exit 1
    fi
  fi

  legacy_plugin_dir="$HOME/.config/omarchy/plugins/$legacy_plugin_id"
  plugin_dir="$HOME/.config/omarchy/plugins/io.github.bryantebeek.whatsapp"
  if [[ -d "$legacy_plugin_dir" && ! -e "$plugin_dir" ]]; then
    mv -- "$legacy_plugin_dir" "$plugin_dir"
  fi
  mkdir -p "$plugin_dir/quickshell/icons"

  # A marketplace installation is a git checkout at plugin_dir already. A local
  # development checkout lives elsewhere and needs its runtime subset copied in.
  if [[ $(realpath -m -- "$repo_dir") != $(realpath -m -- "$plugin_dir") ]]; then
    rm -f -- \
      "$plugin_dir/BarWidget.qml" \
      "$plugin_dir/Service.qml" \
      "$plugin_dir/Panel.qml" \
      "$plugin_dir/Model.js" \
      "$plugin_dir/licenses.json" \
      "$plugin_dir/icons/brand-whatsapp-filled.svg" \
      "$plugin_dir/icons/LICENSE.tabler"
    install_atomically 644 "$repo_dir/quickshell/BarWidget.qml" \
      "$plugin_dir/quickshell/BarWidget.qml"
    install_atomically 644 "$repo_dir/quickshell/Service.qml" \
      "$plugin_dir/quickshell/Service.qml"
    install_atomically 644 "$repo_dir/quickshell/Panel.qml" \
      "$plugin_dir/quickshell/Panel.qml"
    install_atomically 644 "$repo_dir/quickshell/Model.js" \
      "$plugin_dir/quickshell/Model.js"
    install_atomically 644 "$repo_dir/quickshell/licenses.json" \
      "$plugin_dir/quickshell/licenses.json"
    install_atomically 644 \
      "$repo_dir/quickshell/icons/brand-whatsapp-filled.svg" \
      "$plugin_dir/quickshell/icons/brand-whatsapp-filled.svg"
    install_atomically 644 "$repo_dir/quickshell/icons/LICENSE.tabler" \
      "$plugin_dir/quickshell/icons/LICENSE.tabler"
    # The manifest makes the directory visible to the shell, so install it last.
    install_atomically 644 "$repo_dir/manifest.json" "$plugin_dir/manifest.json"
  fi
fi

systemctl --user daemon-reload
systemctl --user disable --now omarchy-whatsapp-native.service >/dev/null 2>&1 || true
systemctl --user enable omarchy-whatsapp.service
systemctl --user restart omarchy-whatsapp.service
rm -f -- "$HOME/.config/systemd/user/omarchy-whatsapp-native.service"
systemctl --user daemon-reload

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$HOME/.local/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi

if ((runtime_only == 0)) && command -v omarchy >/dev/null 2>&1; then
  omarchy plugin validate "$plugin_dir"
  omarchy plugin enable "$plugin_id"
  "$repo_dir/scripts/reload-quickshell.sh"
fi

if ((runtime_only)); then
  echo "Installed and started the Omarchy WhatsApp daemon."
else
  echo "Installed Omarchy WhatsApp. Open it from the app launcher or the bar icon."
fi
