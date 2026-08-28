# Omarchy WhatsApp

A low-footprint WhatsApp linked-device client built for Omarchy. The complete
interface is a Quickshell service, bar widget, and on-demand app panel; a small
Rust daemon owns the connection, local history, and native notifications. There
is no Chromium, Electron, WebKit, or GTK UI process.

![Omarchy WhatsApp preview](preview.png)

This project is unofficial and is not affiliated with or endorsed by WhatsApp
or Meta. It uses the reverse-engineered WhatsApp Web linked-device protocol via
[`whatsapp-rust`](https://github.com/oxidezap/whatsapp-rust). Protocol changes or
account policy enforcement can break unofficial clients; use it with that in
mind.

## What is included

- `omarchy-whatsappd`: persistent Rust daemon, SQLite session/history, Unix IPC,
  synchronized chat state, read receipts, private media caches, avatars, and
  native desktop notifications.
- `io.github.bryantebeek.whatsapp`: the full Quickshell app. Its service
  keeps lightweight UI state, its bar widget shows connection/unread state, and
  its panel provides pairing, chats, history, composing, and deep links.
- `omarchy-whatsappctl`: scriptable status, history, send, and read operations.
- A hardened user systemd service, launcher entry, scalable icon, installer,
  uninstaller, and Arch `PKGBUILD`.

The app renders contact, group, and group-participant avatars; encrypted image,
video, and voice messages; documents; static/live-location cards; and
synchronized emoji reactions. Voice notes download on demand and play inline.
Reaction chips aggregate matching emoji and support adding, changing, and
removing your reaction. Location cards open in the system browser using
OpenStreetMap, avoiding an embedded browser engine. Stickers, full calling,
search, and group administration remain lightweight placeholders or out of
scope.

## Install on Omarchy

Requirements are Omarchy 4, Quickshell, SQLite, systemd, and a Rust toolchain.
The repository pins the nightly used by `whatsapp-rust`; if `mise` is installed,
the installer selects it automatically.

Install the disabled plugin checkout through Omarchy, then run its reviewed
installer to build the external Rust daemon, install its user service, and
enable the plugin:

```bash
omarchy plugin add https://github.com/bryantebeek/omarchy-whatsapp.git
~/.config/omarchy/plugins/io.github.bryantebeek.whatsapp/install.sh
```

Omarchy intentionally never executes plugin install hooks. Running the second
command is therefore required; `omarchy plugin add ... --enable` by itself only
installs the shell interface and cannot install the daemon or systemd unit.

For a development checkout elsewhere on disk, run the same installer there:

```bash
./install.sh
```

The installer builds the Rust workspace, starts the user service, installs and
enables the Omarchy plugin, and adds **WhatsApp** to the app launcher. Current
conversations and groups are also indexed under a WhatsApp submenu in Omarchy's
Super+Space menu. Type a contact or group name there to open that conversation
directly. Open the full app from the launcher, click its bar icon, or run:

```bash
omarchy shell io.github.bryantebeek.whatsapp open
```

At first launch, open WhatsApp on your phone, then **Linked devices → Link a
device**, and scan the QR code shown in the app. Closing the app panel unloads
that view; the Rust daemon stays connected so native notifications continue.
Once linked history arrives, the daemon downloads missing profile photos for
the synchronized conversation list; opening each conversation is not required.

If another WhatsApp widget is already installed, disable it yourself after
confirming this client works. The installer does not remove third-party plugins.

Useful diagnostics:

```bash
omarchy-whatsappctl status
omarchy-whatsappctl chats
omarchy-whatsappctl messages 31612345678@s.whatsapp.net
omarchy-whatsappctl send 31612345678@s.whatsapp.net "Hello from Omarchy"
jq '.chats | length' < <(omarchy-whatsappctl chats --limit 500)
journalctl --user -u omarchy-whatsapp.service -f
```

The shell refreshes the conversation search index after chat-list changes. It
contains conversation names, contact/group type, and the deep-link identifier;
message text is never indexed. The generated, marker-delimited block lives in
`~/.config/omarchy/extensions/omarchy-menu.jsonc`, alongside user-owned menu
customizations. It can be refreshed or removed explicitly with:

```bash
omarchy-whatsappctl launcher-sync
omarchy-whatsappctl launcher-remove
```

Uninstall application files while preserving the paired session:

```bash
./uninstall.sh
```

Run that script from the installed plugin checkout before using
`omarchy plugin remove`: it also stops and removes the external daemon, service,
launcher, and icon that Omarchy's plugin manager does not own.

To also remove the local linked-device keys and history, use
`./uninstall.sh --purge-data`. That permanently signs this installation out and
cannot be undone.

Installation and upgrades replace deployed files atomically and never modify
the account state directory. Normal uninstall also preserves that directory;
only the explicit `--purge-data` option removes it.

## Privacy and footprint

- The runtime directory is mode `0700`; its socket and pairing QR are `0600`.
- Linked-device cryptographic state lives in
  `~/.local/state/omarchy-whatsapp/session.db`.
- The UI index lives separately in `history.db` and retains at most 1,000 text
  entries per chat. Raster images are limited to 25 MiB each and the private
  media cache is pruned to 256 MiB; profile previews are capped at 1 MiB each
  and 64 MiB total.
- Notification content is passed as argv, never through a shell. Clicking a
  notification asks Omarchy Shell to open the app at the relevant conversation.
- Offline history sync is indexed but does not create desktop notifications.
- The systemd service has a read-only home view except for its private state
  directory and can access only Unix, IPv4, and IPv6 sockets.

The panel is part of the already-running Omarchy Shell process and is created
only while visible. Closing it therefore leaves only the incremental service
state in Quickshell plus the protocol daemon; it does not leave a separate web
browser or GUI process running. Exact memory depends on account size and active
history sync. Inspect it with:

```bash
systemctl --user status omarchy-whatsapp.service
systemd-cgtop --user
```

## Omarchy themes

The interface imports Omarchy Shell's `Color` and `Style` singletons directly.
Surfaces, borders, text, accent color, spacing, corner radii, and fonts therefore
use the active Omarchy theme rather than a copied or hard-coded palette. When
Omarchy changes theme, the open app updates through the same live shell theme
state as built-in panels and widgets. The Rust daemon remains appearance-free.

## Development

```bash
./scripts/check.sh
./scripts/coverage.sh
./scripts/cargo.sh deny check
./scripts/cargo.sh build --release --locked --workspace
./tests/smoke.sh
```

`tests/smoke.sh` uses an isolated temporary state directory and socket. It does
not touch the installed service or paired account.

The local checks are mirrored by required GitHub Actions jobs for formatting,
strict Clippy linting, unit tests, coverage, release smoke testing, dependency
policy, dependency review, and CodeQL. See [QUALITY.md](QUALITY.md) for the exact
coverage contract and merge-policy setup.

`scripts/check.sh` validates the root marketplace manifest and entry points.
On Omarchy it also runs strict `qmllint` against the installed shell modules;
hosted CI, where those private modules are unavailable, still performs QML
syntax and formatting checks.

The IPC protocol is newline-delimited JSON. Every request may carry an `id`; a
matching response carries the same value, while broadcasts omit it. For example:

```json
{"id":1,"command":"get_state"}
{"id":1,"event":"state","status":{"state":"connected"},"unread_total":0}
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the process model and invariants.

## License

MIT. The WhatsApp name and marks belong to their respective owner.
