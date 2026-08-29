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
- `omarchy-whatsappctl`: scriptable status, history, send, poll, and read
  operations.
- A hardened user systemd service, launcher entry, scalable icon, installer,
  uninstaller, and Arch `PKGBUILD`.

The app renders contact, group, and group-participant avatars; online and
last-seen presence; direct and group typing/recording indicators; encrypted
image, video, and voice messages; documents; static/live-location cards; and
static and animated WebP stickers, downloaded automatically when their
conversation loads. It also synchronizes emoji reactions. Lottie
stickers show their embedded static preview without passing sender-controlled
animation JSON to Qt's in-process Lottie renderer. Poll cards show live vote
totals and let you vote or revise your selection; new single- or multiple-answer
polls can be created from the composer. Voice notes can be recorded, uploaded,
sent, downloaded on demand, and played inline. Reaction chips aggregate matching
emoji and support adding, changing, and removing your reaction. Location cards
open in the system browser using OpenStreetMap, avoiding an embedded browser
engine. Full calling, search, and group administration are not yet implemented.
The roadmap below separates package-backed work from app-specific scope.

## Roadmap

This roadmap tracks user-facing capabilities exposed by the pinned
`whatsapp-rust` release that are not yet available end-to-end through the
daemon, IPC protocol, and Quickshell interface. A synchronized setting or a
text placeholder does not count as complete until the user can inspect and
operate it in this app.

The order is directional rather than a release promise. Each item also needs
the corresponding Rust, IPC, QML, workflow, coverage, mutation, packaging, and
live-deployment work required by this repository's quality gates.

### Rich messaging

- [ ] Upload and send images, videos, GIFs, documents, and regular audio.
- [x] Record, upload, and send Ogg Opus voice notes, including recording-state
  updates, idempotent retry, crash recovery, and private local-cache retention.
- [ ] Reply to and quote messages, including group-participant context.
- [ ] Compose user and group mentions.
- [ ] Forward existing text and media messages with the correct forwarded
  metadata.
- [ ] Edit sent messages.
- [ ] Delete a message for everyone, including group-admin revocation where
  permitted.
- [ ] Delete a message only for this account and optionally remove its cached
  media.
- [ ] Keep or unkeep messages in disappearing chats.
- [ ] Pin and unpin individual messages, with supported pin durations.
- [ ] Send played receipts after voice and video messages are actually played.
- [x] Render and automatically download static and animated WebP stickers, with
  private PNG previews and accessibility-label fallbacks.
- [ ] Render Lottie sticker animation in a sandboxed helper. Until then, show
  its embedded PNG preview rather than loading untrusted animation JSON in the
  shell process.
- [ ] Fetch first-party sticker packs and create, upload, and send custom
  sticker packs.
- [ ] Create structured events with descriptions, times, locations, call
  links, and guest policy; render events and send RSVP responses.
- [ ] Render and send encrypted comments on Community Announcement posts.
- [ ] Add structured handling for contact cards and group invites instead of
  reducing them to text placeholders.
- [ ] Add outbound contact-card, static-location, and live-location messages
  through the package's generic message API.

### Chats, history, and organization

- [ ] Archive and unarchive chats.
- [ ] Mute, timed-mute, and unmute chats.
- [ ] Star and unstar messages, and expose a starred-message view.
- [ ] Mark chats unread as well as read.
- [ ] Delete chats and clear chat history, with explicit starred-message and
  cached-media choices.
- [ ] Mute and unmute individual contacts' status updates.
- [ ] Save or rename contacts and optionally synchronize them to the primary
  phone address book.
- [ ] Create, rename, recolor, and delete labels.
- [ ] Associate and disassociate labels with chats, then expose label filtering
  in the interface.
- [ ] Configure the account-default disappearing-message duration.
- [ ] Configure disappearing messages for direct chats and groups.
- [ ] Apply message-expiry behavior in local history rather than only storing
  synchronized disappearing-mode metadata.

### Contacts, profiles, safety, and privacy

- [ ] Check whether a phone number is registered on WhatsApp before opening a
  new conversation.
- [ ] Show full contact information, about text, full-size profile pictures,
  verified names, and business profiles.
- [ ] Change this account's push name, about/status text, and profile picture,
  including picture removal.
- [ ] Block and unblock contacts, list blocked contacts, and show blocked state.
- [ ] Report direct or group messages as spam with the available report flows.
- [ ] Fetch and edit last-seen, online, profile-photo, about/status, group-add,
  read-receipt, call-add, message, and defense-mode privacy settings.
- [ ] Manage per-contact privacy exclusion lists for supported categories.

### Groups

- [ ] Create and leave groups.
- [ ] Edit group subjects, descriptions, and profile pictures.
- [ ] Add and remove participants, including removal from linked community
  groups.
- [ ] Promote and demote group administrators.
- [ ] Create, inspect, revoke, and join through group invite links and V4
  invites.
- [ ] View, approve, reject, cancel, and revoke membership requests.
- [ ] Configure locked and announcement modes.
- [ ] Configure who may add members, share invite links, and share history with
  new members.
- [ ] Configure membership approval, group-history sharing, frequently
  forwarded-message restrictions, admin reporting, and sharing limits.
- [ ] Display the full group metadata already available from the package,
  including creation, ownership, permissions, suspension, and community state.
- [ ] Set or clear per-group member labels.
- [ ] Use batch group-info and profile-picture queries where they improve large
  account synchronization.

### Communities

- [ ] Create and deactivate communities.
- [ ] Create community subgroups.
- [ ] Link and unlink existing subgroups.
- [ ] List communities and their subgroups, participant counts, and linked
  participants.
- [ ] Join subgroups and remove participants across community structures.

### Newsletters and status

- [ ] List subscribed newsletters and show newsletter metadata and history.
- [ ] Create, join, leave, and update newsletters, including invite lookup.
- [ ] Send newsletter messages and support newsletter reactions, edits, and
  revocation.
- [ ] Configure follower/admin newsletter mute and live-update subscriptions.
- [ ] Show received WhatsApp status/story posts instead of filtering the status
  broadcast from the messenger UI.
- [ ] Post and revoke text, image, and video statuses.
- [ ] Configure status recipients with contacts, allow-list, and deny-list
  privacy modes.

### Presence and chat state

- [x] Publish available and unavailable presence.
- [x] Subscribe and unsubscribe from contact presence, then display online and
  last-seen state.
- [x] Receive and display direct and group typing and recording indicators.
- [x] Send composing and paused chat-state updates from the text composer.
- [x] Send recording chat-state while capturing a voice note.

### Linking and calls

- [ ] Add phone-number pair-code linking, including custom codes, refresh,
  cancellation, and error recovery.
- [ ] Add WebAuthn/passkey linking and confirmation flows.
- [ ] Let the user reject an incoming call and explicitly terminate signaling;
  these controls are available without compiling the media runtime.
- [ ] Enable an appropriate optional `whatsapp-rust` VoIP profile and implement
  1:1 audio/video calling.
- [ ] Add native group calls and calls bound to existing WhatsApp groups.
- [ ] Create, preview, join, and manage audio/video call links and waiting
  rooms.
- [ ] Add in-call participant invitation/ringing, mute, hand raise, reactions,
  video upgrades, screen sharing, and hangup controls.

### Scope notes

- Message-content search remains a separate product feature: the pinned
  package does not expose a comparable high-level server-side search API.
- Custom storage, transports, runtimes, native library plugins, telemetry,
  Signal internals, retry machinery, raw nodes, and arbitrary protobuf fields
  are integration primitives rather than WhatsApp UI roadmap items.
- Full VoIP is not part of the current dependency feature set and will increase
  binary size and require audio/video device integration. Call rejection and
  termination do not require that optional media feature.

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
omarchy-whatsappctl poll-create 31612345678@s.whatsapp.net "Lunch?" -o Soup -o Salad
omarchy-whatsappctl poll-vote 31612345678@s.whatsapp.net POLL_MESSAGE_ID -o Soup
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
- The UI index lives separately in `history.db` and retains at most 1,000
  messages per chat. Poll creation secrets and each participant's latest vote
  stay in this private database and are not exposed through shell IPC. Raster
  images are limited to 25 MiB each and the private media cache is pruned to
  256 MiB; profile previews are capped at 1 MiB each and 64 MiB total. Voice
  recordings are created in an owner-only outbox and structurally validated as
  Ogg Opus. Failed sends can be retried with the same delivery ID; the outbox is
  capped at eight entries and 64 MiB with seven-day retention, while abandoned
  recordings expire after 24 hours. Successful audio is copied into the private
  media cache before its outbox entry is removed.
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
./scripts/qml-coverage.sh
./scripts/qml-mutation.sh
./scripts/cargo.sh deny check
./scripts/cargo.sh build --release --locked --workspace
./tests/smoke.sh
```

`tests/smoke.sh` uses an isolated temporary state directory and socket. It does
not touch the installed service or paired account.

The local checks are mirrored by required GitHub Actions jobs for formatting,
strict Clippy linting, Rust unit tests and coverage, each of the four QML test
suites, QML behavioral-contract coverage, QML mutation testing, release smoke
testing, dependency policy, dependency review, and CodeQL. See
[QUALITY.md](QUALITY.md) for the exact coverage contract and merge-policy setup.

`scripts/check.sh` validates the root marketplace manifest and entry points. It
also runs portable Qt Quick unit, component, service-state, and UI workflow
tests against side-effect-free Quickshell and Omarchy test doubles. The QML
behavioral contract requires every `Model.js` helper, every `Service.qml` event,
every production entry point, and every required workflow to be covered. A
semantic mutation suite must kill every checked-in mutant. On Omarchy, the same
check additionally runs strict `qmllint` against the installed shell modules;
hosted CI uses the portable test doubles where those modules are unavailable.

The IPC protocol is newline-delimited JSON. Every request may carry an `id`; a
matching response carries the same value, while broadcasts omit it. For example:

```json
{"id":1,"command":"get_state"}
{"id":1,"event":"state","status":{"state":"connected"},"unread_total":0}
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the process model and invariants.

## License

MIT. The WhatsApp name and marks belong to their respective owner.
