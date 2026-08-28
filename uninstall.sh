#!/usr/bin/env bash
set -euo pipefail

purge_data=0
if [[ ${1-} == "--purge-data" ]]; then
  purge_data=1
elif (($#)); then
  echo "Usage: ./uninstall.sh [--purge-data]" >&2
  exit 2
fi

# Remove only the marker-delimited launcher entries owned by this project.
if [[ -x $HOME/.local/bin/omarchy-whatsappctl ]]; then
  "$HOME/.local/bin/omarchy-whatsappctl" launcher-remove >/dev/null 2>&1 || \
    echo "Warning: could not remove generated WhatsApp launcher entries." >&2
fi

systemctl --user disable --now omarchy-whatsapp.service >/dev/null 2>&1 || true
systemctl --user disable --now omarchy-whatsapp-native.service >/dev/null 2>&1 || true

if command -v omarchy >/dev/null 2>&1; then
  omarchy plugin disable io.github.bryantebeek.whatsapp >/dev/null 2>&1 || true
  omarchy plugin disable io.github.bryantebeek.whatsapp-native >/dev/null 2>&1 || true
fi

# Removal is deliberately explicit and limited to paths owned by this project.
rm -f -- \
  "$HOME/.local/bin/omarchy-whatsapp" \
  "$HOME/.local/bin/omarchy-whatsappd" \
  "$HOME/.local/bin/omarchy-whatsappctl" \
  "$HOME/.config/systemd/user/omarchy-whatsapp.service" \
  "$HOME/.config/systemd/user/omarchy-whatsapp-native.service" \
  "$HOME/.local/share/applications/com.omarchy.WhatsApp.desktop" \
  "$HOME/.local/share/icons/hicolor/scalable/apps/com.omarchy.WhatsApp.svg"
rm -rf -- \
  "$HOME/.config/omarchy/plugins/io.github.bryantebeek.whatsapp" \
  "$HOME/.config/omarchy/plugins/io.github.bryantebeek.whatsapp-native"

state_dir="${XDG_STATE_HOME:-"$HOME/.local/state"}/omarchy-whatsapp"
if ((purge_data == 1)); then
  rm -rf -- "$state_dir"
  echo "Removed application files and local WhatsApp session/history data."
else
  echo "Removed application files. Session data remains in $state_dir."
fi
systemctl --user daemon-reload
