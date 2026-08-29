# Architecture

## Process model

```text
WhatsApp linked-device servers
             │ encrypted WebSocket
             ▼
   omarchy-whatsappd (Rust)
      ├── session.db   protocol keys/state
      ├── history.db   bounded message/event index
      ├── avatars/     bounded profile previews
      ├── media/       bounded image/location cache
      ├── org.freedesktop.Notifications
      └── $XDG_RUNTIME_DIR/omarchy-whatsapp/
             ├── daemon.sock   owner-only NDJSON
             └── pairing.svg  temporary, owner-only
                         │
                         ▼
             Omarchy Shell / Quickshell
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
     service         bar widget      lazy app panel
  socket + state   status/unread   chats/messages/QR
```

There is exactly one protocol owner. Quickshell never opens the session database
and never connects directly to WhatsApp. This prevents competing device sessions,
keeps notification ownership unambiguous, and lets the shell or panel restart
without signing out. The daemon reconnects independently.

The plugin service owns the single shell-side IPC connection and normalized UI
state. Both the bar widget and app panel consume it. The service is on-demand and
remains loaded while the plugin is enabled; the heavier panel object is created
only while summoned and destroyed when hidden.

After a chat snapshot changes, the service debounces `omarchy-whatsappctl
launcher-sync`. The helper requests a fresh snapshot and atomically maintains a
marker-delimited block in the user's Omarchy menu extension. Only conversation
names, contact/group type, and shell-quoted deep links are written; message text
is excluded. Omarchy watches that JSONC file, so Super+Space can search those
descendant rows without restarting the shell. The project uninstaller removes
only its marked block and preserves every user-owned menu entry around it.

When an unpaired QR session expires, the daemon rebuilds only the WhatsApp client
after a short delay. Its PID, IPC socket, UI clients, and databases remain in
place while a fresh QR and expiry timestamp are broadcast.

## State boundaries

`session.db` is created and migrated exclusively by `whatsapp-rust`. It contains
the device identity and protocol persistence required to reconnect after login.

`history.db` is application-owned. It stores only the fields required by the
UI, media references, per-sender emoji reactions, synchronized chat settings,
poll definitions, daemon-private poll creation secrets, each participant's
latest poll selection, and unread handling. Only aggregate poll results and the
local user's selected options cross IPC. Inserts are idempotent on
`(chat_jid, id)`, so offline replay does not duplicate messages or unread
counts. Retention is capped per chat and never deletes protocol state.

The active chat is ephemeral daemon state. The focused, visible Quickshell panel
sets it and clears it when hidden or unfocused. Live incoming messages in that
chat are marked read without producing a redundant notification.

## Theme integration

The panel and widget consume Omarchy Shell's live `Color` and `Style` singletons.
They do not compile in a theme or parse a copied palette. Theme changes therefore
flow to surfaces, text, accent, borders, dimensions, corner radii, and fonts in
the same process as built-in shell components. The daemon and wire protocol stay
appearance-agnostic.

## IPC invariants

- Unix socket parent: mode `0700`; socket: mode `0600`.
- One JSON object per line, capped at 128 KiB by the transport; message text is
  additionally capped at 64 KiB at its semantic boundary.
- Requests optionally contain a numeric `id`. Direct responses copy it;
  broadcasts use no `id`.
- Connecting always yields `hello` and current state, allowing the shell service
  to recover after a daemon restart without reconstructing event streams.
- Slow clients may lose broadcasts. The daemon then sends fresh state, and the
  service refreshes chat/message snapshots when needed.

The shared Rust types in `crates/protocol` are the canonical wire contract.

## Notification policy

Only newly persisted, incoming, live messages outside the focused chat produce
a toast. Offline sync, outgoing echoes, duplicates, and the currently visible
conversation do not. Omarchy receives an argv-safe deep link of the form:

```text
omarchy shell -q io.github.bryantebeek.whatsapp openChat <jid>
```

Generic Linux desktops fall back to `notify-send` without a deep link. Because
the Rust daemon owns notifications, they continue after the app panel closes or
the shell restarts.

Muted chat state is replayed from WhatsApp app-state and suppresses message
toasts. Incoming/missed calls, identity changes, pairing failures, and fatal
sync failures use native event notifications without attempting to implement a
full VoIP surface.

## Event and asset policy

All 61 events exposed by the pinned `whatsapp-rust` release have an explicit
policy. Product-relevant events are consumed by the daemon; internal routing
and device-cache events remain library-owned; newsletters, pair-code/passkey
linking, and profile-about events are explicitly excluded.
An ordered compile-time test fails when upstream appends a new event kind until
it is classified.

Presence and chat state are intentionally ephemeral. While the panel is focused,
the daemon advertises this linked device as available and subscribes to presence
for the active direct conversation. The composer sends composing once per typing
burst and paused after inactivity, sending, selection changes, or focus loss.
Incoming online, last-seen, typing, and recording events cross IPC but are never
written to `history.db`; the shell expires typing indicators defensively if a
pause broadcast is lost.

Cross-device regular app-state is replayed once when upgrading to the event-aware
database. Read/unread, pin, mute, archive, star, deletion/clearing, labels,
receipts, contact changes, disappearing defaults, and business names are then
applied incrementally. Reactions embedded in live and history message streams
are stored as per-sender updates and exposed as aggregated chips; an empty
reaction removes that sender's prior choice. Local mark-read also writes the
app-state action back to WhatsApp so other devices converge.

Poll creation messages retain their encryption secret only in the daemon's
private database. Incoming vote updates are decrypted there, applied with
last-vote-wins semantics per participant, and broadcast as refreshed aggregate
cards. Voting uses the stored secret through `whatsapp-rust`; Quickshell never
receives it.

Avatar previews, message images, and sticker previews are raster-only and
owner-readable. Encrypted message images and WebP stickers are streamed through
the library's authenticated decryptor to a
temporary file and atomically renamed after verification. Images are limited to
25 MiB each and the media directory to 256 MiB; avatar responses are limited to
1 MiB each and 64 MiB total. The shell automatically requests undownloaded WebP
stickers as an active conversation loads. Location thumbnails use the same
private cache.

## Deliberate exclusions

Lottie sticker JSON, newsletters, embedded maps, and a full VoIP stack are not
hosted by the UI. Lottie stickers use their embedded PNG preview because Qt
Lottie explicitly treats its input as trusted, while WhatsApp message content
is sender-controlled. This keeps the always-on process small and avoids pulling
Chromium/WebEngine into Omarchy Shell.
