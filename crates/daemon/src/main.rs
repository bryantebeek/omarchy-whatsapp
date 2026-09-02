#![recursion_limit = "512"]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod assets;
mod commands;
mod connections;
mod database;
mod events;
mod history;
mod identity;
mod inbound;
mod ipc;
mod jobs;
mod messages;
mod notification;
mod outbox;
mod presence;
mod revisions;
mod state;
mod sync;
mod text_outbox;
mod util;
mod voice_outbox;

use crate::database::Database;
use crate::events::{
    APP_EVENT_KINDS, await_app_state_sync, log_whatsapp_event, process_contact_event,
    process_group_event, process_history_event, run_app_event_reducer, run_message_reducer,
};
use crate::identity::reconcile_direct_chat_aliases;
use crate::ipc::{bind_private_listener, serve};
use crate::outbox::{run_read_outbox, run_text_outbox};
use crate::presence::force_reconcile_connection_intent;
use crate::state::{
    AppEventWork, AvatarBroadcaster, GenerationJobs, InvalidationBroadcaster, MessageWork,
    PhoneNumberMisses, Shared, broadcast_chats, broadcast_snapshot,
};
use crate::sync::{
    backfill_video_previews, prepare_contact_name_resync, prepare_event_state_resync,
    request_missing_contact_history, sync_avatars, sync_group_names, sync_missing_contact_names,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;
use omarchy_whatsapp_protocol::{AppPaths, ChatStateResyncStatus, ConnectionStatus, ServerEvent};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::{Mutex, Notify, RwLock, Semaphore, broadcast, mpsc, oneshot};
use tracing::{error, info, warn};
use whatsapp_rust::PresencePolicy;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::store::DevicePropsOverride;

#[derive(Debug, Parser)]
#[command(version, about = "Low-footprint WhatsApp companion daemon for Omarchy")]
struct Options {
    /// Override the runtime socket path (primarily for diagnostics and tests).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Override the persistent state directory.
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn remove_sqlite_store(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut artifact = path.as_os_str().to_os_string();
        artifact.push(suffix);
        let artifact = PathBuf::from(artifact);
        match std::fs::remove_file(&artifact) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", artifact.display()));
            }
        }
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn reset_private_directory(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("removing {}", path.display()));
        }
    }
    assets::private_dir(path)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn clear_local_account_data(paths: &AppPaths, shared: &Shared) -> Result<()> {
    let _voice_outbox_guard = shared.voice_outbox_gate.lock().await;
    let _text_outbox_guard = shared.text_outbox_gate.lock().await;
    let _read_outbox_guard = shared.read_outbox_gate.lock().await;
    shared.database.clear_account_data()?;
    reset_private_directory(&shared.avatar_dir)?;
    reset_private_directory(&shared.media_dir)?;
    reset_private_directory(&shared.voice_outbox_dir)?;
    remove_sqlite_store(&paths.protocol_db)?;
    for marker in [
        &shared.contact_sync_marker,
        &shared.contact_history_marker,
        &shared.event_sync_marker,
    ] {
        match std::fs::remove_file(marker) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", marker.display()));
            }
        }
    }
    shared.media_recovery_requested.write().await.clear();
    shared.media_downloads.lock().await.clear();
    broadcast_chats(shared);
    shared.publish(ServerEvent::Unread { total: 0 });
    shared.avatars_changed();
    Ok(())
}

// Process/bootstrap wiring is exercised by the isolated daemon and deployment
// smoke suites; local reducers and IPC semantics are measured independently.
#[cfg_attr(coverage_nightly, coverage(off))]
fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_daemon())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn run_daemon() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "omarchy_whatsappd=info,whatsapp_rust=warn".into()),
        )
        .compact()
        .init();

    let options = Options::parse();
    let mut paths = AppPaths::discover();
    if let Some(state_dir) = options.state_dir {
        paths.state_dir.clone_from(&state_dir);
        paths.protocol_db = state_dir.join("session.db");
        paths.history_db = state_dir.join("history.db");
    }
    if let Some(socket) = options.socket {
        paths.runtime_dir = socket
            .parent()
            .context("socket override must have a parent directory")?
            .to_owned();
        paths.socket = socket;
    }
    std::fs::create_dir_all(&paths.runtime_dir)?;
    std::fs::set_permissions(&paths.runtime_dir, std::fs::Permissions::from_mode(0o700))?;
    std::fs::create_dir_all(&paths.state_dir)?;
    std::fs::set_permissions(&paths.state_dir, std::fs::Permissions::from_mode(0o700))?;
    let avatar_dir = paths.state_dir.join("avatars");
    let media_dir = paths.state_dir.join("media");
    let voice_outbox_dir = paths.state_dir.join("outbox");
    assets::private_dir(&avatar_dir)?;
    assets::private_dir(&media_dir)?;
    assets::private_dir(&voice_outbox_dir)?;
    match voice_outbox::recover_interrupted(&voice_outbox_dir, Utc::now().timestamp()) {
        Ok(entries) if !entries.is_empty() => {
            info!(
                count = entries.len(),
                "recovered retryable voice messages from the outbox"
            );
        }
        Ok(_) => {}
        Err(error) => warn!(%error, "could not recover the voice message outbox"),
    }

    let listener = bind_private_listener(&paths.socket).await?;

    let (events, _) = broadcast::channel(256);
    let (message_reducer, message_queue) = mpsc::channel(4_096);
    let (app_event_reducer, app_event_queue) = mpsc::channel(4_096);
    let database = Arc::new(Database::open(&paths.history_db)?);
    let clock = Arc::new(revisions::RevisionClock::default());
    let avatars = Arc::new(AvatarBroadcaster::new(events.clone(), Arc::clone(&clock)));
    let invalidations = Arc::new(InvalidationBroadcaster::new(
        Arc::clone(&database),
        events.clone(),
        Arc::clone(&clock),
    ));
    let shared = Arc::new(Shared {
        database,
        status: RwLock::new(ConnectionStatus::Starting),
        client: RwLock::new(None),
        events,
        clock,
        avatars,
        invalidations,
        connection_intents: StdMutex::new(connections::ConnectionIntents::default()),
        pairing_qr: paths.runtime_dir.join("pairing.svg"),
        contact_sync_marker: paths.state_dir.join("contact-names-v2"),
        contact_history_marker: paths.state_dir.join("contact-history-names-v1"),
        event_sync_marker: paths.state_dir.join("event-state-v6"),
        avatar_dir,
        media_dir,
        voice_outbox_dir,
        presence_sync_generation: AtomicU64::new(0),
        app_state_failed: AtomicBool::new(false),
        app_state_activity_ms: AtomicU64::new(0),
        app_state_notify: Notify::new(),
        chat_state_resync: RwLock::new((ChatStateResyncStatus::Idle, None)),
        chat_state_resync_requested: AtomicBool::new(false),
        chat_state_resync_notify: Notify::new(),
        logout_requested: AtomicBool::new(false),
        logout_trigger: StdMutex::new(None),
        phone_number_misses: StdMutex::new(PhoneNumberMisses::default()),
        avatar_sync: Mutex::new(()),
        group_name_sync: Mutex::new(()),
        media_recovery_requested: RwLock::new(HashSet::new()),
        media_downloads: Mutex::new(HashSet::new()),
        media_download_permits: Semaphore::new(jobs::MAX_PARALLEL_MEDIA_DOWNLOADS),
        avatar_fetches: Mutex::new(HashSet::new()),
        avatar_fetch_permits: Semaphore::new(jobs::MAX_PARALLEL_AVATAR_FETCHES),
        voice_outbox_gate: Mutex::new(()),
        text_outbox_gate: Mutex::new(()),
        read_outbox_gate: Mutex::new(()),
        text_outbox_notify: Notify::new(),
        read_outbox_notify: Notify::new(),
        command_gates: Mutex::new(HashMap::new()),
        message_reducer,
        app_event_reducer,
    });

    tokio::spawn(backfill_video_previews(Arc::clone(&shared)));
    tokio::spawn(run_text_outbox(Arc::clone(&shared)));
    tokio::spawn(run_read_outbox(Arc::clone(&shared)));
    tokio::spawn(run_message_reducer(Arc::clone(&shared), message_queue));
    tokio::spawn(run_app_event_reducer(Arc::clone(&shared), app_event_queue));

    let ipc_shared = Arc::clone(&shared);
    let mut ipc_task = tokio::spawn(async move { serve(listener, ipc_shared).await });

    // An unpaired upstream client deliberately ends its run loop after the QR
    // window expires. Keep the lightweight daemon and its IPC socket alive,
    // rebuilding only the protocol client so the UI receives a fresh code.
    loop {
        let generation = shared.clock.begin_generation();
        shared.begin_presence_sync(generation);
        let mut logout_signal = shared.arm_logout();
        let generation_jobs = Arc::new(GenerationJobs::default());
        shared.app_state_failed.store(false, Ordering::Relaxed);
        shared.app_state_activity_ms.store(0, Ordering::Relaxed);
        if let Err(error) =
            prepare_contact_name_resync(&paths.protocol_db, &shared.contact_sync_marker)
        {
            warn!(%error, "could not prepare WhatsApp contact-name resync");
        }
        if let Err(error) =
            prepare_event_state_resync(&paths.protocol_db, &shared.event_sync_marker)
        {
            warn!(%error, "could not prepare WhatsApp chat-state event resync");
            if shared
                .chat_state_resync_requested
                .swap(false, Ordering::SeqCst)
            {
                shared
                    .set_chat_state_resync(
                        ChatStateResyncStatus::Failed,
                        Some("Could not prepare the WhatsApp chat-state replay".to_owned()),
                    )
                    .await;
            }
        }
        let store = SqliteStore::new(paths.protocol_db.to_string_lossy().as_ref())
            .await
            .context("initializing WhatsApp session database")?;
        let qr_shared = Arc::clone(&shared);
        let connected_shared = Arc::clone(&shared);
        let logged_out_shared = Arc::clone(&shared);
        let disconnected_shared = Arc::clone(&shared);
        let history_shared = Arc::clone(&shared);
        let contact_shared = Arc::clone(&shared);
        let group_shared = Arc::clone(&shared);
        let app_event_shared = Arc::clone(&shared);
        let message_shared = Arc::clone(&shared);
        let connected_jobs = Arc::clone(&generation_jobs);
        let history_jobs = Arc::clone(&generation_jobs);
        let contact_jobs = Arc::clone(&generation_jobs);
        let group_jobs = Arc::clone(&generation_jobs);
        let app_event_jobs = Arc::clone(&generation_jobs);

        let bot = Bot::builder()
            .with_backend(store)
            .with_presence_policy(PresencePolicy::Manual)
            .with_event_delivery(EventDelivery::Ordered { capacity: 4_096 })
            .with_inbound_durability_hook(inbound::DurableInboundHook::new(Arc::clone(
                &shared.database,
            )))
            .with_device_props(
                DevicePropsOverride::new()
                    .with_os("Linux")
                    .with_platform_type(wa::device_props::PlatformType::DESKTOP),
            )
            .on_event(|event, _client| async move {
                log_whatsapp_event(&event);
            })
            .on_qr_code(move |code, timeout| {
                let shared = Arc::clone(&qr_shared);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    shared
                        .set_status(ConnectionStatus::Pairing {
                            code,
                            expires_at: Utc::now().timestamp()
                                + i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
                        })
                        .await;
                }
            })
            .on_connected(move |client| {
                let shared = Arc::clone(&connected_shared);
                let connected_jobs = Arc::clone(&connected_jobs);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    *shared.client.write().await = Some(Arc::clone(&client));
                    shared.set_status(ConnectionStatus::Connected).await;
                    info!("connected to WhatsApp");
                    if let Err(error) = shared.database.retry_all_text_messages() {
                        warn!(%error, "could not re-arm failed text messages after reconnect");
                    }
                    shared.text_outbox_notify.notify_one();
                    shared.read_outbox_notify.notify_one();
                    let _ = shared
                        .message_reducer
                        .send(MessageWork::Drain {
                            generation,
                            client: Arc::clone(&client),
                        })
                        .await;
                    let connected_shared = Arc::clone(&shared);
                    connected_jobs.spawn(async move {
                        let intent = connected_shared.connection_state();
                        force_reconcile_connection_intent(
                            &connected_shared,
                            &connections::ConnectionState::default(),
                            &intent,
                        )
                        .await;
                    });
                    let alias_shared = Arc::clone(&shared);
                    let alias_client = Arc::clone(&client);
                    connected_jobs.spawn(async move {
                        reconcile_direct_chat_aliases(&alias_shared, &alias_client).await;
                        broadcast_snapshot(&alias_shared);
                    });
                    if !shared.event_sync_marker.exists() {
                        connected_jobs.spawn(await_app_state_sync(Arc::clone(&shared), generation));
                    }
                    connected_jobs
                        .spawn(sync_group_names(Arc::clone(&shared), Arc::clone(&client)));
                    connected_jobs.spawn(sync_avatars(Arc::clone(&shared), Arc::clone(&client)));
                    connected_jobs.spawn(async move {
                        sync_missing_contact_names(Arc::clone(&shared), Arc::clone(&client)).await;
                        request_missing_contact_history(shared, client).await;
                    });
                }
            })
            .on_logged_out(move |_details| {
                let shared = Arc::clone(&logged_out_shared);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    *shared.client.write().await = None;
                    shared.set_status(ConnectionStatus::LoggedOut).await;
                    warn!("WhatsApp device was logged out");
                }
            })
            .on_event_for(
                &[
                    EventKind::Disconnected,
                    EventKind::ConnectFailure,
                    EventKind::StreamError,
                ],
                move |event, _client| {
                    let shared = Arc::clone(&disconnected_shared);
                    async move {
                        if !shared.clock.is_current(generation) {
                            return;
                        }
                        let reason = match &*event {
                            Event::Disconnected(details) => format!("{:?}", details.reason),
                            Event::ConnectFailure(details) => format!("{:?}", details.reason),
                            Event::StreamError(details) => details.code.clone(),
                            _ => "connection closed".to_owned(),
                        };
                        *shared.client.write().await = None;
                        shared
                            .set_status(ConnectionStatus::Disconnected { reason })
                            .await;
                    }
                },
            )
            .on_event_for(&[EventKind::HistorySync], move |event, client| {
                let shared = Arc::clone(&history_shared);
                let history_jobs = Arc::clone(&history_jobs);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    history_jobs.spawn(process_history_event(
                        shared,
                        generation,
                        event,
                        client,
                        Arc::clone(&history_jobs),
                    ));
                }
            })
            .on_event_for(&[EventKind::ContactUpdate], move |event, client| {
                let shared = Arc::clone(&contact_shared);
                let contact_jobs = Arc::clone(&contact_jobs);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    contact_jobs.spawn(process_contact_event(shared, generation, event, client));
                }
            })
            .on_event_for(&[EventKind::GroupUpdate], move |event, client| {
                let shared = Arc::clone(&group_shared);
                let group_jobs = Arc::clone(&group_jobs);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    group_jobs.spawn(process_group_event(shared, generation, event, client));
                }
            })
            .on_event_for(APP_EVENT_KINDS, move |event, client| {
                let shared = Arc::clone(&app_event_shared);
                let app_event_jobs = Arc::clone(&app_event_jobs);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    if let Err(error) = shared
                        .app_event_reducer
                        .send(AppEventWork {
                            generation,
                            event,
                            client,
                            jobs: Arc::clone(&app_event_jobs),
                        })
                        .await
                    {
                        error!(%error, "ordered app-event reducer stopped");
                    }
                }
            })
            .on_message(move |context| {
                let shared = Arc::clone(&message_shared);
                async move {
                    if !shared.clock.is_current(generation) {
                        return;
                    }
                    let key = inbound::InboundKey {
                        chat_jid: context.info.source.chat.to_string(),
                        sender_jid: context.info.source.sender.to_string(),
                        message_id: context.info.id.clone(),
                    };
                    if let Err(error) = shared
                        .message_reducer
                        .send(MessageWork::Live {
                            generation,
                            context: Box::new(context),
                            key,
                        })
                        .await
                    {
                        error!(%error, "ordered message reducer stopped");
                    }
                }
            })
            .build()
            .await
            .context("building WhatsApp client")?;

        info!(socket = %paths.socket.display(), "daemon ready");
        let mut bot_handle = bot.spawn();
        let restart = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown signal received");
                bot_handle.shutdown().await;
                false
            }
            () = &mut bot_handle => true,
            // The logout command owns this generation's trigger, so the run
            // loop exits for a logout even when the upstream client keeps its
            // own loop alive after the request.
            _ = &mut logout_signal => {
                info!("stopping WhatsApp client for a requested logout");
                bot_handle.shutdown().await;
                true
            }
            () = shared.chat_state_resync_notify.notified() => {
                shared
                    .set_chat_state_resync(
                        ChatStateResyncStatus::Syncing,
                        Some("Requesting authoritative chat state from WhatsApp".to_owned()),
                    )
                    .await;
                shared
                    .set_status(ConnectionStatus::Disconnected {
                        reason: "Resynchronizing WhatsApp chat state".to_owned(),
                    })
                    .await;
                info!("restarting WhatsApp client for requested chat-state resync");
                bot_handle.shutdown().await;
                true
            }
            result = &mut ipc_task => {
                result.context("IPC task panicked")??;
                bail!("IPC server stopped unexpectedly");
            }
        };
        shared.clock.retire_generation(generation);
        generation_jobs.abort_all();
        if !restart {
            break;
        }
        let (barrier, drained) = oneshot::channel();
        shared
            .message_reducer
            .send(MessageWork::Barrier(barrier))
            .await
            .context("ordered message reducer stopped before generation barrier")?;
        drained
            .await
            .context("ordered message reducer dropped the generation barrier")?;
        *shared.client.write().await = None;
        if shared.logout_requested.swap(false, Ordering::SeqCst) {
            clear_local_account_data(&paths, &shared)
                .await
                .context("clearing local WhatsApp account data after logout")?;
            shared.set_status(ConnectionStatus::LoggedOut).await;
            info!("cleared local WhatsApp account data after logout");
        } else if shared.chat_state_resync_requested.load(Ordering::SeqCst) {
            info!("WhatsApp client stopped for requested chat-state resync");
        } else {
            shared
                .set_status(ConnectionStatus::Disconnected {
                    reason: "WhatsApp session ended; retrying".to_owned(),
                })
                .await;
            warn!("WhatsApp run loop ended; starting a fresh connection");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let _ = std::fs::remove_file(&paths.socket);
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod test_support {
    use super::*;
    use omarchy_whatsapp_protocol::{Message, ServerFrame};
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    pub(crate) fn test_shared(directory: &tempfile::TempDir) -> Shared {
        let (events, _) = broadcast::channel(8);
        let (message_reducer, _message_queue) = mpsc::channel(8);
        let (app_event_reducer, _app_event_queue) = mpsc::channel(8);
        test_shared_with_reducers(directory, events, message_reducer, app_event_reducer)
    }

    pub(crate) fn test_shared_with_reducers(
        directory: &tempfile::TempDir,
        events: broadcast::Sender<ServerFrame>,
        message_reducer: mpsc::Sender<MessageWork>,
        app_event_reducer: mpsc::Sender<AppEventWork>,
    ) -> Shared {
        let database = Arc::new(Database::open(&directory.path().join("history.db")).unwrap());
        let clock = Arc::new(revisions::RevisionClock::default());
        let avatars = Arc::new(AvatarBroadcaster::new(events.clone(), Arc::clone(&clock)));
        let invalidations = Arc::new(InvalidationBroadcaster::new(
            Arc::clone(&database),
            events.clone(),
            Arc::clone(&clock),
        ));
        Shared {
            database,
            status: RwLock::new(ConnectionStatus::Starting),
            client: RwLock::new(None),
            events,
            clock,
            avatars,
            invalidations,
            connection_intents: StdMutex::new(connections::ConnectionIntents::default()),
            pairing_qr: directory.path().join("pairing.svg"),
            contact_sync_marker: directory.path().join("contact-names-v2"),
            contact_history_marker: directory.path().join("contact-history-names-v1"),
            event_sync_marker: directory.path().join("event-state-v6"),
            avatar_dir: directory.path().join("avatars"),
            media_dir: directory.path().join("media"),
            voice_outbox_dir: directory.path().join("outbox"),
            presence_sync_generation: AtomicU64::new(0),
            app_state_failed: AtomicBool::new(false),
            app_state_activity_ms: AtomicU64::new(0),
            app_state_notify: Notify::new(),
            chat_state_resync: RwLock::new((ChatStateResyncStatus::Idle, None)),
            chat_state_resync_requested: AtomicBool::new(false),
            chat_state_resync_notify: Notify::new(),
            logout_requested: AtomicBool::new(false),
            logout_trigger: StdMutex::new(None),
            phone_number_misses: StdMutex::new(PhoneNumberMisses::default()),
            avatar_sync: Mutex::new(()),
            group_name_sync: Mutex::new(()),
            media_recovery_requested: RwLock::new(HashSet::new()),
            media_downloads: Mutex::new(HashSet::new()),
            media_download_permits: Semaphore::new(jobs::MAX_PARALLEL_MEDIA_DOWNLOADS),
            avatar_fetches: Mutex::new(HashSet::new()),
            avatar_fetch_permits: Semaphore::new(jobs::MAX_PARALLEL_AVATAR_FETCHES),
            voice_outbox_gate: Mutex::new(()),
            text_outbox_gate: Mutex::new(()),
            read_outbox_gate: Mutex::new(()),
            text_outbox_notify: Notify::new(),
            read_outbox_notify: Notify::new(),
            command_gates: Mutex::new(HashMap::new()),
            message_reducer,
            app_event_reducer,
        }
    }

    pub(crate) async fn synthetic_client(directory: &tempfile::TempDir) -> Arc<Client> {
        let store = SqliteStore::new(
            directory
                .path()
                .join("protocol.db")
                .to_string_lossy()
                .as_ref(),
        )
        .await
        .unwrap();
        Bot::builder()
            .with_backend(store)
            .build()
            .await
            .unwrap()
            .client()
    }

    /// Ends a paused-time coalescing window: the scheduled task first has to
    /// run far enough to register its timer, then the clock passes it.
    pub(crate) async fn advance_past(window: std::time::Duration) {
        tokio::task::yield_now().await;
        tokio::time::advance(window + std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
    }

    pub(crate) fn seed_completed_app_state(directory: &tempfile::TempDir) {
        rusqlite::Connection::open(directory.path().join("session.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE app_state_versions (name TEXT NOT NULL);
                 INSERT INTO app_state_versions (name) VALUES
                    ('regular'), ('regular_low'), ('regular_high');",
            )
            .unwrap();
    }

    pub(crate) fn unread_message(id: &str) -> Message {
        Message {
            id: id.to_owned(),
            chat_jid: "1@s.whatsapp.net".into(),
            sender_jid: "1@s.whatsapp.net".into(),
            sender_name: "Ada".into(),
            text: "synthetic".into(),
            timestamp: 10,
            from_me: false,
            receipt: 0,
            delivered_at: None,
            read_at: None,
            delivered_to: Vec::new(),
            read_by: Vec::new(),
            media: None,
            reactions: Vec::new(),
        }
    }

    pub(crate) async fn read_test_frame(
        stream: &mut UnixStream,
        buffer: &mut Vec<u8>,
    ) -> ServerFrame {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte).await.unwrap();
                if byte[0] == b'\n' {
                    break;
                }
                buffer.push(byte[0]);
            }
        })
        .await
        .unwrap();
        serde_json::from_slice(buffer).unwrap()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::test_support::{advance_past, test_shared};

    #[tokio::test(start_paused = true)]
    async fn clearing_account_data_publishes_the_removed_avatar_jids() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assets::private_dir(&shared.avatar_dir).unwrap();
        assets::private_dir(&shared.media_dir).unwrap();
        let jid = "1@s.whatsapp.net";
        assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, jid), b"avatar")
            .unwrap();
        let mut events = shared.events.subscribe();
        shared.avatars_changed();
        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;
        let _ = events.try_recv().unwrap();
        let paths = AppPaths {
            runtime_dir: directory.path().join("runtime"),
            state_dir: directory.path().to_path_buf(),
            socket: directory.path().join("runtime/daemon.sock"),
            protocol_db: directory.path().join("session.db"),
            history_db: directory.path().join("history.db"),
        };

        clear_local_account_data(&paths, &shared).await.unwrap();
        advance_past(jobs::AVATAR_FLUSH_WINDOW).await;

        let avatar_event = std::iter::from_fn(|| events.try_recv().ok())
            .find(|frame| matches!(frame.event, ServerEvent::Avatars { .. }))
            .unwrap();
        assert_eq!(
            avatar_event.event,
            ServerEvent::Avatars {
                revision: 2,
                jids: Vec::new(),
                changed_jids: vec![jid.into()],
            }
        );
    }
}
