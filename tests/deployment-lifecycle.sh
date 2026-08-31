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
chmod 755 "$shim_dir/systemctl" "$shim_dir/install"

export HOME="$test_home"
export XDG_STATE_HOME="$test_state_home"
export PATH="$shim_dir:/usr/bin:/bin"
export REAL_INSTALL="$real_install"
export SYSTEMCTL_LOG="$systemctl_log"

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
OMARCHY_WHATSAPP_BUILD_DIR="$repo_dir/target" \
  "$repo_dir/scripts/setup-daemon.sh" setup >/dev/null
"$repo_dir/scripts/setup-daemon.sh" check
assert_state_preserved

# The in-plugin setup path builds outside the watched plugin checkout and then
# installs only runtime files, leaving the already-enabled plugin tree alone.
external_target="$test_dir/external-target"
mkdir -p -- "$external_target/release"
install -m 755 "$release_daemon" "$external_target/release/omarchy-whatsappd"
install -m 755 "$release_ctl" "$external_target/release/omarchy-whatsappctl"
plugin_manifest="$HOME/.config/omarchy/plugins/io.github.bryantebeek.whatsapp/manifest.json"
plugin_manifest_digest=$(sha256sum "$plugin_manifest" | cut -d' ' -f1)
CARGO_TARGET_DIR="$external_target" \
  "$repo_dir/install.sh" --no-build --runtime-only >/dev/null
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

echo "Deployment lifecycle regression test passed."
