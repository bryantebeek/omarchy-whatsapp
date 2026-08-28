#!/usr/bin/env bash
set -euo pipefail

plugin_id="io.github.bryantebeek.whatsapp"
session_omarchy_path=$(systemctl --user show-environment 2>/dev/null \
  | sed -n 's/^OMARCHY_PATH=//p' | tail -n 1)
: "${session_omarchy_path:=${OMARCHY_PATH:-/usr/share/omarchy}}"
shell_path="$session_omarchy_path/shell"

plugin_target_ready() {
  timeout 1 qs ipc -n -p "$shell_path" show 2>/dev/null \
    | rg -q "^target ${plugin_id}$"
}

plugin_known() {
  omarchy shell shell listPlugins 2>/dev/null \
    | jq -e --arg id "$plugin_id" 'any(.[]; .id == $id)' >/dev/null
}

omarchy shell shell rescanPlugins

known_deadline=$((SECONDS + 10))
while ! plugin_known; do
  if ((SECONDS >= known_deadline)); then
    echo "WhatsApp plugin was not discovered after rescan." >&2
    exit 1
  fi
  sleep 0.1
done
omarchy plugin enable "$plugin_id" >/dev/null

# A rescan unloads and recreates plugin components asynchronously. File watcher
# events can also queue another reload 150 ms later. Require the plugin's
# IpcHandler to remain registered across a full one-second stability window so
# a restart cannot overlap IpcHandler::onPostReload().
stable_checks=0
deadline=$((SECONDS + 10))
while ((SECONDS < deadline)); do
  if plugin_target_ready; then
    stable_checks=$((stable_checks + 1))
    if ((stable_checks >= 6)); then
      break
    fi
  else
    stable_checks=0
  fi
  sleep 0.2
done

if ((stable_checks < 6)); then
  echo "WhatsApp plugin reload did not settle; refusing to restart Quickshell." >&2
  exit 1
fi

omarchy restart shell
omarchy shell shell ping >/dev/null
