// Daemon-wide shared state: the `Shared` handle every module receives, the
// coalescing broadcasters that publish revisioned snapshots, and the local
// broadcast helpers built on top of them.

use crate::transport::Transport;
use crate::{assets, connections, database::Database, inbound, jobs, revisions, voice_outbox};
use anyhow::{Result, bail};
use omarchy_whatsapp_protocol::{
    ChatStateResyncStatus, ConnectionStatus, Resource, ServerEvent, ServerFrame,
};
use qrcode::{QrCode, render::svg};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex as StdMutex, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::{Mutex, Notify, RwLock, Semaphore, broadcast, mpsc, oneshot};
use tracing::warn;
use whatsapp_rust::prelude::*;

/// Coalescing state for the avatar directory. A bounded sync writes up to
/// `AVATAR_SYNC_LIMIT` files, so scanning and publishing the complete jid list
/// per file would push slow IPC clients into a lagged resync. Folding a burst
/// into one scan keeps the wire shape while emitting a single event.
#[derive(Default)]
struct AvatarCoalescer {
    fingerprints: std::collections::BTreeMap<String, assets::AvatarFingerprint>,
    changed_jids: std::collections::BTreeSet<String>,
    seeded: bool,
    flush_scheduled: bool,
}

impl AvatarCoalescer {
    /// Marks the directory dirty and reports whether this caller owns the
    /// single pending flush.
    fn mark_dirty(&mut self) -> bool {
        !std::mem::replace(&mut self.flush_scheduled, true)
    }

    /// Folds one directory scan into the pending change set and returns every
    /// jid that changed since the previous flush.
    fn flush(
        &mut self,
        current: std::collections::BTreeMap<String, assets::AvatarFingerprint>,
    ) -> Vec<String> {
        self.flush_scheduled = false;
        for jid in current.keys().chain(self.fingerprints.keys()) {
            if current.get(jid) != self.fingerprints.get(jid) {
                self.changed_jids.insert(jid.clone());
            }
        }
        self.fingerprints = current;
        self.seeded = true;
        std::mem::take(&mut self.changed_jids).into_iter().collect()
    }

    /// Adopts the first scan served to a client as the baseline. Later
    /// snapshots keep the previous baseline so a pending flush still announces
    /// the files it has not published yet.
    fn seed(&mut self, current: std::collections::BTreeMap<String, assets::AvatarFingerprint>) {
        if !self.seeded {
            self.fingerprints = current;
            self.seeded = true;
        }
    }
}

/// Owns the avatar revision counter and the coalesced `avatars` broadcast. It
/// is held behind an `Arc` so a delayed flush needs the publishing state only,
/// not the whole daemon.
pub(crate) struct AvatarBroadcaster {
    events: broadcast::Sender<ServerFrame>,
    clock: Arc<revisions::RevisionClock>,
    revision: AtomicU64,
    state: StdMutex<AvatarCoalescer>,
}

impl AvatarBroadcaster {
    pub(crate) fn new(
        events: broadcast::Sender<ServerFrame>,
        clock: Arc<revisions::RevisionClock>,
    ) -> Self {
        Self {
            events,
            clock,
            revision: AtomicU64::new(0),
            state: StdMutex::new(AvatarCoalescer::default()),
        }
    }

    fn mark_dirty(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mark_dirty()
    }

    /// Publishes at most one `avatars` event covering every change since the
    /// previous flush. The single scan serves both the diff and the list, and
    /// runs inside the critical section so a write that folded into this flush
    /// cannot land between the scan and the diff.
    fn flush(&self, directory: &Path) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = assets::avatar_fingerprints(directory);
        let jids = current.keys().cloned().collect::<Vec<_>>();
        let changed_jids = state.flush(current);
        drop(state);
        if changed_jids.is_empty() {
            return;
        }
        let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self
            .events
            .send(self.clock.stamp_event(ServerEvent::Avatars {
                revision,
                jids,
                changed_jids,
            }));
    }

    fn snapshot(&self, directory: &Path) -> ServerEvent {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = assets::avatar_fingerprints(directory);
        let jids = current.keys().cloned().collect::<Vec<_>>();
        state.seed(current);
        drop(state);
        ServerEvent::Avatars {
            revision: self.revision.load(Ordering::Relaxed),
            jids,
            changed_jids: Vec::new(),
        }
    }
}

/// Per-key leading and trailing coalescing state for invalidation broadcasts.
/// The first call for a key publishes immediately and opens a window; further
/// calls inside that window collapse into one trailing publish.
#[derive(Default)]
struct InvalidationCoalescer {
    windows: HashMap<String, bool>,
}

impl InvalidationCoalescer {
    /// Returns whether the caller publishes now and owns the new window.
    fn record(&mut self, key: &str) -> bool {
        if let Some(trailing) = self.windows.get_mut(key) {
            *trailing = true;
            return false;
        }
        self.windows.insert(key.to_owned(), false);
        true
    }

    /// Ends one window. A due trailing publish keeps a fresh window open, so a
    /// steady stream of events stays bounded to one publish per window.
    fn finish(&mut self, key: &str) -> bool {
        let Some(trailing) = self.windows.get_mut(key) else {
            return false;
        };
        if std::mem::replace(trailing, false) {
            return true;
        }
        self.windows.remove(key);
        false
    }

    /// Closes a window without a trailing publish.
    fn cancel(&mut self, key: &str) {
        self.windows.remove(key);
    }
}

/// A local snapshot the shell has to reload. `Unread` carries no total because
/// a coalesced window must report the count when it ends, not when it opened.
#[derive(Clone)]
enum Invalidation {
    Chats,
    Messages(String),
    Unread,
}

impl Invalidation {
    fn key(&self) -> String {
        match self {
            Self::Chats => "chats".to_owned(),
            Self::Messages(chat_jid) => format!("messages:{chat_jid}"),
            Self::Unread => "unread".to_owned(),
        }
    }
}

/// Publishes invalidations with a per-key coalescing window. Every receipt in
/// a group and every incoming message invalidates the same few snapshots, so
/// publishing each one separately makes the shell reload them per event.
pub(crate) struct InvalidationBroadcaster {
    database: Arc<Database>,
    events: broadcast::Sender<ServerFrame>,
    clock: Arc<revisions::RevisionClock>,
    state: StdMutex<InvalidationCoalescer>,
}

impl InvalidationBroadcaster {
    pub(crate) fn new(
        database: Arc<Database>,
        events: broadcast::Sender<ServerFrame>,
        clock: Arc<revisions::RevisionClock>,
    ) -> Self {
        Self {
            database,
            events,
            clock,
            state: StdMutex::new(InvalidationCoalescer::default()),
        }
    }

    fn publish(&self, invalidation: &Invalidation) {
        let event = match invalidation {
            Invalidation::Chats => ServerEvent::Invalidated {
                resource: Resource::Chats,
                key: None,
            },
            Invalidation::Messages(chat_jid) => ServerEvent::Invalidated {
                resource: Resource::Messages,
                key: Some(chat_jid.clone()),
            },
            Invalidation::Unread => match self.database.unread_total() {
                Ok(total) => ServerEvent::Unread { total },
                Err(error) => {
                    warn!(%error, "could not publish WhatsApp unread state");
                    return;
                }
            },
        };
        let _ = self.events.send(self.clock.stamp_event(event));
    }

    fn schedule(self: &Arc<Self>, invalidation: Invalidation) {
        let key = invalidation.key();
        let leading = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(&key);
        if !leading {
            return;
        }
        self.publish(&invalidation);
        // Without a runtime there is nothing to schedule the trailing publish
        // on, so every call keeps publishing on its leading edge instead.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cancel(&key);
            return;
        };
        let broadcaster = Arc::clone(self);
        runtime.spawn(async move {
            broadcaster.drain_windows(key, invalidation).await;
        });
    }

    fn finish_window(&self, key: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish(key)
    }

    async fn drain_windows(&self, key: String, invalidation: Invalidation) {
        loop {
            tokio::time::sleep(jobs::INVALIDATION_WINDOW).await;
            if !self.finish_window(&key) {
                return;
            }
            self.publish(&invalidation);
        }
    }
}

/// Bounded negative cache for the `list_chats` phone-number backfill. A LID
/// chat without a mapping keeps that answer until the client generation
/// changes, so the hot list path stops re-querying the SDK for it.
#[derive(Default)]
pub(crate) struct PhoneNumberMisses {
    generation: u64,
    jids: HashSet<String>,
}

impl PhoneNumberMisses {
    fn remember(&mut self, generation: u64, jid: &str) {
        self.retain_generation(generation);
        self.jids.insert(jid.to_owned());
    }

    fn contains(&mut self, generation: u64, jid: &str) -> bool {
        self.retain_generation(generation);
        self.jids.contains(jid)
    }

    fn retain_generation(&mut self, generation: u64) {
        if self.generation != generation {
            self.generation = generation;
            self.jids.clear();
        }
    }
}

pub(crate) enum MessageWork {
    Drain {
        generation: u64,
        transport: Arc<dyn Transport>,
    },
    Live {
        generation: u64,
        message: Arc<wa::Message>,
        info: Box<MessageInfo>,
        transport: Arc<dyn Transport>,
        key: inbound::InboundKey,
    },
    Barrier(oneshot::Sender<()>),
}

pub(crate) struct AppEventWork {
    pub(crate) generation: u64,
    pub(crate) event: Arc<Event>,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) jobs: Arc<GenerationJobs>,
}

#[derive(Default)]
pub(crate) struct GenerationJobs {
    handles: StdMutex<Vec<tokio::task::AbortHandle>>,
}

impl GenerationJobs {
    // Tokio's generic task wrapper is process orchestration; job cancellation
    // semantics are verified by behavior tests without counting monomorphized
    // copies at every upstream adapter call site.
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let handle = tokio::spawn(future);
        let mut handles = self
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        handles.retain(|registered| !registered.is_finished());
        handles.push(handle.abort_handle());
        handle
    }

    pub(crate) fn abort_all(&self) {
        for handle in self
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            handle.abort();
        }
    }
}

pub(crate) struct Shared {
    pub(crate) database: Arc<Database>,
    pub(crate) status: RwLock<ConnectionStatus>,
    pub(crate) client: RwLock<Option<Arc<dyn Transport>>>,
    pub(crate) events: broadcast::Sender<ServerFrame>,
    pub(crate) clock: Arc<revisions::RevisionClock>,
    pub(crate) avatars: Arc<AvatarBroadcaster>,
    pub(crate) invalidations: Arc<InvalidationBroadcaster>,
    pub(crate) connection_intents: StdMutex<connections::ConnectionIntents>,
    pub(crate) pairing_qr: PathBuf,
    pub(crate) contact_sync_marker: PathBuf,
    pub(crate) contact_history_marker: PathBuf,
    pub(crate) event_sync_marker: PathBuf,
    pub(crate) avatar_dir: PathBuf,
    pub(crate) media_dir: PathBuf,
    pub(crate) clipboard: Arc<dyn crate::paste::ClipboardBackend>,
    pub(crate) voice_outbox_dir: PathBuf,
    pub(crate) presence_sync_generation: AtomicU64,
    pub(crate) app_state_failed: AtomicBool,
    pub(crate) app_state_activity_ms: AtomicU64,
    pub(crate) app_state_notify: Notify,
    pub(crate) chat_state_resync: RwLock<(ChatStateResyncStatus, Option<String>)>,
    pub(crate) chat_state_resync_requested: AtomicBool,
    pub(crate) chat_state_resync_notify: Notify,
    pub(crate) logout_requested: AtomicBool,
    pub(crate) logout_trigger: StdMutex<Option<oneshot::Sender<()>>>,
    pub(crate) phone_number_misses: StdMutex<PhoneNumberMisses>,
    pub(crate) avatar_sync: Mutex<()>,
    pub(crate) group_name_sync: Mutex<()>,
    pub(crate) media_recovery_requested: RwLock<HashSet<String>>,
    pub(crate) media_downloads: Mutex<HashSet<String>>,
    pub(crate) media_download_permits: Semaphore,
    pub(crate) avatar_fetches: Mutex<HashSet<String>>,
    pub(crate) avatar_fetch_permits: Semaphore,
    pub(crate) voice_outbox_gate: Mutex<()>,
    pub(crate) text_outbox_gate: Mutex<()>,
    pub(crate) read_outbox_gate: Mutex<()>,
    pub(crate) text_outbox_notify: Notify,
    pub(crate) read_outbox_notify: Notify,
    pub(crate) command_gates: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    pub(crate) message_reducer: mpsc::Sender<MessageWork>,
    pub(crate) app_event_reducer: mpsc::Sender<AppEventWork>,
}

impl Shared {
    pub(crate) fn publish(&self, event: ServerEvent) {
        let _ = self.events.send(self.clock.stamp_event(event));
    }

    pub(crate) fn response(&self, id: Option<u64>, event: ServerEvent) -> ServerFrame {
        self.clock.stamp_response(id, event)
    }

    /// Installs this generation's logout channel. Each generation owns its
    /// own channel, so a trigger armed for a previous client can never stop
    /// the current run loop.
    pub(crate) fn arm_logout(&self) -> oneshot::Receiver<()> {
        let (trigger, signal) = oneshot::channel();
        *self
            .logout_trigger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(trigger);
        signal
    }

    /// Takes this generation's logout trigger. It is single use: a second
    /// logout while one is armed finds nothing and is rejected instead of
    /// arming a second account wipe.
    pub(crate) fn take_logout_trigger(&self) -> Option<oneshot::Sender<()>> {
        self.logout_trigger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn phone_number_is_missing(&self, jid: &str) -> bool {
        let generation = self.clock.generation();
        self.phone_number_misses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(generation, jid)
    }

    pub(crate) fn remember_missing_phone_number(&self, jid: &str) {
        let generation = self.clock.generation();
        self.phone_number_misses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remember(generation, jid);
    }

    pub(crate) fn open_connection(&self) -> u64 {
        self.connection_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connect()
    }

    pub(crate) fn close_connection(
        &self,
        connection_id: u64,
    ) -> (connections::ConnectionState, connections::ConnectionState) {
        let mut connections = self
            .connection_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = connections.state();
        connections.disconnect(connection_id);
        (before, connections.state())
    }

    pub(crate) fn connection_state(&self) -> connections::ConnectionState {
        self.connection_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state()
    }

    pub(crate) fn begin_presence_sync(&self, generation: u64) {
        self.presence_sync_generation
            .store(generation, Ordering::SeqCst);
    }

    pub(crate) fn finish_presence_sync(&self, generation: u64) -> bool {
        self.presence_sync_generation
            .compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn presence_sync_pending(&self) -> bool {
        let generation = self.presence_sync_generation.load(Ordering::SeqCst);
        self.clock.is_current(generation)
    }

    pub(crate) fn set_connection_active_chat(
        &self,
        connection_id: u64,
        chat_jid: Option<String>,
    ) -> Result<(connections::ConnectionState, connections::ConnectionState)> {
        let mut connections = self
            .connection_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = connections.state();
        if !connections.set_active_chat(connection_id, chat_jid) {
            bail!("IPC connection closed");
        }
        Ok((before, connections.state()))
    }

    pub(crate) fn set_connection_available(
        &self,
        connection_id: u64,
        available: bool,
    ) -> Result<(connections::ConnectionState, connections::ConnectionState)> {
        let mut connections = self
            .connection_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = connections.state();
        if !connections.set_available(connection_id, available) {
            bail!("IPC connection closed");
        }
        Ok((before, connections.state()))
    }

    pub(crate) fn chat_is_focused(&self, chat_jid: &str) -> bool {
        self.connection_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_focused(chat_jid)
    }

    pub(crate) async fn command_gate(&self, key: &str) -> Arc<Mutex<()>> {
        let mut gates = self.command_gates.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(key).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(key.to_owned(), Arc::downgrade(&gate));
        gate
    }

    pub(crate) fn unread_total_or_zero(&self) -> u32 {
        match self.database.unread_total() {
            Ok(total) => total,
            Err(error) => {
                warn!(%error, "could not read WhatsApp unread total");
                0
            }
        }
    }

    pub(crate) async fn set_status(&self, status: ConnectionStatus) {
        self.write_pairing_qr(&status);
        *self.status.write().await = status.clone();
        let total = self.unread_total_or_zero();
        self.publish(ServerEvent::State {
            status,
            unread_total: total,
        });
    }

    pub(crate) async fn chat_state_resync_event(&self) -> ServerEvent {
        let (status, message) = self.chat_state_resync.read().await.clone();
        ServerEvent::ChatStateResync { status, message }
    }

    pub(crate) async fn set_chat_state_resync(
        &self,
        status: ChatStateResyncStatus,
        message: Option<String>,
    ) {
        *self.chat_state_resync.write().await = (status, message.clone());
        self.publish(ServerEvent::ChatStateResync { status, message });
    }

    fn write_pairing_qr(&self, status: &ConnectionStatus) {
        let ConnectionStatus::Pairing { code, .. } = status else {
            let _ = std::fs::remove_file(&self.pairing_qr);
            return;
        };
        let Ok(code) = QrCode::new(code.as_bytes()) else {
            warn!("could not encode WhatsApp pairing QR");
            return;
        };
        let svg = code
            .render::<svg::Color>()
            .min_dimensions(512, 512)
            .quiet_zone(true)
            .dark_color(svg::Color("#111111"))
            .light_color(svg::Color("#ffffff"))
            .build();
        let temporary = self.pairing_qr.with_extension("svg.tmp");
        let result = (|| -> std::io::Result<()> {
            std::fs::write(&temporary, svg)?;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&temporary, &self.pairing_qr)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temporary);
            warn!(%error, "could not write WhatsApp pairing QR");
        }
    }

    pub(crate) fn mark_contact_sync_complete(&self) {
        if self.contact_sync_marker.exists() {
            return;
        }
        if let Err(error) = write_private_marker(&self.contact_sync_marker) {
            warn!(%error, "could not mark contact-name sync complete");
        }
    }

    pub(crate) fn mark_event_sync_complete(&self) {
        if self.event_sync_marker.exists() {
            return;
        }
        match self.database.regular_app_state_is_complete() {
            Ok(true) => {
                if let Err(error) = write_private_marker(&self.event_sync_marker) {
                    warn!(%error, "could not mark app-state event sync complete");
                }
            }
            Ok(false) => warn!("WhatsApp app-state sync is incomplete; leaving resync armed"),
            Err(error) => warn!(%error, "could not inspect WhatsApp app-state progress"),
        }
    }

    pub(crate) fn avatars_changed(&self) {
        if !self.avatars.mark_dirty() {
            return;
        }
        let avatars = Arc::clone(&self.avatars);
        let directory = self.avatar_dir.clone();
        // Avatar writes arrive in bursts from the bounded sync and from picture
        // updates. Without a runtime the flush stays inline; otherwise a single
        // delayed task publishes the whole burst.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    tokio::time::sleep(jobs::AVATAR_FLUSH_WINDOW).await;
                    avatars.flush(&directory);
                });
            }
            Err(_) => avatars.flush(&directory),
        }
    }

    pub(crate) fn avatar_snapshot(&self) -> ServerEvent {
        self.avatars.snapshot(&self.avatar_dir)
    }

    pub(crate) async fn state_event(&self) -> ServerEvent {
        ServerEvent::State {
            status: self.status.read().await.clone(),
            unread_total: self.unread_total_or_zero(),
        }
    }
}

pub(crate) fn broadcast_chats(shared: &Shared) {
    shared.invalidations.schedule(Invalidation::Chats);
}

fn broadcast_unread(shared: &Shared) {
    shared.invalidations.schedule(Invalidation::Unread);
}

pub(crate) fn broadcast_messages(shared: &Shared, chat_jid: &str) {
    shared
        .invalidations
        .schedule(Invalidation::Messages(chat_jid.to_owned()));
}

pub(crate) fn voice_outbox_event(shared: &Shared) -> Result<ServerEvent> {
    Ok(ServerEvent::VoiceOutbox {
        entries: voice_outbox::entries(&shared.voice_outbox_dir)?,
    })
}

pub(crate) fn broadcast_voice_outbox(shared: &Shared) {
    match voice_outbox_event(shared) {
        Ok(event) => shared.publish(event),
        Err(error) => warn!(%error, "could not publish voice outbox state"),
    }
}

pub(crate) fn broadcast_text_outbox(shared: &Shared) {
    match shared.database.text_outbox() {
        Ok(entries) => shared.publish(ServerEvent::TextOutbox { entries }),
        Err(error) => warn!(%error, "could not publish text outbox state"),
    }
}

pub(crate) fn broadcast_snapshot(shared: &Shared) {
    broadcast_chats(shared);
    broadcast_unread(shared);
}

pub(crate) fn write_private_marker(path: &Path) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, b"1\n")?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::default_trait_access)] // Generated protobuf fixture types are inferred by MessageField.
mod tests {
    use super::*;
    use crate::commands::canonical_requested_jid;
    use crate::identity::{display_name, ingest_contact_name, metadata_jid};
    use crate::test_support::{advance_past, test_shared, unread_message};
    use chrono::Utc;

    #[test]
    fn avatar_coalescer_reports_every_change_since_the_last_flush() {
        let mut coalescer = AvatarCoalescer::default();
        assert!(coalescer.mark_dirty());
        assert!(!coalescer.mark_dirty());

        let first = std::collections::BTreeMap::from([("a".to_owned(), (1, 2, 3, 4))]);
        assert_eq!(coalescer.flush(first.clone()), vec!["a".to_owned()]);
        assert!(coalescer.flush(first.clone()).is_empty());
        assert!(coalescer.mark_dirty());

        // A snapshot served after the baseline exists must not adopt files the
        // pending flush still has to announce.
        let second = std::collections::BTreeMap::from([
            ("a".to_owned(), (1, 2, 3, 4)),
            ("b".to_owned(), (5, 6, 7, 8)),
        ]);
        coalescer.seed(second.clone());
        assert_eq!(coalescer.flush(second), vec!["b".to_owned()]);
        assert_eq!(coalescer.flush(first), vec!["b".to_owned()]);

        let mut fresh = AvatarCoalescer::default();
        fresh.seed(std::collections::BTreeMap::from([(
            "c".to_owned(),
            (9, 9, 9, 9),
        )]));
        assert!(
            fresh
                .flush(std::collections::BTreeMap::from([(
                    "c".to_owned(),
                    (9, 9, 9, 9)
                )]))
                .is_empty()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn avatar_bursts_publish_one_coalesced_broadcast() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assets::private_dir(&shared.avatar_dir).unwrap();
        let mut events = shared.events.subscribe();

        for jid in ["1@s.whatsapp.net", "2@s.whatsapp.net", "3@s.whatsapp.net"] {
            assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, jid), b"avatar")
                .unwrap();
            shared.avatars_changed();
        }
        assert!(events.try_recv().is_err());

        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Avatars {
                revision: 1,
                jids: vec![
                    "1@s.whatsapp.net".into(),
                    "2@s.whatsapp.net".into(),
                    "3@s.whatsapp.net".into(),
                ],
                changed_jids: vec![
                    "1@s.whatsapp.net".into(),
                    "2@s.whatsapp.net".into(),
                    "3@s.whatsapp.net".into(),
                ],
            }
        );
        assert!(events.try_recv().is_err());

        // A later burst opens a fresh window and keeps the revision monotonic.
        assets::remove_avatar(&shared.avatar_dir, "2@s.whatsapp.net");
        shared.avatars_changed();
        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Avatars {
                revision: 2,
                jids: vec!["1@s.whatsapp.net".into(), "3@s.whatsapp.net".into()],
                changed_jids: vec!["2@s.whatsapp.net".into()],
            }
        );
    }

    #[test]
    fn avatar_changes_without_a_runtime_publish_inline() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assets::private_dir(&shared.avatar_dir).unwrap();
        let mut events = shared.events.subscribe();
        let jid = "1@s.whatsapp.net";
        assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, jid), b"avatar")
            .unwrap();

        shared.avatars_changed();

        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Avatars {
                revision: 1,
                jids: vec![jid.into()],
                changed_jids: vec![jid.into()],
            }
        );
        shared.avatars_changed();
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn invalidation_coalescer_opens_one_window_per_key() {
        let mut coalescer = InvalidationCoalescer::default();
        assert!(coalescer.record("chats"));
        assert!(!coalescer.record("chats"));
        assert!(coalescer.record("unread"));

        // The trailing publish keeps its window open so a steady stream stays
        // bounded to one publish per window.
        assert!(coalescer.finish("chats"));
        assert!(!coalescer.finish("chats"));
        assert!(coalescer.record("chats"));
        assert!(!coalescer.finish("unread"));
        assert!(!coalescer.finish("messages:1@s.whatsapp.net"));

        coalescer.cancel("chats");
        assert!(coalescer.record("chats"));
    }

    #[tokio::test(start_paused = true)]
    async fn invalidations_publish_a_leading_and_one_trailing_event() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let mut events = shared.events.subscribe();

        for _ in 0..3 {
            broadcast_chats(&shared);
            broadcast_messages(&shared, "1@s.whatsapp.net");
            broadcast_unread(&shared);
        }
        let leading = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert_eq!(
            leading,
            vec![
                ServerEvent::Invalidated {
                    resource: Resource::Chats,
                    key: None,
                },
                ServerEvent::Invalidated {
                    resource: Resource::Messages,
                    key: Some("1@s.whatsapp.net".into()),
                },
                ServerEvent::Unread { total: 0 },
            ]
        );

        // The trailing unread total is evaluated when the window ends, not when
        // the folded calls were made.
        shared
            .database
            .insert_message(&unread_message("late"), "Ada", false, true)
            .unwrap();
        advance_past(jobs::INVALIDATION_WINDOW).await;
        let trailing = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert_eq!(
            trailing,
            vec![
                ServerEvent::Invalidated {
                    resource: Resource::Chats,
                    key: None,
                },
                ServerEvent::Invalidated {
                    resource: Resource::Messages,
                    key: Some("1@s.whatsapp.net".into()),
                },
                ServerEvent::Unread { total: 1 },
            ]
        );

        // A window without folded calls closes instead of publishing again.
        advance_past(jobs::INVALIDATION_WINDOW).await;
        assert!(events.try_recv().is_err());
        broadcast_chats(&shared);
        assert!(matches!(
            events.try_recv().unwrap().event,
            ServerEvent::Invalidated {
                resource: Resource::Chats,
                ..
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn coalesced_unread_failures_degrade_without_publishing() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let mut events = shared.events.subscribe();
        broadcast_unread(&shared);
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Unread { total: 0 }
        );
        broadcast_unread(&shared);
        shared
            .database
            .execute_test_sql("DROP TABLE chats")
            .unwrap();

        advance_past(jobs::INVALIDATION_WINDOW).await;

        assert!(events.try_recv().is_err());
    }

    #[test]
    fn invalidations_without_a_runtime_publish_every_call() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let mut events = shared.events.subscribe();

        broadcast_chats(&shared);
        broadcast_chats(&shared);

        assert_eq!(
            std::iter::from_fn(|| events.try_recv().ok()).count(),
            2,
            "without a runtime there is nothing to schedule a trailing publish on"
        );
    }

    #[test]
    fn missing_phone_numbers_are_cached_per_client_generation() {
        let mut misses = PhoneNumberMisses::default();
        assert!(!misses.contains(1, "1@lid"));
        misses.remember(1, "1@lid");
        assert!(misses.contains(1, "1@lid"));
        assert!(!misses.contains(1, "2@lid"));
        assert!(!misses.contains(2, "1@lid"));
        misses.remember(2, "1@lid");
        assert!(misses.contains(2, "1@lid"));

        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assert!(!shared.phone_number_is_missing("1@lid"));
        shared.remember_missing_phone_number("1@lid");
        assert!(shared.phone_number_is_missing("1@lid"));
        let _ = shared.clock.begin_generation();
        assert!(!shared.phone_number_is_missing("1@lid"));
    }

    #[tokio::test]
    async fn logout_triggers_are_single_use_and_scoped_to_one_generation() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assert!(shared.take_logout_trigger().is_none());

        let mut signal = shared.arm_logout();
        let trigger = shared.take_logout_trigger().unwrap();
        assert!(
            shared.take_logout_trigger().is_none(),
            "a second logout must not arm another account wipe"
        );
        trigger.send(()).unwrap();
        assert!(signal.try_recv().is_ok());

        // Re-arming replaces the previous generation's channel, so its trigger
        // can never stop the current run loop.
        let mut stale = shared.arm_logout();
        let mut current = shared.arm_logout();
        assert!(stale.try_recv().is_err());
        shared.take_logout_trigger().unwrap().send(()).unwrap();
        assert!(current.try_recv().is_ok());
    }

    #[tokio::test]
    async fn generation_jobs_abort_pending_work_and_prune_finished_handles() {
        let jobs = GenerationJobs::default();
        let completed = jobs.spawn(async { 7_u8 });
        assert_eq!(completed.await.unwrap(), 7);
        let pending = jobs.spawn(std::future::pending::<()>());
        assert_eq!(jobs.handles.lock().unwrap().len(), 1);
        jobs.abort_all();
        assert!(pending.await.unwrap_err().is_cancelled());
        assert!(jobs.handles.lock().unwrap().is_empty());
        jobs.abort_all();
    }

    #[tokio::test]
    async fn shared_connection_gates_markers_and_state_are_locally_consistent() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = test_shared(&directory);
        assets::private_dir(&shared.avatar_dir).unwrap();
        let connection = shared.open_connection();
        assert!(
            shared
                .set_connection_active_chat(999, Some("ignored".into()))
                .is_err()
        );
        assert!(shared.set_connection_available(999, true).is_err());
        let (_, active) = shared
            .set_connection_active_chat(connection, Some("chat@s.whatsapp.net".into()))
            .unwrap();
        assert!(active.active_chats.contains("chat@s.whatsapp.net"));
        assert!(shared.chat_is_focused("chat@s.whatsapp.net"));
        let (_, available) = shared.set_connection_available(connection, true).unwrap();
        assert!(available.available);
        assert_eq!(shared.connection_state(), available);
        let (before_close, after_close) = shared.close_connection(connection);
        assert_ne!(before_close, after_close);
        let (before_second, after_second) = shared.close_connection(connection);
        assert_eq!(before_second, after_second);

        let first = shared.command_gate("chat").await;
        let same = shared.command_gate("chat").await;
        assert!(Arc::ptr_eq(&first, &same));
        drop(first);
        drop(same);
        let replacement = shared.command_gate("chat").await;
        assert_eq!(Arc::strong_count(&replacement), 1);

        shared.mark_contact_sync_complete();
        shared.mark_contact_sync_complete();
        shared.mark_event_sync_complete();
        assert!(shared.contact_sync_marker.exists());
        assert!(!shared.event_sync_marker.exists());
        assert_eq!(shared.unread_total_or_zero(), 0);
        shared
            .set_chat_state_resync(ChatStateResyncStatus::Syncing, Some("working".into()))
            .await;
        assert_eq!(
            shared.chat_state_resync_event().await,
            ServerEvent::ChatStateResync {
                status: ChatStateResyncStatus::Syncing,
                message: Some("working".into()),
            }
        );
        shared.set_status(ConnectionStatus::Connected).await;
        assert!(matches!(
            shared.state_event().await,
            ServerEvent::State {
                status: ConnectionStatus::Connected,
                ..
            }
        ));

        assert_eq!(
            canonical_requested_jid(&shared, "not a jid").await,
            "not a jid"
        );
        assert_eq!(
            canonical_requested_jid(&shared, "1:2@s.whatsapp.net").await,
            "1@s.whatsapp.net"
        );
        assert_eq!(
            metadata_jid("1:2@s.whatsapp.net", "lid"),
            "1@s.whatsapp.net"
        );
        std::fs::remove_file(&shared.contact_sync_marker).unwrap();
        let blocked = directory.path().join("blocked-contact-marker");
        std::fs::write(&blocked, b"file").unwrap();
        shared.contact_sync_marker = blocked.join("marker");
        shared.mark_contact_sync_complete();
        assert!(!shared.contact_sync_marker.exists());
    }

    #[test]
    fn broadcast_helpers_publish_only_revisioned_local_state() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assets::private_dir(&shared.voice_outbox_dir).unwrap();
        let mut events = shared.events.subscribe();

        broadcast_chats(&shared);
        broadcast_messages(&shared, "chat@s.whatsapp.net");
        broadcast_unread(&shared);
        broadcast_voice_outbox(&shared);
        broadcast_text_outbox(&shared);
        broadcast_snapshot(&shared);

        let frames = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(frames.iter().all(|frame| frame.sequence > 0));
        assert!(frames.iter().any(|frame| matches!(
            frame.event,
            ServerEvent::Invalidated {
                resource: Resource::Chats,
                ..
            }
        )));
        assert!(frames.iter().any(|frame| matches!(
            frame.event,
            ServerEvent::Invalidated { resource: Resource::Messages, ref key }
                if key.as_deref() == Some("chat@s.whatsapp.net")
        )));
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame.event, ServerEvent::VoiceOutbox { .. }))
        );
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame.event, ServerEvent::TextOutbox { .. }))
        );
    }

    #[test]
    fn local_snapshot_failures_degrade_without_panicking() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = test_shared(&directory);
        shared
            .database
            .update_address_book_name("4@s.whatsapp.net", "Ada")
            .unwrap();
        assert_eq!(
            display_name(&shared, &"4@s.whatsapp.net".parse().unwrap()),
            "Ada"
        );
        assert_eq!(
            display_name(&shared, &"5@s.whatsapp.net".parse().unwrap()),
            "5@s.whatsapp.net"
        );

        shared
            .database
            .execute_test_sql("DROP TABLE chats; DROP TABLE contacts; DROP TABLE text_outbox;")
            .unwrap();
        assert_eq!(shared.unread_total_or_zero(), 0);
        broadcast_unread(&shared);
        broadcast_text_outbox(&shared);
        let contact = whatsapp_rust::types::events::ContactUpdate::builder()
            .jid("6@s.whatsapp.net".parse().unwrap())
            .timestamp(Utc::now())
            .action(Box::new(wa::sync_action_value::ContactAction {
                full_name: Some("Synthetic".into()),
                ..Default::default()
            }))
            .from_full_sync(false)
            .build();
        ingest_contact_name(&shared, &contact);

        let blocked = directory.path().join("blocked-outbox");
        std::fs::write(&blocked, b"file").unwrap();
        shared.voice_outbox_dir = blocked;
        broadcast_voice_outbox(&shared);

        let blocked_parent = directory.path().join("blocked-qr-parent");
        std::fs::write(&blocked_parent, b"file").unwrap();
        shared.pairing_qr = blocked_parent.join("pairing.svg");
        shared.write_pairing_qr(&ConnectionStatus::Pairing {
            code: "synthetic".into(),
            expires_at: 1,
        });
        assert!(!shared.pairing_qr.exists());
    }

    #[test]
    fn pairing_status_writes_private_qr_and_connected_removes_it() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);

        shared.write_pairing_qr(&ConnectionStatus::Pairing {
            code: "example WhatsApp pairing payload".into(),
            expires_at: 1_700_000_000,
        });

        let contents = std::fs::read_to_string(&shared.pairing_qr).unwrap();
        assert!(contents.contains("<svg"));
        assert_eq!(
            std::fs::metadata(&shared.pairing_qr)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        shared.write_pairing_qr(&ConnectionStatus::Connected);
        assert!(!shared.pairing_qr.exists());

        shared.write_pairing_qr(&ConnectionStatus::Pairing {
            code: "x".repeat(64 * 1024),
            expires_at: 1_700_000_000,
        });
        assert!(!shared.pairing_qr.exists());
    }

    #[test]
    fn event_sync_marker_requires_every_regular_collection() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = test_shared(&directory);

        shared.mark_event_sync_complete();
        assert!(!shared.event_sync_marker.exists());

        let protocol_db = directory.path().join("session.db");
        std::fs::create_dir(&protocol_db).unwrap();
        shared.mark_event_sync_complete();
        assert!(!shared.event_sync_marker.exists());
        std::fs::remove_dir(&protocol_db).unwrap();

        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        shared.mark_event_sync_complete();
        assert!(!shared.event_sync_marker.exists());
        connection
            .execute_batch(
                "CREATE TABLE app_state_versions (
                    name TEXT NOT NULL,
                    state_data BLOB NOT NULL,
                    device_id INTEGER NOT NULL DEFAULT 1,
                    PRIMARY KEY (name, device_id)
                 );",
            )
            .unwrap();
        for name in ["regular", "regular_low"] {
            connection
                .execute(
                    "INSERT INTO app_state_versions (name, state_data) VALUES (?1, X'00')",
                    [name],
                )
                .unwrap();
        }
        shared.mark_event_sync_complete();
        assert!(!shared.event_sync_marker.exists());

        connection
            .execute(
                "INSERT INTO app_state_versions (name, state_data) VALUES ('regular_high', X'00')",
                [],
            )
            .unwrap();
        let blocked_parent = directory.path().join("blocked");
        std::fs::write(&blocked_parent, b"not a directory").unwrap();
        shared.event_sync_marker = blocked_parent.join("marker");
        shared.mark_event_sync_complete();
        assert!(!shared.event_sync_marker.exists());
        std::fs::remove_file(&blocked_parent).unwrap();
        shared.event_sync_marker = directory.path().join("event-state-v6");
        shared.mark_event_sync_complete();
        assert!(shared.event_sync_marker.exists());
        shared.mark_event_sync_complete();
    }

    #[tokio::test(start_paused = true)]
    async fn avatar_events_only_revise_the_files_that_changed() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assets::private_dir(&shared.avatar_dir).unwrap();
        let mut events = shared.events.subscribe();
        let first_jid = "1@s.whatsapp.net";
        let second_jid = "2@s.whatsapp.net";

        assets::write_private_bytes(
            &assets::avatar_path(&shared.avatar_dir, first_jid),
            b"first avatar",
        )
        .unwrap();
        shared.avatars_changed();
        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Avatars {
                revision: 1,
                jids: vec![first_jid.into()],
                changed_jids: vec![first_jid.into()],
            }
        );

        assets::write_private_bytes(
            &assets::avatar_path(&shared.avatar_dir, second_jid),
            b"second avatar",
        )
        .unwrap();
        shared.avatars_changed();
        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Avatars {
                revision: 2,
                jids: vec![first_jid.into(), second_jid.into()],
                changed_jids: vec![second_jid.into()],
            }
        );

        assets::write_private_bytes(
            &assets::avatar_path(&shared.avatar_dir, first_jid),
            b"replacement avatar",
        )
        .unwrap();
        assert_eq!(
            shared.avatar_snapshot(),
            ServerEvent::Avatars {
                revision: 2,
                jids: vec![first_jid.into(), second_jid.into()],
                changed_jids: Vec::new(),
            }
        );
        shared.avatars_changed();
        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Avatars {
                revision: 3,
                jids: vec![first_jid.into(), second_jid.into()],
                changed_jids: vec![first_jid.into()],
            }
        );

        shared.avatars_changed();
        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;
        assert!(events.try_recv().is_err());
    }
}
