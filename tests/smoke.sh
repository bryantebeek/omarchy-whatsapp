#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)
daemon_pid=

cleanup() {
  if [[ -n $daemon_pid ]]; then
    kill -TERM "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$test_dir"
}
trap cleanup EXIT

daemon=${OMARCHY_WHATSAPP_DAEMON:-"$repo_dir/target/release/omarchy-whatsappd"}
ctl=${OMARCHY_WHATSAPP_CTL:-"$repo_dir/target/release/omarchy-whatsappctl"}
[[ -x $daemon && -x $ctl ]] || {
  echo "Release binaries are missing; run cargo build --release --locked --workspace first." >&2
  exit 1
}

qmlformat=/usr/lib/qt6/bin/qmlformat
if [[ -x $qmlformat ]]; then
  "$qmlformat" -n "$repo_dir"/quickshell/*.qml >/dev/null
fi

socket="$test_dir/runtime/daemon.sock"
state="$test_dir/state"
"$daemon" --socket "$socket" --state-dir "$state" >"$test_dir/daemon.log" 2>&1 &
daemon_pid=$!

for _ in {1..100}; do
  [[ -S $socket ]] && break
  kill -0 "$daemon_pid" 2>/dev/null || {
    sed -n '1,160p' "$test_dir/daemon.log" >&2
    exit 1
  }
  sleep 0.05
done
[[ -S $socket ]] || {
  echo "daemon socket did not appear" >&2
  exit 1
}

[[ $(stat -c '%a' "$(dirname "$socket")") == 700 ]]
[[ $(stat -c '%a' "$socket") == 600 ]]
"$ctl" --socket "$socket" ping | jq -e '.event == "pong"' >/dev/null
"$ctl" --socket "$socket" status | jq -e \
  '.event == "state" and (.status.state == "starting" or .status.state == "pairing")' \
  >/dev/null
"$ctl" --socket "$socket" chats | jq -e '.event == "chats" and (.chats | type == "array")' \
  >/dev/null

menu="$test_dir/omarchy-menu.jsonc"
printf '{\n  "personal": {"label":"Personal"}\n}\n' >"$menu"
"$ctl" --socket "$socket" launcher-sync --menu-path "$menu" \
  | jq -e '.launcher == "synced" and .chats == 0 and .changed == true' >/dev/null
grep -F 'BEGIN omarchy-whatsapp launcher chats' "$menu" >/dev/null
grep -F '"personal"' "$menu" >/dev/null
"$ctl" launcher-remove --menu-path "$menu" \
  | jq -e '.launcher == "removed" and .changed == true' >/dev/null
if grep -F 'BEGIN omarchy-whatsapp launcher chats' "$menu" >/dev/null; then
  echo "launcher-remove left its generated menu block behind" >&2
  exit 1
fi
grep -F '"personal"' "$menu" >/dev/null

# A same-user process can reach the socket, so verify an unterminated oversized
# frame is rejected without taking down the daemon or growing an unbounded
# input buffer. Keep socat optional so the core smoke test has no extra runtime
# dependency.
if command -v socat >/dev/null 2>&1; then
  { head -c 140000 /dev/zero | tr '\0' x; printf '\n'; } \
    | timeout 2 socat - "UNIX-CONNECT:$socket" >/dev/null 2>&1 || true
  "$ctl" --socket "$socket" ping | jq -e '.event == "pong"' >/dev/null
fi

echo "daemon IPC smoke test passed"
