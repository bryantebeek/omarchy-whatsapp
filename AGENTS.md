# Working on Omarchy WhatsApp

## Protect public repository contents

This is a public repository. Treat every committed or pushed byte as visible to
anyone. Before every commit and push, inspect the staged diff and untracked
files carefully for secrets, credentials, tokens, private keys, WhatsApp account
or session data, personal information, private logs, and machine-specific paths
or identifiers. Never stage local state from `~/.local/state/omarchy-whatsapp`
or other private runtime data. If any content might be private, stop and remove
or redact it before publishing; when uncertain, ask the user before pushing.

## Make local changes visible

This repository is developed and tested on the same Omarchy workstation where
the plugin is installed. Unless the user explicitly asks for a source-only
change, do not stop after editing repository files: deploy the result so the
user can immediately see or exercise it.

## Keep the QML test suites current

Treat the Qt Quick tests under `tests/qml/` as part of the production contract,
not as optional follow-up work. Every new or changed frontend behavior must add
or update tests in the appropriate suite:

- `tst_model.qml` for every helper and edge case in `quickshell/Model.js`,
  including malformed input, escaping, dates, locales, and time zones;
- `tst_components.qml` for component loading, bindings, control states, and
  independently testable panel or bar behavior;
- `tst_service.qml` for every IPC event, state transition, request lifecycle,
  stale or malformed response, reconnect path, and selection change in
  `quickshell/Service.qml`;
- `tst_workflows.qml` for user-visible workflows such as opening and searching
  the panel, selecting chats, composing and sending, loading history, opening
  media, and recovering from connection changes.

Add direct behavioral tests for new service events, production entry points,
public `Model.js` helpers, and user workflows. Add a targeted mutant to
`scripts/qml-mutation.py` only when it represents a high-risk semantic fault in
validation, ordering, connection safety, or a core workflow. Mutation testing
is not a coverage metric; do not add source-text permutations for styling,
spacing, or every branch merely to increase a score.

Keep test doubles and fixtures synthetic and side-effect-free. QML tests must
never connect to the real daemon, read or write paired account state, start user
services, execute desktop commands, or contain captured account data. Keep
stable `objectName` hooks on controls exercised by workflow tests. Run the tests
offscreen through the repository scripts; do not invoke `qmltestrunner`
directly in a way that creates visible windows.

Before declaring any QML or frontend-related change complete, run:

```bash
./scripts/qml-test-all.sh
./scripts/qml-mutation.sh
```

Both commands must pass. Also run the formatting, linting, validation,
deployment, reload, and live-log checks required by the relevant section below.
The CI `Quality` job runs all four suites and the targeted mutation set once.

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
