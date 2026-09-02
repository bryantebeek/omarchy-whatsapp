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
      ├── outbox/      temporary voice-note recordings
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
without signing out. The daemon reconnects independently. Startup refuses to
replace a live daemon socket or a non-socket path; only a confirmed stale Unix
socket is removed, preventing two processes from owning the same linked session.

The plugin service owns the single shell-side IPC connection and normalized UI
state. Both the bar widget and app panel consume it. The service is on-demand and
remains loaded while the plugin is enabled; the heavier panel object is created
only while summoned and destroyed when hidden.

When an enabled marketplace checkout has no matching daemon runtime, the panel
offers an explicit setup action. Its repository-owned helper builds into the
user cache rather than the recursively watched plugin directory, then uses the
same atomic runtime-only installation path as the normal installer. Quickshell
never evaluates build output as code, never requests root access, and continues
to communicate only through the daemon socket after systemd starts the service.

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
`(chat_jid, sender_jid, id)`, so equal message IDs from distinct group senders
remain distinct without duplicating unread counts. Before history is persisted,
all encountered WhatsApp LIDs are resolved to the same phone-number identity
used by live events. Alias reconciliation transactionally collapses any legacy
rows stored under both forms, preserving their strongest message and chat
state. Retention is capped per chat and never deletes protocol state.
Schema upgrades inspect existing columns before applying migrations, so an
already-applied migration is idempotent while storage, permission, and corruption
errors remain fatal and visible. A panicked database worker rolls back its active
transaction and later callers recover the mutex instead of cascading the panic.

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
- Connecting always yields a versioned `hello` and current state. The shell does
  not issue commands until the protocol version matches; a partial or mismatched
  installation therefore reports an actionable error instead of corrupting UI
  request state.
- Slow clients may lose broadcasts. The daemon repeats the versioned `hello` and
  current state after lag, which explicitly makes the service refresh chats,
  messages, avatars, and outbox state from authoritative snapshots.
- A connection executes a bounded number of commands in parallel and queues the
  rest; only a client that exceeds the much larger queue bound is rejected with
  `too many active requests`. Each command's deadline covers the queue wait, so
  a queued request fails visibly instead of stalling behind stuck work. Clients
  use one generous safety watchdog rather than duplicating the daemon's
  per-command timeout table.
- Media downloads and avatar requests never run on the command path. They
  validate, ack, and hand the transfer to a deduplicated background queue with
  its own parallelism limit, so slow network work cannot starve cheap commands.
  Outcomes arrive as `media_downloaded`, `media_download_failed`, and `avatars`
  broadcasts, which every connected client observes.

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

The daemon subscribes only to upstream events with production behavior in this
app. Internal routing, device-cache maintenance, and unsupported product areas
remain owned by `whatsapp-rust` or are ignored. Adding a feature means adding
its event subscription and behavioral test at the same time; there is no second
catalog of every upstream enum variant to keep synchronized.

Presence and chat state are intentionally ephemeral. The daemon configures
`whatsapp-rust` for manual global-presence ownership, so library reconnect and
push-name handling cannot briefly advertise the linked device as available.
Every connection remains unavailable while its offline catch-up is pending;
after `OfflineSyncCompleted`, the daemon advertises availability only while the
panel is focused. It still subscribes to presence for the active direct
conversation. The composer sends composing once per typing burst and paused
after inactivity, sending, selection changes, or focus loss.
Incoming online, last-seen, typing, and recording events cross IPC but are never
written to `history.db`; the shell expires typing indicators defensively if a
pause broadcast is lost.

Voice-note capture runs in Qt Multimedia only while the user is recording. The
shell writes Ogg Opus under an opaque recording ID in the owner-only outbox and
publishes recording chat state every eight seconds. The daemon parses the full
Ogg page structure, derives duration from Opus granule positions, and persists a
small private send record before upload. Every retry reuses one WhatsApp message
ID, making a retry safe across disconnects or a daemon crash. A successful send
is copied into the private message-media cache before its outbox files are
removed; a failed send remains available for explicit retry or discard.

Outbox retention is deliberately bounded rather than becoming a general job
system: at most eight failed voice notes and 64 MiB are retained for seven days.
Recordings that never reached the daemon are removed after 24 hours. Interrupted
`sending` records recover as `failed`, and a locally indexed message with the
same delivery ID completes recovery without another network send.

Cross-device regular app-state is replayed once when upgrading to the event-aware
database. Read/unread, pin, mute, archive, deletion/clearing, receipts, contact
changes, and business names are then applied incrementally. Reactions embedded
in live and history message streams are stored as per-sender updates and
exposed as aggregated chips; an empty reaction removes that sender's prior
choice. Local mark-read also writes the app-state action back to WhatsApp so
other devices converge.

Unread state is an event-sourced projection rather than a phone poll. Incoming
messages increment the materialized count; cross-device self-read receipts and
ranged `MarkChatAsRead` actions advance a monotonic per-chat watermark plus
explicit same-second message IDs. The separate explicit-unread marker represents
only WhatsApp's `read=false` app-state action, so ordinary incoming messages
cannot masquerade as a synchronized “mark unread”. Full app-state replay repairs
the projection, while delayed read events cannot clear messages newer than their
wire boundary or resurrect messages already covered by a newer event.

The local unread tables are a materialized projection: they let the shell render
immediately, work while the linked device is temporarily offline, and combine
incremental events without querying the phone. They are not an independent
source of truth. A “local unread reset” would directly zero those tables without
a corresponding WhatsApp event, hiding drift while destroying the evidence
needed to converge correctly. The More-menu **Resync chat state** recovery action
instead asks the daemon to perform a controlled client reconnect, resets only
the linked-device app-state cursors, and rebuilds the projection from WhatsApp's
authoritative event replay. Pairing, messages, media, and local history remain
untouched; the daemon reports requested, syncing, succeeded, or failed state to
the shell over IPC.

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
Every cache/outbox replacement uses a uniquely created owner-only temporary file,
syncs its contents before atomic rename, and syncs the parent directory. Startup
removes abandoned transfer files before cache accounting, so crashes cannot leak
uncounted partial downloads or make concurrent writers share a temporary path.

## Deliberate exclusions

Continuous encrypted live-location updates, Lottie sticker JSON, newsletters,
embedded maps, and a full VoIP stack are not hosted by the app. Static
locations and final live-location snapshots from normal message history still
render, without a second application-owned cryptographic ratchet. Lottie
stickers use their embedded PNG preview because Qt
Lottie explicitly treats its input as trusted, while WhatsApp message content
is sender-controlled. This keeps the always-on process small and avoids pulling
Chromium/WebEngine into Omarchy Shell.
