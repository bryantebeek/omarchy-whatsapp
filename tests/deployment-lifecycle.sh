#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)

cleanup() {
  rm -rf -- "$test_dir"
}
trap cleanup EXIT

release_daemon="$repo_dir/target/release/omarchy-whatsappd"
release_ctl="$repo_dir/target/release/omarchy-whatsappctl"
[[ -x $release_daemon && -x $release_ctl ]] || {
  echo "Release binaries are missing; run cargo build --release --locked --workspace first." >&2
  exit 1
}

test_home="$test_dir/home"
shim_dir="$test_dir/bin"
systemctl_log="$test_dir/systemctl.log"
mise_log="$test_dir/mise.log"
desktop_log="$test_dir/desktop.log"
real_install=$(command -v install)
test_state_home="$test_dir/xdg-state"
mkdir -p -- "$test_state_home/omarchy-whatsapp" "$shim_dir"

# Keep the test independent of a running user manager. The install shim also
# lets us simulate an interrupted copy before the atomic rename.
# The single-quoted strings are literal source for the generated shim.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  'printf "%s\n" "$*" >>"${SYSTEMCTL_LOG:?}"' \
  >"$shim_dir/systemctl"
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  'destination=${!#}' \
  'if [[ -n ${INSTALL_FAIL_MATCH:-} && $destination == *"$INSTALL_FAIL_MATCH"* ]]; then' \
  '  exit 70' \
  'fi' \
  'exec "${REAL_INSTALL:?}" "$@"' \
  >"$shim_dir/install"
# Release compilation and binary behavior have dedicated gates. Here the mise
# shim verifies setup's build command and supplies those already-tested inputs.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  '[[ $# -ge 5 && $1 == exec && $3 == -- && $4 == cargo && $5 == build ]] || exit 71' \
  'shift 4' \
  'printf "%s\n" "$*" >>"${MISE_LOG:?}"' \
  '"${REAL_INSTALL:?}" -d -m 700 -- "${CARGO_TARGET_DIR:?}/release"' \
  '"$REAL_INSTALL" -m 755 "${TEST_RELEASE_DAEMON:?}" "$CARGO_TARGET_DIR/release/omarchy-whatsappd"' \
  '"$REAL_INSTALL" -m 755 "${TEST_RELEASE_CTL:?}" "$CARGO_TARGET_DIR/release/omarchy-whatsappctl"' \
  >"$shim_dir/mise"
# Changing HOME does not isolate commands that talk to the running desktop.
# Stub both entry points used by install/uninstall and the reload helper.
cat >"$shim_dir/omarchy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'omarchy %s\n' "$*" >>"${DESKTOP_LOG:?}"
case "$*" in
  'shell shell listPlugins')
    printf '%s\n' '[{"id":"io.github.bryantebeek.whatsapp"}]' ;;
  'plugin validate '*) [[ -f $3/manifest.json ]] ;;
  'plugin enable io.github.bryantebeek.whatsapp' | \
  'plugin disable io.github.bryantebeek.whatsapp' | \
  'plugin disable io.github.bryantebeek.whatsapp-native' | \
  'shell shell rescanPlugins' | 'shell shell ping' | 'restart shell') ;;
  *) echo "Unexpected desktop command: $*" >&2; exit 72 ;;
esac
SH
cat >"$shim_dir/qs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'qs %s\n' "$*" >>"${DESKTOP_LOG:?}"
[[ $# == 5 && $1 == ipc && $2 == -n && $3 == -p && $5 == show ]] || exit 73
printf '%s\n' 'target io.github.bryantebeek.whatsapp'
SH
chmod 755 "$shim_dir/systemctl" "$shim_dir/install" "$shim_dir/mise" \
  "$shim_dir/omarchy" "$shim_dir/qs"

export HOME="$test_home"
export XDG_STATE_HOME="$test_state_home"
export XDG_CONFIG_HOME="$test_home/.config"
export XDG_CACHE_HOME="$test_home/.cache"
export XDG_DATA_HOME="$test_home/.local/share"
export XDG_RUNTIME_DIR="$test_dir/runtime"
mkdir -m 700 -- "$XDG_RUNTIME_DIR"
unset DBUS_SESSION_BUS_ADDRESS WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE
export PATH="$shim_dir:/usr/bin:/bin"
export REAL_INSTALL="$real_install"
export SYSTEMCTL_LOG="$systemctl_log"
export MISE_LOG="$mise_log"
export DESKTOP_LOG="$desktop_log"
export TEST_RELEASE_DAEMON="$release_daemon"
export TEST_RELEASE_CTL="$release_ctl"

state_dir="$XDG_STATE_HOME/omarchy-whatsapp"
printf '%s\n' 'fictional linked-device test state' >"$state_dir/session.db"
printf '%s\n' 'fictional local history test state' >"$state_dir/history.db"
session_digest=$(sha256sum "$state_dir/session.db" | cut -d' ' -f1)
history_digest=$(sha256sum "$state_dir/history.db" | cut -d' ' -f1)

assert_state_preserved() {
  [[ -f $state_dir/session.db && -f $state_dir/history.db ]]
  [[ $(sha256sum "$state_dir/session.db" | cut -d' ' -f1) == "$session_digest" ]]
  [[ $(sha256sum "$state_dir/history.db" | cut -d' ' -f1) == "$history_digest" ]]
}

# A failed copy must leave the previous installed file and private state intact.
mkdir -p -- "$HOME/.local/bin"
printf '%s\n' 'previous daemon release' >"$HOME/.local/bin/omarchy-whatsappd"
chmod 755 "$HOME/.local/bin/omarchy-whatsappd"
if INSTALL_FAIL_MATCH='/omarchy-whatsappd.tmp.' \
  "$repo_dir/install.sh" --no-build >/dev/null 2>&1; then
  echo "Installer unexpectedly succeeded during the injected copy failure." >&2
  exit 1
fi
grep -Fx 'previous daemon release' "$HOME/.local/bin/omarchy-whatsappd" >/dev/null
if find "$HOME" -name '*.tmp.*' -print -quit | grep -q .; then
  echo "Interrupted installation left a temporary file behind." >&2
  exit 1
fi
assert_state_preserved

# Initial installation and an in-place update must not alter account data.
"$repo_dir/install.sh" --no-build >/dev/null
cmp "$release_daemon" "$HOME/.local/bin/omarchy-whatsappd"
cmp "$release_ctl" "$HOME/.local/bin/omarchy-whatsappctl"
"$repo_dir/scripts/setup-daemon.sh" check
assert_state_preserved
"$repo_dir/install.sh" --no-build >/dev/null
assert_state_preserved
plugin_manifest="$HOME/.config/omarchy/plugins/io.github.bryantebeek.whatsapp/manifest.json"
plugin_manifest_digest=$(sha256sum "$plugin_manifest" | cut -d' ' -f1)
setup_build_dir="$test_dir/setup-build"
OMARCHY_WHATSAPP_BUILD_DIR="$setup_build_dir" \
  "$repo_dir/scripts/setup-daemon.sh" setup >/dev/null
grep -Fx 'build --release --locked --workspace' "$mise_log" >/dev/null
[[ -x $setup_build_dir/release/omarchy-whatsappd ]]
[[ -x $setup_build_dir/release/omarchy-whatsappctl ]]
[[ $(sha256sum "$plugin_manifest" | cut -d' ' -f1) == "$plugin_manifest_digest" ]]
"$repo_dir/scripts/setup-daemon.sh" check
assert_state_preserved

# Ordinary removal preserves the session; the explicit purge is the sole
# destructive lifecycle operation.
"$repo_dir/uninstall.sh" >/dev/null
assert_state_preserved
[[ ! -e $HOME/.config/systemd/user/omarchy-whatsapp.service ]]
[[ ! -e $HOME/.config/omarchy/plugins/io.github.bryantebeek.whatsapp ]]
"$repo_dir/uninstall.sh" --purge-data >/dev/null
[[ ! -e $state_dir ]]

grep -F -- '--user enable omarchy-whatsapp.service' "$systemctl_log" >/dev/null
grep -F -- '--user restart omarchy-whatsapp.service' "$systemctl_log" >/dev/null
grep -F -- '--user disable --now omarchy-whatsapp.service' "$systemctl_log" >/dev/null
for command in \
  'plugin enable io.github.bryantebeek.whatsapp' \
  'plugin disable io.github.bryantebeek.whatsapp' \
  'shell shell rescanPlugins' 'restart shell' 'shell shell ping'; do
  grep -Fx "omarchy $command" "$desktop_log" >/dev/null
done
grep -F 'qs ipc -n -p ' "$desktop_log" >/dev/null

echo "Deployment lifecycle regression test passed."
