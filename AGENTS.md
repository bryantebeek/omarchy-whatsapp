# Working on Omarchy WhatsApp

## Make local changes visible

This repository is developed and tested on the same Omarchy workstation where
the plugin is installed. Unless the user explicitly asks for a source-only
change, do not stop after editing repository files: deploy the result so the
user can immediately see or exercise it.

### Quickshell-only changes

For changes under `quickshell/`:

1. Run `/usr/lib/qt6/bin/qmlformat -n quickshell/*.qml`,
   `./scripts/qml-lint.sh`, and `omarchy plugin validate .`.
2. Copy the changed QML and JavaScript files into the installed plugin's
   `quickshell/` directory, preserving file modes. If `manifest.json` changed,
   copy it to the installed plugin root. The repository's `install.sh` shows
   the canonical file mapping.
3. Validate the installed plugin directory.
4. Run `./scripts/reload-quickshell.sh`. Do not replace it with a raw
   `rescanPlugins` followed by `omarchy restart shell`: plugin reload is
   asynchronous, and the helper waits for the WhatsApp `IpcHandler` to remain
   stable before restarting.
5. Confirm that the shell restarted successfully and that its recent logs do
   not contain QML loading errors before telling the user the update is live.

Do not rebuild or restart the Rust daemon for a QML-only change unless the QML
change depends on a daemon/protocol change.

### Rust, protocol, packaging, or mixed changes

For changes to Rust code, the IPC protocol, service files, packaging, or a mix
of daemon and UI files:

1. Run the relevant formatting, checks, and tests from the README.
2. Run `./install.sh` without `--no-build`. This must build the current source,
   install the binaries and plugin files, restart the user service, rescan the
   shell plugin with `scripts/reload-quickshell.sh`, wait for that reload to
   settle, and restart the shell.
3. Verify `omarchy-whatsapp.service` is active and inspect its recent
   logs for startup failures.

Never use `./install.sh --no-build` after a Rust change: that can silently
install a stale release binary. If building, validation, installation, or
reload fails, report the failure and do not claim the change is visible.

### Pairing identity changes

WhatsApp records device properties only during initial pairing. After deploying
a change to the linked-device identity, explicitly tell the user to remove the
existing entry in WhatsApp's **Linked devices** and scan a new QR code. Restarting
the daemon alone cannot rename an existing linked device.

### Preserve local account data

Deployment and reloads must preserve the paired account and local history. Do
not remove the state under `~/.local/state/omarchy-whatsapp` unless the user
explicitly requests a logout/reset and understands that relinking is required.
