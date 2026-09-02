// Upstream WhatsApp event handling: the ordered reducers, the app-state
// replay waiter, and the adapter that applies each subscribed event.

use crate::history::{download_pending_media, history_lid_jids};
use crate::identity::{
    canonical_contact_jid, display_name, group_participant_identity, ingest_contact_name,
    resolve_group_participants,
};
use crate::presence::{effective_presence_available, force_reconcile_connection_intent};
use crate::state::{
    AppEventWork, GenerationJobs, MessageWork, Shared, broadcast_chats, broadcast_messages,
    broadcast_snapshot,
};
use crate::sync::{
    refresh_avatar, request_missing_contact_history, sync_avatars, sync_group_names,
    sync_missing_contact_names,
};
use crate::transport::Transport;
use crate::{assets, connections, inbound, notification};
use chrono::Utc;
use omarchy_whatsapp_protocol::{ChatState, ChatStateResyncStatus, ConnectionStatus, ServerEvent};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

// WhatsApp replays app-state in bursts. The waiter treats the replay as
// settled once no mutation arrived for the quiet period, and gives up after
// the deadline so a stalled replay cannot arm the resync report forever.
const APP_STATE_QUIET_MS: u64 = 2_000;
const APP_STATE_QUIET_PERIOD: std::time::Duration =
    std::time::Duration::from_millis(APP_STATE_QUIET_MS);
const APP_STATE_SYNC_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

async fn reduce_message(
    shared: &Arc<Shared>,
    generation: u64,
    message: Arc<wa::Message>,
    info: MessageInfo,
    transport: Arc<dyn Transport>,
    key: &inbound::InboundKey,
) {
    if !shared.clock.is_current(generation) {
        debug!(
            generation,
            "discarded message work from an obsolete client generation"
        );
        return;
    }
    let accepted = shared
        .receive_message(generation, message, info, transport)
        .await;
    finish_inbound_reduction(shared, key, accepted);
}

fn finish_inbound_reduction(shared: &Shared, key: &inbound::InboundKey, accepted: bool) {
    if !accepted {
        return;
    }
    match shared.database.finish_inbound(key) {
        Ok(_) => {}
        Err(error) => warn!(%error, "could not finish durable inbound message"),
    }
}

pub(crate) async fn run_message_reducer(
    shared: Arc<Shared>,
    mut queue: mpsc::Receiver<MessageWork>,
) {
    while let Some(work) = queue.recv().await {
        match work {
            MessageWork::Drain {
                generation,
                transport,
            } => {
                if !shared.clock.is_current(generation) {
                    continue;
                }
                let pending = match shared.database.pending_inbound(10_000) {
                    Ok(pending) => pending,
                    Err(error) => {
                        warn!(%error, "could not read durable inbound inbox");
                        continue;
                    }
                };
                for record in pending {
                    if !shared.clock.is_current(generation) {
                        break;
                    }
                    let (message, info) = match record.decode_parts() {
                        Ok(parts) => parts,
                        Err(error) => {
                            error!(%error, "durable inbound message requires manual recovery");
                            continue;
                        }
                    };
                    reduce_message(
                        &shared,
                        generation,
                        message,
                        info,
                        Arc::clone(&transport),
                        &record.key,
                    )
                    .await;
                }
            }
            MessageWork::Live {
                generation,
                message,
                info,
                transport,
                key,
            } => reduce_message(&shared, generation, message, *info, transport, &key).await,
            MessageWork::Barrier(completed) => {
                let _ = completed.send(());
            }
        }
    }
}

pub(crate) async fn run_app_event_reducer(
    shared: Arc<Shared>,
    mut queue: mpsc::Receiver<AppEventWork>,
) {
    while let Some(work) = queue.recv().await {
        if shared.clock.is_current(work.generation) {
            let task = work.jobs.spawn(handle_app_event(
                Arc::clone(&shared),
                work.generation,
                work.event,
                work.transport,
                Arc::clone(&work.jobs),
            ));
            let _ = task.await;
        }
    }
}

// The two `spawn_blocking` join arms report a panicking history worker. A
// scripted transport cannot make the bounded, `Result`-returning parsing and
// ingest closures panic, so those arms stay outside the measured set; every
// other branch here is driven by the history tests.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn process_history_event(
    shared: Arc<Shared>,
    generation: u64,
    event: Arc<Event>,
    transport: Arc<dyn Transport>,
    jobs: Arc<GenerationJobs>,
) {
    let Event::HistorySync(history) = &*event else {
        return;
    };
    let history = history.clone();
    let scan_history = history.clone();
    let candidates =
        match tokio::task::spawn_blocking(move || history_lid_jids(&scan_history)).await {
            Ok(Ok(candidates)) => candidates,
            Ok(Err(error)) => {
                error!(%error, "could not inspect history sync identities");
                return;
            }
            Err(error) => {
                error!(%error, "history identity worker panicked");
                return;
            }
        };
    if !shared.clock.is_current(generation) {
        return;
    }
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort_unstable();
    let mut aliases = HashMap::with_capacity(candidates.len());
    for raw in candidates {
        if !shared.clock.is_current(generation) {
            return;
        }
        let Ok(jid) = raw.parse::<Jid>() else {
            continue;
        };
        let canonical = canonical_contact_jid(&shared, transport.as_ref(), &jid).await;
        if canonical != raw {
            aliases.insert(raw, canonical);
        }
    }
    let ingest_shared = Arc::clone(&shared);
    let own_pn = transport.pn().map(|jid| jid.to_non_ad_string());
    let result = tokio::task::spawn_blocking(move || {
        ingest_shared.ingest_history(&history, own_pn.as_deref(), &aliases)
    })
    .await;
    if !shared.clock.is_current(generation) {
        return;
    }
    match result {
        Ok(Ok((pending_media, changed_message_chats))) => {
            broadcast_chats(&shared);
            for chat_jid in changed_message_chats {
                broadcast_messages(&shared, &chat_jid);
            }
            shared.publish(ServerEvent::Unread {
                total: shared.unread_total_or_zero(),
            });
            sync_avatars(Arc::clone(&shared), Arc::clone(&transport)).await;
            if !shared.clock.is_current(generation) {
                return;
            }
            let names_shared = Arc::clone(&shared);
            let names_transport = Arc::clone(&transport);
            jobs.spawn(async move {
                sync_missing_contact_names(Arc::clone(&names_shared), Arc::clone(&names_transport))
                    .await;
                request_missing_contact_history(names_shared, names_transport).await;
            });
            if !pending_media.is_empty() {
                jobs.spawn(download_pending_media(shared, transport, pending_media));
            }
        }
        Ok(Err(error)) => error!(%error, "could not ingest history sync"),
        Err(error) => error!(%error, "history sync worker panicked"),
    }
}

pub(crate) async fn process_contact_event(
    shared: Arc<Shared>,
    generation: u64,
    event: Arc<Event>,
    transport: Arc<dyn Transport>,
) {
    let Event::ContactUpdate(update) = &*event else {
        return;
    };
    if !shared.clock.is_current(generation) {
        return;
    }
    ingest_contact_name(&shared, update);
    canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
}

pub(crate) async fn process_group_event(
    shared: Arc<Shared>,
    generation: u64,
    event: Arc<Event>,
    transport: Arc<dyn Transport>,
) {
    let Event::GroupUpdate(update) = &*event else {
        return;
    };
    let metadata = match transport.group_metadata(&update.group_jid).await {
        Ok(metadata) => metadata,
        Err(error) => {
            warn!(%error, "could not refresh WhatsApp group metadata");
            return;
        }
    };
    if !shared.clock.is_current(generation) {
        return;
    }
    let participant_identities = metadata
        .participants
        .into_iter()
        .map(group_participant_identity)
        .collect();
    match shared
        .database
        .update_group_name(&update.group_jid.to_non_ad_string(), &metadata.subject)
    {
        Ok(true) => broadcast_chats(&shared),
        Ok(false) => {}
        Err(error) => warn!(%error, "could not update WhatsApp group subject"),
    }
    let chat_jid = update.group_jid.to_non_ad_string();
    let participants =
        resolve_group_participants(&shared, transport.as_ref(), participant_identities).await;
    if shared.clock.is_current(generation) {
        shared.publish(ServerEvent::GroupParticipants {
            chat_jid,
            participants,
        });
    }
}

/// Time left before the app-state replay counts as quiet. `None` means the
/// quiet period elapsed and completeness can be inspected.
fn app_state_quiet_remaining(now_ms: u64, last_activity_ms: u64) -> Option<std::time::Duration> {
    if last_activity_ms == 0 {
        return None;
    }
    let elapsed = now_ms.saturating_sub(last_activity_ms);
    (elapsed < APP_STATE_QUIET_MS)
        .then(|| std::time::Duration::from_millis(APP_STATE_QUIET_MS - elapsed))
}

/// Applies the terminal outcome of a settled app-state replay. Reconciling the
/// imported counters is its last step, so a requested resync reports success
/// or failure from that result.
async fn finish_app_state_sync(shared: &Shared, generation: u64) {
    match shared.database.reconcile_unread_after_full_sync() {
        Ok(changed) => {
            info!(
                changed,
                generation, "reconciled imported unread counters with app-state replay"
            );
            broadcast_snapshot(shared);
            shared.mark_event_sync_complete();
            if shared
                .chat_state_resync_requested
                .swap(false, Ordering::SeqCst)
            {
                shared
                    .set_chat_state_resync(
                        ChatStateResyncStatus::Succeeded,
                        Some("WhatsApp chat state is up to date".to_owned()),
                    )
                    .await;
            }
        }
        Err(error) => {
            warn!(%error, generation, "could not reconcile WhatsApp unread counters");
            if shared
                .chat_state_resync_requested
                .swap(false, Ordering::SeqCst)
            {
                shared
                    .set_chat_state_resync(
                        ChatStateResyncStatus::Failed,
                        Some("Could not reconcile the replayed WhatsApp chat state".to_owned()),
                    )
                    .await;
            }
        }
    }
}

/// Reports the deadline outcome when the linked device never reached a
/// complete quiet checkpoint inside the replay window.
async fn expire_app_state_sync(shared: &Shared, generation: u64) {
    warn!(
        generation,
        "WhatsApp app-state replay did not reach a complete quiet checkpoint"
    );
    if shared
        .chat_state_resync_requested
        .swap(false, Ordering::SeqCst)
    {
        shared
            .set_chat_state_resync(
                ChatStateResyncStatus::Failed,
                Some("WhatsApp did not finish the chat-state replay".to_owned()),
            )
            .await;
    }
}

// Each completeness check opens the session database, so the waiter sleeps
// until the quiet period elapses instead of polling it.
pub(crate) async fn await_app_state_sync(shared: Arc<Shared>, generation: u64) {
    let deadline = tokio::time::Instant::now() + APP_STATE_SYNC_DEADLINE;
    loop {
        if !shared.clock.is_current(generation) || shared.app_state_failed.load(Ordering::Relaxed) {
            return;
        }
        let now_ms = u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default();
        let last_activity = shared.app_state_activity_ms.load(Ordering::Relaxed);
        let mut window = APP_STATE_QUIET_PERIOD;
        if let Some(remaining) = app_state_quiet_remaining(now_ms, last_activity) {
            window = remaining;
        } else {
            let complete = match shared.database.regular_app_state_is_complete() {
                Ok(complete) => complete,
                Err(error) => {
                    warn!(%error, generation, "could not inspect app-state replay progress");
                    false
                }
            };
            if complete {
                finish_app_state_sync(&shared, generation).await;
                return;
            }
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            expire_app_state_sync(&shared, generation).await;
            return;
        }
        // A fresh app-state mutation restarts the quiet window; otherwise the
        // waiter sleeps until that window or the replay deadline elapses.
        let _ = tokio::time::timeout(
            window.min(deadline - now),
            shared.app_state_notify.notified(),
        )
        .await;
    }
}

fn protocol_chat_state(
    state: whatsapp_rust::wacore::types::presence::ChatPresence,
    media: whatsapp_rust::wacore::types::presence::ChatPresenceMedia,
) -> ChatState {
    match (state, media) {
        (
            whatsapp_rust::wacore::types::presence::ChatPresence::Composing,
            whatsapp_rust::wacore::types::presence::ChatPresenceMedia::Audio,
        ) => ChatState::Recording,
        (whatsapp_rust::wacore::types::presence::ChatPresence::Composing, _) => ChatState::Typing,
        (whatsapp_rust::wacore::types::presence::ChatPresence::Paused, _) => ChatState::Paused,
    }
}

pub(crate) const APP_EVENT_KINDS: &[EventKind] = &[
    EventKind::PairError,
    EventKind::QrScannedWithoutMultidevice,
    EventKind::ClientOutdated,
    EventKind::Receipt,
    EventKind::UndecryptableMessage,
    EventKind::ChatPresence,
    EventKind::Presence,
    EventKind::PictureUpdate,
    EventKind::ContactUpdated,
    EventKind::ContactNumberChanged,
    EventKind::ContactSyncRequested,
    EventKind::IncomingCall,
    EventKind::MissedCall,
    EventKind::CallEndedElsewhere,
    EventKind::PushNameUpdate,
    EventKind::SelfPushNameUpdated,
    EventKind::PinUpdate,
    EventKind::MuteUpdate,
    EventKind::ArchiveUpdate,
    EventKind::MarkChatAsReadUpdate,
    EventKind::DeleteChatUpdate,
    EventKind::ClearChatUpdate,
    EventKind::DeleteMessageForMeUpdate,
    EventKind::OfflineSyncCompleted,
    EventKind::DirtyState,
    EventKind::IdentityChange,
    EventKind::BusinessStatusUpdate,
    EventKind::StreamReplaced,
    EventKind::TemporaryBan,
    EventKind::ServerAck,
    EventKind::PairingQrCodesExhausted,
    EventKind::AppStateSyncFailed,
];

fn read_action_boundary(
    action: &wa::sync_action_value::MarkChatAsReadAction,
) -> (Option<i64>, Vec<String>) {
    let range = action.message_range.as_option();
    let timestamp = range.and_then(|range| range.last_message_timestamp);
    let ids = range
        .into_iter()
        .flat_map(|range| range.messages.iter())
        .filter_map(|message| message.key.as_option()?.id.clone())
        .collect();
    (timestamp, ids)
}

fn event_diagnostic(event: &Event) -> String {
    match event {
        Event::RawNode(node) => format!("RawNode({node:?})"),
        Event::Notification(node) => format!("Notification({node:?})"),
        _ => serialized_event_diagnostic(event),
    }
}

// Serialization failures depend on serde implementations in the upstream SDK;
// the raw-node policies and successful full-payload logging remain measured.
#[cfg_attr(coverage_nightly, coverage(off))]
fn serialized_event_diagnostic(event: &Event) -> String {
    serde_json::to_string(event).unwrap_or_else(|error| {
        format!(
            "{{\"event_kind\":\"{:?}\",\"serialization_error\":{}}}",
            event.kind(),
            serde_json::to_string(&error.to_string()).unwrap_or_else(|_| "null".to_owned())
        )
    })
}

pub(crate) fn log_whatsapp_event(event: &Event) {
    info!(event = %event_diagnostic(event), "received WhatsApp event");
}

async fn handle_app_event(
    shared: Arc<Shared>,
    generation: u64,
    event: Arc<Event>,
    transport: Arc<dyn Transport>,
    jobs: Arc<GenerationJobs>,
) {
    if !shared.clock.is_current(generation) {
        return;
    }
    shared.app_state_activity_ms.store(
        u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default(),
        Ordering::Relaxed,
    );
    shared.app_state_notify.notify_waiters();
    match &*event {
        Event::Receipt(receipt) => {
            let receipt_type = receipt.r#type.as_wire_str();
            if matches!(receipt_type, "read-self" | "played-self") {
                let jid =
                    canonical_contact_jid(&shared, transport.as_ref(), &receipt.source.chat).await;
                match shared.database.apply_self_read_receipt(
                    &jid,
                    &receipt.message_ids,
                    receipt.timestamp.timestamp(),
                ) {
                    Ok(changed) => {
                        info!(
                            receipt_type,
                            message_count = receipt.message_ids.len(),
                            offline = receipt.offline,
                            changed,
                            "applied cross-device WhatsApp read receipt"
                        );
                        if changed {
                            broadcast_snapshot(&shared);
                        }
                    }
                    Err(error) => {
                        warn!(%error, "could not apply cross-device WhatsApp read receipt");
                    }
                }
                return;
            }
            let state: u8 = match receipt_type {
                "read" | "read-self" => 3,
                "played" | "played-self" => 4,
                "delivery" => 2,
                "sent" | "sender" => 1,
                _ => 0,
            };
            if state > 0 {
                let chat_jid =
                    canonical_contact_jid(&shared, transport.as_ref(), &receipt.source.chat).await;
                let recipient_jid = if state >= 2
                    && (!receipt.source.chat.is_group()
                        || receipt.source.sender != receipt.source.chat)
                {
                    Some(
                        canonical_contact_jid(&shared, transport.as_ref(), &receipt.source.sender)
                            .await,
                    )
                } else {
                    None
                };
                match shared.database.update_receipts(
                    &chat_jid,
                    &receipt.message_ids,
                    state,
                    recipient_jid.as_deref(),
                    receipt.timestamp.timestamp(),
                ) {
                    Ok(true) => broadcast_messages(&shared, &chat_jid),
                    Ok(false) => {}
                    Err(error) => warn!(%error, "could not persist WhatsApp receipts"),
                }
            }
        }
        Event::UndecryptableMessage(details) => {
            warn!(
                message_id = %details.info.id,
                unavailable = details.is_unavailable,
                "WhatsApp message could not be decrypted; library recovery remains active"
            );
        }
        Event::ChatPresence(update) => {
            let state = protocol_chat_state(update.state, update.media);
            let chat_jid =
                canonical_contact_jid(&shared, transport.as_ref(), &update.source.chat).await;
            let sender_jid =
                canonical_contact_jid(&shared, transport.as_ref(), &update.source.sender).await;
            let sender_name = shared
                .database
                .contact_name(&sender_jid)
                .ok()
                .flatten()
                .or_else(|| shared.database.chat_name(&sender_jid).ok().flatten())
                .filter(|name| name != &sender_jid)
                .unwrap_or_default();
            shared.publish(ServerEvent::ChatState {
                chat_jid,
                sender_jid,
                sender_name,
                state,
            });
        }
        Event::Presence(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.from).await;
            shared.publish(ServerEvent::Presence {
                jid,
                available: !update.unavailable,
                last_seen: update.last_seen.map(|last_seen| last_seen.timestamp()),
            });
        }
        Event::PictureUpdate(update) => {
            let raw = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            let jid = raw.parse::<Jid>().unwrap_or_else(|_| update.jid.clone());
            assets::remove_avatar(&shared.avatar_dir, &raw);
            if update.removed {
                let marker = assets::avatar_missing_path(&shared.avatar_dir, &raw);
                if let Err(error) = assets::write_private_bytes(&marker, b"none\n") {
                    warn!(%error, %raw, "could not persist missing-avatar marker");
                }
                shared.avatars_changed();
            } else {
                jobs.spawn(refresh_avatar(shared, transport, jid, true));
            }
        }
        Event::ContactUpdated(update) => {
            jobs.spawn(refresh_avatar(
                Arc::clone(&shared),
                Arc::clone(&transport),
                update.jid.clone(),
                true,
            ));
            jobs.spawn(sync_missing_contact_names(shared, transport));
        }
        Event::ContactNumberChanged(update) => {
            let old = update.old_jid.to_non_ad_string();
            let new = update.new_jid.to_non_ad_string();
            if let (Some(old_lid), Some(new_lid)) = (&update.old_lid, &update.new_lid) {
                for (lid, pn) in [(old_lid, &update.old_jid), (new_lid, &update.new_jid)] {
                    if let Err(error) = shared
                        .database
                        .migrate_contact_jid(&lid.to_non_ad_string(), &pn.to_non_ad_string())
                    {
                        warn!(%error, "could not merge changed WhatsApp contact alias");
                    }
                }
            }
            if let Err(error) = shared.database.migrate_contact_jid(&old, &new) {
                warn!(%error, %old, %new, "could not migrate changed WhatsApp number");
            }
            assets::remove_avatar(&shared.avatar_dir, &old);
            jobs.spawn(refresh_avatar(
                Arc::clone(&shared),
                transport,
                update.new_jid.clone(),
                true,
            ));
            broadcast_snapshot(&shared);
        }
        Event::ContactSyncRequested(_) => {
            jobs.spawn(sync_missing_contact_names(shared, transport));
        }
        Event::IncomingCall(call) => {
            let action = call.action.wire_tag();
            if matches!(action, "offer" | "offer_notice") && !call.offline {
                let name = call
                    .notify
                    .clone()
                    .unwrap_or_else(|| display_name(&shared, &call.from));
                notification::send_event(
                    "Incoming WhatsApp call",
                    &format!("{name} is calling"),
                    "critical",
                );
            }
        }
        Event::MissedCall(call) => {
            notification::send_event(
                "Missed WhatsApp call",
                &display_name(&shared, &call.from),
                "normal",
            );
        }
        Event::CallEndedElsewhere(call) => {
            info!(from = %call.from, outcome = ?call.outcome, "WhatsApp call ended on another device");
        }
        Event::PushNameUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Err(error) = shared
                .database
                .update_contact_name(&jid, &update.new_push_name)
            {
                warn!(%error, "could not update WhatsApp push name");
            }
            broadcast_chats(&shared);
        }
        Event::SelfPushNameUpdated(update) => {
            info!(old = %update.old_name, new = %update.new_name, "own WhatsApp profile name changed");
            if effective_presence_available(
                shared.connection_state().available,
                shared.presence_sync_pending(),
            ) && !update.new_name.is_empty()
                && let Err(error) = transport.set_available().await
            {
                warn!(%error, "could not restore deferred available WhatsApp presence");
            }
        }
        Event::PinUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Some(pinned) = update.action.pinned
                && let Err(error) =
                    shared
                        .database
                        .apply_pin_at(&jid, pinned, update.timestamp.timestamp())
            {
                warn!(%error, "could not apply WhatsApp pin state");
            }
            broadcast_chats(&shared);
        }
        Event::MuteUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Some(muted) = update.action.muted
                && let Err(error) = shared.database.apply_mute_at(
                    &jid,
                    muted,
                    update.action.mute_end_timestamp.unwrap_or(0),
                    update.timestamp.timestamp(),
                )
            {
                warn!(%error, "could not apply WhatsApp mute state");
            }
        }
        Event::ArchiveUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Some(archived) = update.action.archived
                && let Err(error) =
                    shared
                        .database
                        .apply_archive_at(&jid, archived, update.timestamp.timestamp())
            {
                warn!(%error, "could not apply WhatsApp archive state");
            }
            broadcast_snapshot(&shared);
        }
        Event::MarkChatAsReadUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Some(read) = update.action.read {
                let range = update.action.message_range.as_option();
                let (boundary_timestamp, boundary_ids) = read_action_boundary(&update.action);
                match shared.database.apply_synced_read_state(
                    &jid,
                    read,
                    boundary_timestamp,
                    &boundary_ids,
                    update.timestamp.timestamp(),
                ) {
                    Ok(changed) => info!(
                        read,
                        from_full_sync = update.from_full_sync,
                        has_range = range.is_some(),
                        boundary_message_count = boundary_ids.len(),
                        changed,
                        "applied cross-device WhatsApp chat read state"
                    ),
                    Err(error) => {
                        warn!(%error, "could not apply cross-device WhatsApp read state");
                    }
                }
            }
            broadcast_snapshot(&shared);
        }
        Event::DeleteChatUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Err(error) = shared
                .database
                .delete_chat(&jid, update.timestamp.timestamp())
            {
                warn!(%error, %jid, "could not apply WhatsApp chat deletion");
            }
            if update.delete_media {
                assets::remove_chat_media(&shared.media_dir, &jid);
            }
            broadcast_snapshot(&shared);
        }
        Event::ClearChatUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Err(error) = shared
                .database
                .clear_chat(&jid, update.timestamp.timestamp())
            {
                warn!(%error, %jid, "could not apply WhatsApp history clearing");
            }
            if update.delete_media {
                assets::remove_chat_media(&shared.media_dir, &jid);
            }
            broadcast_snapshot(&shared);
            broadcast_messages(&shared, &jid);
        }
        Event::DeleteMessageForMeUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.chat_jid).await;
            if let Err(error) = shared.database.delete_message(&jid, &update.message_id) {
                warn!(%error, "could not apply WhatsApp message deletion");
            }
            assets::remove_message_media(&shared.media_dir, &jid, &update.message_id);
            broadcast_chats(&shared);
            broadcast_messages(&shared, &jid);
        }
        Event::OfflineSyncCompleted(details) => {
            info!(count = details.count, "WhatsApp offline sync completed");
            broadcast_snapshot(&shared);
            if shared.finish_presence_sync(generation) {
                let intent = shared.connection_state();
                force_reconcile_connection_intent(
                    &shared,
                    &connections::ConnectionState::default(),
                    &intent,
                )
                .await;
            }
            jobs.spawn(sync_group_names(
                Arc::clone(&shared),
                Arc::clone(&transport),
            ));
        }
        Event::DirtyState(details) => {
            info!(kind = ?details.dirty_type, "WhatsApp requested derived-state refresh");
            jobs.spawn(sync_group_names(
                Arc::clone(&shared),
                Arc::clone(&transport),
            ));
            jobs.spawn(sync_missing_contact_names(
                Arc::clone(&shared),
                Arc::clone(&transport),
            ));
            jobs.spawn(sync_avatars(shared, transport));
        }
        Event::IdentityChange(change) => {
            notification::send_event(
                "WhatsApp security code changed",
                &format!(
                    "Security information changed for {}",
                    display_name(&shared, &change.user)
                ),
                "normal",
            );
        }
        Event::BusinessStatusUpdate(update) => {
            let jid = canonical_contact_jid(&shared, transport.as_ref(), &update.jid).await;
            if let Some(name) = update.verified_name.as_deref()
                && let Err(error) = shared.database.update_contact_name(&jid, name)
            {
                warn!(%error, "could not update WhatsApp business name");
            }
            broadcast_chats(&shared);
            jobs.spawn(refresh_avatar(shared, transport, update.jid.clone(), true));
        }
        Event::StreamReplaced(_) => {
            *shared.client.write().await = None;
            shared
                .set_status(ConnectionStatus::Disconnected {
                    reason: "WhatsApp was opened by another companion session".to_owned(),
                })
                .await;
        }
        Event::TemporaryBan(ban) => {
            let message = ban
                .message
                .clone()
                .unwrap_or_else(|| format!("Temporary WhatsApp restriction: {}", ban.code));
            notification::send_event("WhatsApp temporarily restricted", &message, "critical");
            shared.set_status(ConnectionStatus::Error { message }).await;
        }
        Event::ServerAck(ack) => {
            if let Some(code) = &ack.error {
                warn!(id = %ack.id, class = ?ack.class, %code, "WhatsApp server rejected outgoing stanza");
                if ack.class.as_deref() == Some("message") {
                    notification::send_event(
                        "WhatsApp message not sent",
                        &format!("Server error {code}"),
                        "normal",
                    );
                }
            }
        }
        Event::PairError(_) => {
            shared
                .set_status(ConnectionStatus::Error {
                    message: "WhatsApp device pairing failed; retrying".to_owned(),
                })
                .await;
        }
        Event::QrScannedWithoutMultidevice(_) => {
            shared
                .set_status(ConnectionStatus::Error {
                    message: "Enable multi-device WhatsApp and scan the new QR code".to_owned(),
                })
                .await;
        }
        Event::ClientOutdated(_) => {
            shared
                .set_status(ConnectionStatus::Error {
                    message: "This WhatsApp client version is no longer accepted".to_owned(),
                })
                .await;
        }
        Event::PairingQrCodesExhausted(_) => {
            shared
                .set_status(ConnectionStatus::Disconnected {
                    reason: "Pairing QR expired; generating a new code".to_owned(),
                })
                .await;
        }
        Event::AppStateSyncFailed(failure) => {
            shared.app_state_failed.store(true, Ordering::Relaxed);
            let _ = std::fs::remove_file(&shared.event_sync_marker);
            let detail = format!(
                "WhatsApp state sync incomplete (fatal: {}, retryable: {}, skipped: {})",
                failure.fatal.len(),
                failure.retryable.len(),
                failure.skipped.len()
            );
            warn!(%detail);
            if shared
                .chat_state_resync_requested
                .swap(false, Ordering::SeqCst)
            {
                shared
                    .set_chat_state_resync(
                        ChatStateResyncStatus::Failed,
                        Some("WhatsApp could not complete the chat-state replay".to_owned()),
                    )
                    .await;
            }
            notification::send_event("WhatsApp synchronization incomplete", &detail, "normal");
            if !failure.connected || !failure.fatal.is_empty() {
                shared
                    .set_status(ConnectionStatus::Error { message: detail })
                    .await;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::default_trait_access)] // Generated protobuf fixture types are inferred by MessageField.
mod tests {
    use super::*;
    use crate::commands::canonical_requested_jid;
    use crate::test_support::{
        seed_completed_app_state, synthetic_client, test_shared, test_shared_with_reducers,
        unread_message,
    };
    use crate::transport::ClientTransport;
    use crate::transport::fake::{Call, CallKind, FakeTransport};
    use buffa::MessageField;
    use chrono::TimeZone;
    use omarchy_whatsapp_protocol::Resource;
    use tokio::sync::{broadcast, oneshot};
    use whatsapp_rust::types::events as sdk;
    use whatsapp_rust::wacore::types::{
        call::{
            CallAction, CallEndedElsewhere, ElsewhereOutcome, IncomingCall, MissedCall,
            MissedReason,
        },
        events::{DecryptFailMode, Receipt, UnavailableType},
        message::MessageSource,
        presence::{ChatPresence, ChatPresenceMedia, ReceiptType},
    };
    use whatsapp_rust::wacore_binary::builder::NodeBuilder;

    const CHAT: &str = "31600000001@s.whatsapp.net";
    const OTHER: &str = "31600000002@s.whatsapp.net";
    const GROUP: &str = "120363000000000001@g.us";
    const NOW: i64 = 1_700_000_000;

    fn stamp(seconds: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).unwrap()
    }

    fn jid(value: &str) -> Jid {
        value.parse().unwrap()
    }

    fn linked(directory: &tempfile::TempDir) -> (Arc<Shared>, u64, Arc<FakeTransport>) {
        let shared = Arc::new(test_shared(directory));
        assets::private_dir(&shared.avatar_dir).unwrap();
        assets::private_dir(&shared.media_dir).unwrap();
        let generation = shared.clock.begin_generation();
        (shared, generation, Arc::new(FakeTransport::new()))
    }

    /// Reports every diagnostic as enabled without recording it, so the
    /// adapter's structured fields are evaluated the way they are under the
    /// daemon's own subscriber instead of being skipped as disabled.
    struct DiagnosticsEnabled;

    impl tracing::Subscriber for DiagnosticsEnabled {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, _: &tracing::Event<'_>) {}

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    async fn apply(
        shared: &Arc<Shared>,
        generation: u64,
        event: Event,
        fake: &Arc<FakeTransport>,
        jobs: &Arc<GenerationJobs>,
    ) {
        let _diagnostics = tracing::subscriber::set_default(DiagnosticsEnabled);
        handle_app_event(
            Arc::clone(shared),
            generation,
            Arc::new(event),
            crate::transport::fake::transport(fake),
            Arc::clone(jobs),
        )
        .await;
    }

    fn stored_message(id: &str, chat: &str, from_me: bool) -> omarchy_whatsapp_protocol::Message {
        omarchy_whatsapp_protocol::Message {
            id: id.to_owned(),
            chat_jid: chat.to_owned(),
            sender_jid: if from_me {
                "me".to_owned()
            } else {
                chat.to_owned()
            },
            sender_name: "Ada".into(),
            text: "synthetic".into(),
            timestamp: NOW,
            from_me,
            receipt: u8::from(from_me),
            delivered_at: None,
            read_at: None,
            delivered_to: Vec::new(),
            read_by: Vec::new(),
            media: None,
            reactions: Vec::new(),
        }
    }

    fn receipt(chat: &str, sender: &str, kind: ReceiptType, ids: &[&str]) -> Event {
        Event::Receipt(
            Receipt::builder()
                .source(MessageSource {
                    chat: jid(chat),
                    sender: jid(sender),
                    is_group: chat.ends_with("@g.us"),
                    ..Default::default()
                })
                .message_ids(ids.iter().map(|id| (*id).to_owned()).collect())
                .timestamp(stamp(NOW + 10))
                .r#type(kind)
                .offline(false)
                .build(),
        )
    }

    #[tokio::test]
    async fn a_retired_generation_stops_the_event_adapter() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());

        apply(
            &shared,
            generation.saturating_add(1),
            Event::StreamReplaced(sdk::StreamReplaced::builder().build()),
            &fake,
            &jobs,
        )
        .await;

        assert_eq!(shared.app_state_activity_ms.load(Ordering::Relaxed), 0);
        assert!(matches!(
            *shared.status.read().await,
            ConnectionStatus::Starting
        ));
    }

    #[tokio::test]
    async fn cross_device_read_receipts_reconcile_the_local_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        shared
            .database
            .insert_message(&stored_message("M-1", CHAT, false), "Ada", false, true)
            .unwrap();
        let mut events = shared.events.subscribe();

        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::ReadSelf, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(shared.database.unread_total().unwrap(), 0);
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(|frame| matches!(
                frame.event,
                ServerEvent::Invalidated {
                    resource: Resource::Chats,
                    ..
                }
            ))
        );

        // Replaying the same watermark changes nothing.
        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::PlayedSelf, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;

        shared
            .database
            .execute_test_sql("DROP TABLE messages")
            .unwrap();
        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::ReadSelf, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;
        assert!(shared.app_state_activity_ms.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn delivery_and_read_receipts_advance_stored_messages() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        shared
            .database
            .insert_message(&stored_message("M-1", CHAT, true), "Ada", false, false)
            .unwrap();
        shared
            .database
            .insert_message(&stored_message("G-1", GROUP, true), "Garden", true, false)
            .unwrap();

        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::Delivered, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(shared.database.messages(CHAT, 10).unwrap()[0].receipt, 2);

        // The same receipt again is a no-op rather than a broadcast.
        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::Delivered, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::Read, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(shared.database.messages(CHAT, 10).unwrap()[0].receipt, 3);

        // A group receipt names the participant that reported it.
        apply(
            &shared,
            generation,
            receipt(GROUP, OTHER, ReceiptType::Played, &["G-1"]),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            shared.database.messages(GROUP, 10).unwrap()[0]
                .read_by
                .iter()
                .map(|reader| reader.jid.clone())
                .collect::<Vec<_>>(),
            vec![OTHER.to_owned()]
        );

        // A group-wide receipt carries no participant, and a plain send ack
        // never resolves one.
        apply(
            &shared,
            generation,
            receipt(GROUP, GROUP, ReceiptType::Read, &["G-1"]),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::Sender, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;
        // An unknown receipt type is ignored entirely.
        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::Other("synthetic".into()), &["M-1"]),
            &fake,
            &jobs,
        )
        .await;

        shared
            .database
            .execute_test_sql("DROP TABLE messages")
            .unwrap();
        apply(
            &shared,
            generation,
            receipt(CHAT, CHAT, ReceiptType::Read, &["M-1"]),
            &fake,
            &jobs,
        )
        .await;
    }

    #[tokio::test]
    async fn presence_and_chat_state_updates_are_published_with_a_display_name() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        let mut events = shared.events.subscribe();
        let chat_presence = |state, media| {
            Event::ChatPresence(
                sdk::ChatPresenceUpdate::builder()
                    .source(MessageSource {
                        chat: jid(GROUP),
                        sender: jid(CHAT),
                        is_group: true,
                        ..Default::default()
                    })
                    .state(state)
                    .media(media)
                    .build(),
            )
        };

        // Without a stored name the update still names the sender's JID only.
        apply(
            &shared,
            generation,
            chat_presence(ChatPresence::Composing, ChatPresenceMedia::Text),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::ChatState {
                chat_jid: GROUP.into(),
                sender_jid: CHAT.into(),
                sender_name: String::new(),
                state: ChatState::Typing,
            }
        );

        shared
            .database
            .update_address_book_name(CHAT, "Ada")
            .unwrap();
        apply(
            &shared,
            generation,
            chat_presence(ChatPresence::Composing, ChatPresenceMedia::Audio),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::ChatState {
                chat_jid: GROUP.into(),
                sender_jid: CHAT.into(),
                sender_name: "Ada".into(),
                state: ChatState::Recording,
            }
        );

        apply(
            &shared,
            generation,
            chat_presence(ChatPresence::Paused, ChatPresenceMedia::Text),
            &fake,
            &jobs,
        )
        .await;
        assert!(matches!(
            events.try_recv().unwrap().event,
            ServerEvent::ChatState {
                state: ChatState::Paused,
                ..
            }
        ));

        apply(
            &shared,
            generation,
            Event::Presence(
                sdk::PresenceUpdate::builder()
                    .from(jid(CHAT))
                    .unavailable(false)
                    .last_seen(stamp(NOW))
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Presence {
                jid: CHAT.into(),
                available: true,
                last_seen: Some(NOW),
            }
        );
    }

    #[tokio::test]
    async fn undecryptable_messages_and_call_events_only_notify() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());

        apply(
            &shared,
            generation,
            Event::UndecryptableMessage(
                sdk::UndecryptableMessage::builder()
                    .info(Arc::new(MessageInfo {
                        source: MessageSource {
                            chat: jid(CHAT),
                            sender: jid(CHAT),
                            ..Default::default()
                        },
                        id: "M-1".into(),
                        timestamp: stamp(NOW),
                        ..Default::default()
                    }))
                    .is_unavailable(true)
                    .unavailable_type(UnavailableType::Unknown)
                    .decrypt_fail_mode(DecryptFailMode::Show)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;

        let offer = |offline: bool, notify: Option<&str>| {
            let mut call = IncomingCall::new_for_test(
                jid(CHAT),
                "stanza-1".into(),
                stamp(NOW),
                CallAction::OfferNotice {
                    call_id: "call-1".into(),
                    call_creator: jid(CHAT),
                    is_video: false,
                    is_group: false,
                },
            );
            call.offline = offline;
            call.notify = notify.map(str::to_owned);
            Event::IncomingCall(call)
        };
        apply(&shared, generation, offer(false, Some("Ada")), &fake, &jobs).await;
        apply(&shared, generation, offer(false, None), &fake, &jobs).await;
        // A replayed offer from the offline queue must never ring.
        apply(&shared, generation, offer(true, None), &fake, &jobs).await;
        apply(
            &shared,
            generation,
            Event::IncomingCall(IncomingCall::new_for_test(
                jid(CHAT),
                "stanza-2".into(),
                stamp(NOW),
                CallAction::Reject {
                    call_id: "call-1".into(),
                    call_creator: jid(CHAT),
                    reason: None,
                },
            )),
            &fake,
            &jobs,
        )
        .await;

        apply(
            &shared,
            generation,
            Event::MissedCall(MissedCall::new(
                jid(CHAT),
                "call-2".into(),
                stamp(NOW),
                MissedReason::Offline,
            )),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            Event::CallEndedElsewhere(CallEndedElsewhere::new(
                jid(CHAT),
                "call-3".into(),
                stamp(NOW),
                ElsewhereOutcome::Accepted,
            )),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            Event::IdentityChange(
                sdk::IdentityChange::builder()
                    .user(jid(CHAT))
                    .implicit(true)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;

        // None of these touch local state beyond the app-state heartbeat.
        assert!(shared.database.list_chats(10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn picture_updates_remove_the_cached_avatar_or_refresh_it() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        let picture = |removed| {
            Event::PictureUpdate(
                sdk::PictureUpdate::builder()
                    .jid(jid(CHAT))
                    .timestamp(stamp(NOW))
                    .removed(removed)
                    .build(),
            )
        };
        assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, CHAT), b"avatar")
            .unwrap();

        apply(&shared, generation, picture(true), &fake, &jobs).await;
        assert!(!assets::avatar_path(&shared.avatar_dir, CHAT).exists());
        assert!(assets::avatar_missing_path(&shared.avatar_dir, CHAT).exists());

        apply(&shared, generation, picture(false), &fake, &jobs).await;
        jobs.abort_all();

        // An unwritable avatar directory is reported instead of panicking.
        let mut blocked = test_shared(&directory);
        blocked.avatar_dir = directory.path().join("avatars/marker/blocked");
        let blocked = Arc::new(blocked);
        let blocked_generation = blocked.clock.begin_generation();
        apply(&blocked, blocked_generation, picture(true), &fake, &jobs).await;
        assert!(!assets::avatar_missing_path(&blocked.avatar_dir, CHAT).exists());
    }

    #[tokio::test]
    async fn contact_notifications_refresh_names_avatars_and_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());

        apply(
            &shared,
            generation,
            Event::ContactUpdated(
                sdk::ContactUpdated::builder()
                    .jid(jid(CHAT))
                    .timestamp(stamp(NOW))
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            Event::ContactSyncRequested(
                sdk::ContactSyncRequested::builder()
                    .timestamp(stamp(NOW))
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;

        shared
            .database
            .update_address_book_name(CHAT, "Ada")
            .unwrap();
        shared
            .database
            .update_address_book_name("100000000000001@lid", "Ada")
            .unwrap();
        apply(
            &shared,
            generation,
            Event::ContactNumberChanged(
                sdk::ContactNumberChanged::builder()
                    .old_jid(jid(CHAT))
                    .new_jid(jid(OTHER))
                    .old_lid(jid("100000000000001@lid"))
                    .new_lid(jid("100000000000002@lid"))
                    .timestamp(stamp(NOW))
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            shared.database.contact_name(OTHER).unwrap().as_deref(),
            Some("Ada")
        );
        assert_eq!(shared.database.contact_name(CHAT).unwrap(), None);

        // Without LIDs only the phone number is migrated, and an unusable store
        // is reported instead of dropping the notification.
        shared
            .database
            .update_address_book_name("100000000000003@lid", "Ada")
            .unwrap();
        shared
            .database
            .execute_test_sql("DROP TABLE message_tombstones")
            .unwrap();
        apply(
            &shared,
            generation,
            Event::ContactNumberChanged(
                sdk::ContactNumberChanged::builder()
                    .old_jid(jid(OTHER))
                    .new_jid(jid("31600000003@s.whatsapp.net"))
                    .timestamp(stamp(NOW))
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            Event::ContactNumberChanged(
                sdk::ContactNumberChanged::builder()
                    .old_jid(jid(OTHER))
                    .new_jid(jid("31600000003@s.whatsapp.net"))
                    .old_lid(jid("100000000000003@lid"))
                    .new_lid(jid("100000000000004@lid"))
                    .timestamp(stamp(NOW))
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            shared.database.contact_name(OTHER).unwrap().as_deref(),
            Some("Ada")
        );
        jobs.abort_all();
    }

    #[tokio::test]
    async fn push_and_business_names_are_stored_or_reported() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        let push_name = |name: &str| {
            Event::PushNameUpdate(
                sdk::PushNameUpdate::builder()
                    .jid(jid(CHAT))
                    .message(Box::new(MessageInfo {
                        source: MessageSource {
                            chat: jid(CHAT),
                            sender: jid(CHAT),
                            ..Default::default()
                        },
                        id: "M-1".into(),
                        timestamp: stamp(NOW),
                        ..Default::default()
                    }))
                    .old_push_name(String::new())
                    .new_push_name(name.to_owned())
                    .build(),
            )
        };
        let business = |name: Option<&str>| {
            Event::BusinessStatusUpdate(
                sdk::BusinessStatusUpdate::builder()
                    .jid(jid(OTHER))
                    .update_type(sdk::BusinessUpdateType::VerifiedNameChanged)
                    .timestamp(stamp(NOW))
                    .maybe_verified_name(name.map(str::to_owned))
                    .product_ids(Vec::new())
                    .collection_ids(Vec::new())
                    .subscriptions(Vec::new())
                    .build(),
            )
        };

        apply(&shared, generation, push_name("Ada"), &fake, &jobs).await;
        assert_eq!(
            shared.database.contact_name(CHAT).unwrap().as_deref(),
            Some("Ada")
        );
        apply(
            &shared,
            generation,
            business(Some("Ada's Flowers")),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            shared.database.contact_name(OTHER).unwrap().as_deref(),
            Some("Ada's Flowers")
        );
        apply(&shared, generation, business(None), &fake, &jobs).await;

        shared
            .database
            .execute_test_sql("DROP TABLE contacts")
            .unwrap();
        apply(&shared, generation, push_name("Grace"), &fake, &jobs).await;
        apply(
            &shared,
            generation,
            business(Some("Grace's Garden")),
            &fake,
            &jobs,
        )
        .await;
        jobs.abort_all();
    }

    #[tokio::test]
    async fn a_restored_profile_name_re_applies_the_deferred_presence() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        let renamed = |name: &str| {
            Event::SelfPushNameUpdated(
                sdk::SelfPushNameUpdated::builder()
                    .from_server(true)
                    .old_name(String::new())
                    .new_name(name.to_owned())
                    .build(),
            )
        };

        // Nothing is requested while the shell has not asked to be available.
        apply(&shared, generation, renamed("Ada"), &fake, &jobs).await;
        assert!(fake.calls_of(CallKind::SetAvailable).is_empty());

        let connection = shared.open_connection();
        shared.set_connection_available(connection, true).unwrap();
        // An empty replacement name still cannot restore presence.
        apply(&shared, generation, renamed(""), &fake, &jobs).await;
        assert!(fake.calls_of(CallKind::SetAvailable).is_empty());

        apply(&shared, generation, renamed("Ada"), &fake, &jobs).await;
        assert_eq!(
            fake.calls_of(CallKind::SetAvailable),
            vec![Call::SetAvailable]
        );

        fake.fail(CallKind::SetAvailable, "synthetic presence failure");
        apply(&shared, generation, renamed("Ada"), &fake, &jobs).await;
        assert_eq!(fake.calls_of(CallKind::SetAvailable).len(), 2);
    }

    #[tokio::test]
    async fn cross_device_chat_settings_are_applied_with_their_timestamps() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        shared
            .database
            .insert_message(&stored_message("M-1", CHAT, false), "Ada", false, true)
            .unwrap();
        let pin = |pinned: Option<bool>| {
            Event::PinUpdate(
                sdk::PinUpdate::builder()
                    .jid(jid(CHAT))
                    .timestamp(stamp(NOW))
                    .action(Box::new(wa::sync_action_value::PinAction { pinned }))
                    .from_full_sync(false)
                    .build(),
            )
        };
        let mute = |muted: Option<bool>| {
            Event::MuteUpdate(
                sdk::MuteUpdate::builder()
                    .jid(jid(CHAT))
                    .timestamp(stamp(NOW))
                    .action(Box::new(wa::sync_action_value::MuteAction {
                        muted,
                        mute_end_timestamp: None,
                        ..Default::default()
                    }))
                    .from_full_sync(false)
                    .build(),
            )
        };
        let archive = |archived: Option<bool>| {
            Event::ArchiveUpdate(
                sdk::ArchiveUpdate::builder()
                    .jid(jid(CHAT))
                    .timestamp(stamp(NOW))
                    .action(Box::new(wa::sync_action_value::ArchiveChatAction {
                        archived,
                        ..Default::default()
                    }))
                    .from_full_sync(false)
                    .build(),
            )
        };
        let read = |value: Option<bool>| {
            Event::MarkChatAsReadUpdate(
                sdk::MarkChatAsReadUpdate::builder()
                    .jid(jid(CHAT))
                    .timestamp(stamp(NOW))
                    .action(Box::new(wa::sync_action_value::MarkChatAsReadAction {
                        read: value,
                        message_range: MessageField::some(
                            wa::sync_action_value::SyncActionMessageRange {
                                last_message_timestamp: Some(NOW),
                                messages: vec![wa::sync_action_value::SyncActionMessage {
                                    key: MessageField::some(wa::MessageKey {
                                        id: Some("M-1".into()),
                                        ..Default::default()
                                    }),
                                    timestamp: Some(NOW),
                                }],
                                ..Default::default()
                            },
                        ),
                    }))
                    .from_full_sync(true)
                    .build(),
            )
        };

        apply(&shared, generation, pin(Some(true)), &fake, &jobs).await;
        apply(&shared, generation, pin(None), &fake, &jobs).await;
        apply(&shared, generation, mute(Some(true)), &fake, &jobs).await;
        apply(&shared, generation, mute(None), &fake, &jobs).await;
        apply(&shared, generation, archive(Some(true)), &fake, &jobs).await;
        apply(&shared, generation, archive(None), &fake, &jobs).await;
        apply(&shared, generation, read(Some(true)), &fake, &jobs).await;
        apply(&shared, generation, read(None), &fake, &jobs).await;

        let chats = shared.database.list_chats(10).unwrap();
        assert!(chats[0].pinned);
        assert!(chats[0].muted);
        assert_eq!(chats[0].unread, 0);
        assert!(shared.database.is_muted(CHAT, NOW).unwrap());

        shared
            .database
            .execute_test_sql("DROP TABLE chat_settings")
            .unwrap();
        apply(&shared, generation, pin(Some(false)), &fake, &jobs).await;
        apply(&shared, generation, mute(Some(false)), &fake, &jobs).await;
        apply(&shared, generation, archive(Some(false)), &fake, &jobs).await;
        apply(&shared, generation, read(Some(false)), &fake, &jobs).await;
    }

    #[tokio::test]
    async fn cross_device_deletions_remove_messages_and_their_media() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        for id in ["M-1", "M-2", "M-3"] {
            shared
                .database
                .insert_message(&stored_message(id, CHAT, false), "Ada", false, true)
                .unwrap();
        }
        std::fs::write(
            assets::message_image_path(&shared.media_dir, CHAT, "M-1"),
            b"image",
        )
        .unwrap();

        apply(
            &shared,
            generation,
            Event::DeleteMessageForMeUpdate(
                sdk::DeleteMessageForMeUpdate::builder()
                    .chat_jid(jid(CHAT))
                    .message_id("M-1".to_owned())
                    .from_me(false)
                    .timestamp(stamp(NOW + 1))
                    .action(Box::new(
                        wa::sync_action_value::DeleteMessageForMeAction::default(),
                    ))
                    .from_full_sync(false)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert!(!assets::message_image_path(&shared.media_dir, CHAT, "M-1").exists());
        assert_eq!(shared.database.messages(CHAT, 10).unwrap().len(), 2);

        apply(
            &shared,
            generation,
            Event::ClearChatUpdate(
                sdk::ClearChatUpdate::builder()
                    .jid(jid(CHAT))
                    .delete_starred(false)
                    .delete_media(true)
                    .timestamp(stamp(NOW + 2))
                    .action(Box::new(wa::sync_action_value::ClearChatAction::default()))
                    .from_full_sync(false)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert!(shared.database.messages(CHAT, 10).unwrap().is_empty());

        apply(
            &shared,
            generation,
            Event::DeleteChatUpdate(
                sdk::DeleteChatUpdate::builder()
                    .jid(jid(CHAT))
                    .delete_media(true)
                    .timestamp(stamp(NOW + 3))
                    .action(Box::new(wa::sync_action_value::DeleteChatAction::default()))
                    .from_full_sync(false)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert!(shared.database.list_chats(10).unwrap().is_empty());

        shared
            .database
            .execute_test_sql("DROP TABLE chat_settings; DROP TABLE messages;")
            .unwrap();
        apply(
            &shared,
            generation,
            Event::DeleteMessageForMeUpdate(
                sdk::DeleteMessageForMeUpdate::builder()
                    .chat_jid(jid(CHAT))
                    .message_id("M-2".to_owned())
                    .from_me(false)
                    .timestamp(stamp(NOW + 4))
                    .action(Box::new(
                        wa::sync_action_value::DeleteMessageForMeAction::default(),
                    ))
                    .from_full_sync(false)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            Event::ClearChatUpdate(
                sdk::ClearChatUpdate::builder()
                    .jid(jid(CHAT))
                    .delete_starred(false)
                    .delete_media(false)
                    .timestamp(stamp(NOW + 5))
                    .action(Box::new(wa::sync_action_value::ClearChatAction::default()))
                    .from_full_sync(false)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        apply(
            &shared,
            generation,
            Event::DeleteChatUpdate(
                sdk::DeleteChatUpdate::builder()
                    .jid(jid(CHAT))
                    .delete_media(false)
                    .timestamp(stamp(NOW + 6))
                    .action(Box::new(wa::sync_action_value::DeleteChatAction::default()))
                    .from_full_sync(false)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
    }

    #[tokio::test]
    async fn offline_sync_and_dirty_state_schedule_the_recovery_passes() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        shared.begin_presence_sync(generation);

        apply(
            &shared,
            generation,
            Event::OfflineSyncCompleted(sdk::OfflineSyncCompleted::builder().count(3).build()),
            &fake,
            &jobs,
        )
        .await;
        assert!(!shared.presence_sync_pending());

        // A second completion has no deferred presence left to reconcile.
        apply(
            &shared,
            generation,
            Event::OfflineSyncCompleted(sdk::OfflineSyncCompleted::builder().count(0).build()),
            &fake,
            &jobs,
        )
        .await;

        apply(
            &shared,
            generation,
            Event::DirtyState(
                sdk::DirtyState::builder()
                    .dirty_type(whatsapp_rust::wacore::iq::dirty::DirtyType::Groups)
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        jobs.abort_all();
    }

    #[tokio::test]
    async fn terminal_stream_events_move_the_connection_status() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        *shared.client.write().await = Some(crate::transport::fake::transport(&fake));

        apply(
            &shared,
            generation,
            Event::StreamReplaced(sdk::StreamReplaced::builder().build()),
            &fake,
            &jobs,
        )
        .await;
        assert!(shared.client.read().await.is_none());
        assert!(matches!(
            *shared.status.read().await,
            ConnectionStatus::Disconnected { .. }
        ));

        apply(
            &shared,
            generation,
            Event::TemporaryBan(
                sdk::TemporaryBan::builder()
                    .code(sdk::TempBanReason::BlockedByUsers)
                    .expire(chrono::Duration::seconds(3_600))
                    .message("Synthetic restriction".to_owned())
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert_eq!(
            *shared.status.read().await,
            ConnectionStatus::Error {
                message: "Synthetic restriction".into(),
            }
        );

        apply(
            &shared,
            generation,
            Event::TemporaryBan(
                sdk::TemporaryBan::builder()
                    .code(sdk::TempBanReason::BroadcastList)
                    .expire(chrono::Duration::seconds(60))
                    .build(),
            ),
            &fake,
            &jobs,
        )
        .await;
        assert!(matches!(
            *shared.status.read().await,
            ConnectionStatus::Error { ref message }
                if message.starts_with("Temporary WhatsApp restriction")
        ));

        for (event, expected) in [
            (
                Event::PairError(
                    sdk::PairError::builder()
                        .id(jid(CHAT))
                        .lid(jid("100000000000001@lid"))
                        .business_name(String::new())
                        .platform("android".to_owned())
                        .error("synthetic".to_owned())
                        .build(),
                ),
                ConnectionStatus::Error {
                    message: "WhatsApp device pairing failed; retrying".into(),
                },
            ),
            (
                Event::QrScannedWithoutMultidevice(
                    sdk::QrScannedWithoutMultidevice::builder().build(),
                ),
                ConnectionStatus::Error {
                    message: "Enable multi-device WhatsApp and scan the new QR code".into(),
                },
            ),
            (
                Event::ClientOutdated(sdk::ClientOutdated::builder().build()),
                ConnectionStatus::Error {
                    message: "This WhatsApp client version is no longer accepted".into(),
                },
            ),
            (
                Event::PairingQrCodesExhausted(
                    sdk::PairingQrCodesExhausted::builder()
                        .disconnected(true)
                        .build(),
                ),
                ConnectionStatus::Disconnected {
                    reason: "Pairing QR expired; generating a new code".into(),
                },
            ),
        ] {
            apply(&shared, generation, event, &fake, &jobs).await;
            assert_eq!(*shared.status.read().await, expected);
        }

        // Server acks are observed; only a rejected message is surfaced.
        for ack in [
            sdk::ServerAck::builder()
                .id("M-1".to_owned())
                .class("message".to_owned())
                .error("479".to_owned())
                .build(),
            sdk::ServerAck::builder()
                .id("R-1".to_owned())
                .class("receipt".to_owned())
                .error("500".to_owned())
                .build(),
            sdk::ServerAck::builder().id("M-2".to_owned()).build(),
        ] {
            apply(&shared, generation, Event::ServerAck(ack), &fake, &jobs).await;
        }

        // An event this adapter does not subscribe to is ignored.
        apply(
            &shared,
            generation,
            Event::Connected(sdk::Connected::builder().build()),
            &fake,
            &jobs,
        )
        .await;
    }

    #[tokio::test]
    async fn a_failed_app_state_sync_reports_and_disarms_the_resync() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let jobs = Arc::new(GenerationJobs::default());
        crate::state::write_private_marker(&shared.event_sync_marker).unwrap();
        let mut events = shared.events.subscribe();
        let failure = |fatal: Vec<String>, connected: bool| {
            Event::AppStateSyncFailed(
                sdk::AppStateSyncFailed::builder()
                    .fatal(fatal)
                    .retryable(Vec::new())
                    .skipped(Vec::new())
                    .connected(connected)
                    .build(),
            )
        };

        // A retryable failure on a connected client keeps the status.
        apply(&shared, generation, failure(Vec::new(), true), &fake, &jobs).await;
        assert!(!shared.event_sync_marker.exists());
        assert!(shared.app_state_failed.load(Ordering::Relaxed));
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|frame| matches!(frame.event, ServerEvent::State { .. }))
        );

        shared
            .chat_state_resync_requested
            .store(true, Ordering::SeqCst);
        apply(
            &shared,
            generation,
            failure(vec!["regular".to_owned()], true),
            &fake,
            &jobs,
        )
        .await;
        let published = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert!(published.iter().any(|event| matches!(
            event,
            ServerEvent::ChatStateResync {
                status: ChatStateResyncStatus::Failed,
                ..
            }
        )));
        assert!(
            published
                .iter()
                .any(|event| matches!(event, ServerEvent::State { .. }))
        );

        // A disconnected failure also surfaces as a connection error.
        apply(
            &shared,
            generation,
            failure(Vec::new(), false),
            &fake,
            &jobs,
        )
        .await;
        assert!(matches!(
            *shared.status.read().await,
            ConnectionStatus::Error { .. }
        ));
    }

    #[tokio::test]
    async fn contact_update_events_ingest_names_and_reconcile_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let update = Arc::new(Event::ContactUpdate(
            whatsapp_rust::types::events::ContactUpdate::builder()
                .jid(jid(CHAT))
                .timestamp(stamp(NOW))
                .action(Box::new(wa::sync_action_value::ContactAction {
                    full_name: Some("Ada Lovelace".into()),
                    ..Default::default()
                }))
                .from_full_sync(false)
                .build(),
        ));

        // Another event kind is not this handler's work.
        process_contact_event(
            Arc::clone(&shared),
            generation,
            Arc::new(Event::StreamReplaced(
                sdk::StreamReplaced::builder().build(),
            )),
            crate::transport::fake::transport(&fake),
        )
        .await;
        // A retired generation stops before any local write.
        process_contact_event(
            Arc::clone(&shared),
            generation.saturating_add(1),
            Arc::clone(&update),
            crate::transport::fake::transport(&fake),
        )
        .await;
        assert_eq!(shared.database.contact_name(CHAT).unwrap(), None);

        process_contact_event(
            Arc::clone(&shared),
            generation,
            update,
            crate::transport::fake::transport(&fake),
        )
        .await;
        assert_eq!(
            shared.database.contact_name(CHAT).unwrap().as_deref(),
            Some("Ada Lovelace")
        );
    }

    #[tokio::test]
    async fn group_update_events_refresh_the_subject_and_participants() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, _) = linked(&directory);
        let fake = Arc::new(FakeTransport::new().with_group_metadata(
            GROUP,
            whatsapp_rust::GroupMetadata {
                subject: "Garden".into(),
                ..whatsapp_rust::GroupMetadata::default()
            },
        ));
        let update = Arc::new(Event::GroupUpdate(
            sdk::GroupUpdate::builder()
                .group_jid(jid(GROUP))
                .timestamp(stamp(NOW))
                .is_lid_addressing_mode(false)
                .action(whatsapp_rust::wacore::stanza::groups::GroupNotificationAction::Unlocked)
                .build(),
        ));
        shared
            .database
            .insert_message(&stored_message("G-1", GROUP, false), "", true, false)
            .unwrap();
        let mut events = shared.events.subscribe();

        // Another event kind is not this handler's work.
        process_group_event(
            Arc::clone(&shared),
            generation,
            Arc::new(Event::StreamReplaced(
                sdk::StreamReplaced::builder().build(),
            )),
            crate::transport::fake::transport(&fake),
        )
        .await;

        process_group_event(
            Arc::clone(&shared),
            generation,
            Arc::clone(&update),
            crate::transport::fake::transport(&fake),
        )
        .await;
        assert_eq!(
            shared.database.chat_name(GROUP).unwrap().as_deref(),
            Some("Garden")
        );
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(|frame| matches!(
                frame.event,
                ServerEvent::GroupParticipants { ref chat_jid, .. } if chat_jid == GROUP
            ))
        );

        // A repeated refresh changes nothing.
        process_group_event(
            Arc::clone(&shared),
            generation,
            Arc::clone(&update),
            crate::transport::fake::transport(&fake),
        )
        .await;

        // A refused update is reported and a retired generation stops early.
        process_group_event(
            Arc::clone(&shared),
            generation.saturating_add(1),
            Arc::clone(&update),
            crate::transport::fake::transport(&fake),
        )
        .await;
        shared
            .database
            .execute_test_sql(
                "CREATE TRIGGER block_group_subject BEFORE UPDATE ON chats
                 WHEN NEW.name_source = 30
                 BEGIN SELECT RAISE(ABORT, 'synthetic subject failure'); END;",
            )
            .unwrap();
        shared
            .database
            .execute_test_sql("UPDATE chats SET name = '', name_source = 0")
            .unwrap();
        process_group_event(
            Arc::clone(&shared),
            generation,
            Arc::clone(&update),
            crate::transport::fake::transport(&fake),
        )
        .await;

        let unavailable = Arc::new(FakeTransport::new());
        process_group_event(
            Arc::clone(&shared),
            generation,
            update,
            crate::transport::fake::transport(&unavailable),
        )
        .await;
        assert_eq!(
            unavailable.calls_of(CallKind::GroupMetadata),
            vec![Call::GroupMetadata(GROUP.to_owned())]
        );
    }

    #[tokio::test]
    async fn the_app_event_reducer_runs_current_work_in_order() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let (app_sender, app_queue) = mpsc::channel(8);
        let jobs = Arc::new(GenerationJobs::default());
        shared
            .database
            .insert_message(&stored_message("M-1", CHAT, true), "Ada", false, false)
            .unwrap();

        for work_generation in [generation, generation.saturating_add(1)] {
            app_sender
                .send(AppEventWork {
                    generation: work_generation,
                    event: Arc::new(receipt(CHAT, CHAT, ReceiptType::Delivered, &["M-1"])),
                    transport: crate::transport::fake::transport(&fake),
                    jobs: Arc::clone(&jobs),
                })
                .await
                .unwrap();
        }
        drop(app_sender);
        run_app_event_reducer(Arc::clone(&shared), app_queue).await;

        assert_eq!(shared.database.messages(CHAT, 10).unwrap()[0].receipt, 2);
        assert_eq!(fake.calls_of(CallKind::LidPnEntry).len(), 2);
    }

    #[tokio::test]
    async fn the_message_reducer_drains_the_durable_inbox_until_it_is_stale() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let (message_sender, message_queue) = mpsc::channel(8);
        let record = |id: &str, committed_at: i64| inbound::DurableInbound {
            key: inbound::InboundKey {
                chat_jid: CHAT.into(),
                sender_jid: CHAT.into(),
                message_id: id.into(),
            },
            message: buffa::Message::encode_to_vec(&wa::Message::text("durable")),
            push_name: "Ada".into(),
            timestamp: NOW,
            media_type: "text".into(),
            is_from_me: false,
            is_group: false,
            is_offline: true,
            committed_at,
        };
        let mut corrupt = record("CORRUPT", 1);
        corrupt.message = vec![0xff];
        shared
            .database
            .commit_inbound_batch(&[corrupt, record("M-1", 2), record("M-2", 3)])
            .unwrap();
        // The first reduced message retires the generation, exactly as the run
        // loop can while the reducer is mid-drain.
        let clock = Arc::clone(&shared.clock);
        fake.on_call(move |kind| {
            if kind == CallKind::LidPnEntry {
                clock.retire_generation(generation);
            }
        });

        // Stale work is dropped before the inbox is even read.
        message_sender
            .send(MessageWork::Drain {
                generation: generation.saturating_add(5),
                transport: crate::transport::fake::transport(&fake),
            })
            .await
            .unwrap();
        message_sender
            .send(MessageWork::Drain {
                generation,
                transport: crate::transport::fake::transport(&fake),
            })
            .await
            .unwrap();
        drop(message_sender);
        run_message_reducer(Arc::clone(&shared), message_queue).await;

        // Nothing was acknowledged: the retired generation stopped the drain.
        assert_eq!(shared.database.pending_inbound(10).unwrap().len(), 3);
        assert!(shared.database.messages(CHAT, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_durable_inbox_is_reported_and_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let (message_sender, message_queue) = mpsc::channel(8);
        shared
            .database
            .execute_test_sql("DROP TABLE inbound_inbox")
            .unwrap();

        message_sender
            .send(MessageWork::Drain {
                generation,
                transport: crate::transport::fake::transport(&fake),
            })
            .await
            .unwrap();
        drop(message_sender);
        run_message_reducer(Arc::clone(&shared), message_queue).await;

        assert!(fake.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_app_state_waiter_settles_expires_and_stops_on_a_retired_client() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, _) = linked(&directory);

        // A retired generation and a failed replay both stop immediately.
        await_app_state_sync(Arc::clone(&shared), generation.saturating_add(1)).await;
        shared.app_state_failed.store(true, Ordering::Relaxed);
        await_app_state_sync(Arc::clone(&shared), generation).await;
        shared.app_state_failed.store(false, Ordering::Relaxed);
        assert!(!shared.event_sync_marker.exists());

        // A replay that never reports a complete quiet checkpoint expires.
        rusqlite::Connection::open(directory.path().join("session.db"))
            .unwrap()
            .execute_batch("CREATE TABLE app_state_versions (other TEXT NOT NULL);")
            .unwrap();
        await_app_state_sync(Arc::clone(&shared), generation).await;
        assert!(!shared.event_sync_marker.exists());

        // A replay that keeps mutating restarts its quiet window until the
        // deadline.
        shared.app_state_activity_ms.store(
            u64::try_from(Utc::now().timestamp_millis()).unwrap(),
            Ordering::Relaxed,
        );
        await_app_state_sync(Arc::clone(&shared), generation).await;

        // A settled, complete replay reconciles and marks the sync done.
        shared.app_state_activity_ms.store(0, Ordering::Relaxed);
        std::fs::remove_file(directory.path().join("session.db")).unwrap();
        seed_completed_app_state(&directory);
        await_app_state_sync(Arc::clone(&shared), generation).await;
        assert!(shared.event_sync_marker.exists());
    }

    #[test]
    fn app_state_quiet_period_restarts_with_every_mutation() {
        assert_eq!(app_state_quiet_remaining(10_000, 0), None);
        assert_eq!(
            app_state_quiet_remaining(10_000, 9_500),
            Some(std::time::Duration::from_millis(1_500))
        );
        assert_eq!(app_state_quiet_remaining(10_000, 8_000), None);
        // A clock that jumped backwards restarts the window instead of
        // declaring the replay settled.
        assert_eq!(
            app_state_quiet_remaining(1_000, 9_000),
            Some(APP_STATE_QUIET_PERIOD)
        );
    }

    #[tokio::test]
    async fn settled_app_state_replay_reconciles_and_reports_success() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        seed_completed_app_state(&directory);
        shared
            .database
            .insert_message(&unread_message("replayed"), "Ada", false, true)
            .unwrap();
        let mut events = shared.events.subscribe();

        finish_app_state_sync(&shared, 1).await;
        assert!(shared.event_sync_marker.exists());
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|frame| matches!(frame.event, ServerEvent::ChatStateResync { .. })),
            "an unrequested replay reports no resync outcome"
        );

        shared
            .chat_state_resync_requested
            .store(true, Ordering::SeqCst);
        finish_app_state_sync(&shared, 1).await;
        assert!(!shared.chat_state_resync_requested.load(Ordering::SeqCst));
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(|frame| frame.event
                == ServerEvent::ChatStateResync {
                    status: ChatStateResyncStatus::Succeeded,
                    message: Some("WhatsApp chat state is up to date".into()),
                })
        );
    }

    #[tokio::test]
    async fn unreconcilable_app_state_replay_reports_failure() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        seed_completed_app_state(&directory);
        shared
            .database
            .execute_test_sql("DROP TABLE chats")
            .unwrap();
        let mut events = shared.events.subscribe();

        finish_app_state_sync(&shared, 1).await;
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|frame| matches!(frame.event, ServerEvent::ChatStateResync { .. })),
            "an unrequested replay reports no resync outcome"
        );

        shared
            .chat_state_resync_requested
            .store(true, Ordering::SeqCst);
        finish_app_state_sync(&shared, 1).await;

        assert!(!shared.event_sync_marker.exists());
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(|frame| frame.event
                == ServerEvent::ChatStateResync {
                    status: ChatStateResyncStatus::Failed,
                    message: Some("Could not reconcile the replayed WhatsApp chat state".into()),
                })
        );
    }

    #[tokio::test]
    async fn expired_app_state_replay_reports_the_deadline_once() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let mut events = shared.events.subscribe();

        expire_app_state_sync(&shared, 1).await;
        assert!(events.try_recv().is_err());

        shared
            .chat_state_resync_requested
            .store(true, Ordering::SeqCst);
        expire_app_state_sync(&shared, 1).await;
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::ChatStateResync {
                status: ChatStateResyncStatus::Failed,
                message: Some("WhatsApp did not finish the chat-state replay".into()),
            }
        );
        assert!(!shared.chat_state_resync_requested.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn ordered_reducers_honor_stale_work_and_barriers() {
        let directory = tempfile::tempdir().unwrap();
        let transport: Arc<dyn Transport> =
            Arc::new(ClientTransport(synthetic_client(&directory).await));
        let (events, _) = broadcast::channel(8);
        let (message_sender, message_queue) = mpsc::channel(8);
        let (app_sender, app_queue) = mpsc::channel(8);
        let shared = Arc::new(test_shared_with_reducers(
            &directory,
            events,
            message_sender.clone(),
            app_sender.clone(),
        ));
        let generation = shared.clock.begin_generation();
        finish_inbound_reduction(
            &shared,
            &inbound::InboundKey {
                chat_jid: "ignored".into(),
                sender_jid: "ignored".into(),
                message_id: "ignored".into(),
            },
            false,
        );
        *shared.client.write().await = Some(Arc::clone(&transport));
        assert_eq!(
            canonical_requested_jid(&shared, "1:2@s.whatsapp.net").await,
            "1@s.whatsapp.net"
        );
        let message_task = tokio::spawn(run_message_reducer(Arc::clone(&shared), message_queue));
        let app_task = tokio::spawn(run_app_event_reducer(Arc::clone(&shared), app_queue));

        let info = |chat: &str, id: &str, push_name: &str| MessageInfo {
            source: whatsapp_rust::wacore::types::message::MessageSource {
                chat: chat.parse().unwrap(),
                sender: chat.parse().unwrap(),
                ..Default::default()
            },
            id: id.into(),
            push_name: push_name.into(),
            timestamp: Utc::now(),
            ..Default::default()
        };
        let key = |chat: &str, id: &str| inbound::InboundKey {
            chat_jid: chat.into(),
            sender_jid: chat.into(),
            message_id: id.into(),
        };

        message_sender
            .send(MessageWork::Drain {
                generation: generation.saturating_add(1),
                transport: Arc::clone(&transport),
            })
            .await
            .unwrap();
        message_sender
            .send(MessageWork::Drain {
                generation,
                transport: Arc::clone(&transport),
            })
            .await
            .unwrap();
        message_sender
            .send(MessageWork::Live {
                generation: generation.saturating_add(1),
                message: Arc::new(wa::Message::text("stale")),
                info: Box::new(info("1@s.whatsapp.net", "stale", "")),
                transport: Arc::clone(&transport),
                key: key("1@s.whatsapp.net", "stale"),
            })
            .await
            .unwrap();

        reduce_message(
            &shared,
            generation,
            Arc::new(wa::Message::text("current")),
            info("2@s.whatsapp.net", "current", "Synthetic"),
            Arc::clone(&transport),
            &key("2@s.whatsapp.net", "current"),
        )
        .await;
        reduce_message(
            &shared,
            generation,
            Arc::new(wa::Message::text("current")),
            info("2@s.whatsapp.net", "current", "Synthetic"),
            Arc::clone(&transport),
            &key("2@s.whatsapp.net", "current"),
        )
        .await;

        shared
            .database
            .execute_test_sql("DROP TABLE inbound_inbox")
            .unwrap();
        reduce_message(
            &shared,
            generation,
            Arc::new(wa::Message::text("still durable locally")),
            info("2@s.whatsapp.net", "no-inbox", "Synthetic"),
            Arc::clone(&transport),
            &key("2@s.whatsapp.net", "no-inbox"),
        )
        .await;
        let (barrier, drained) = oneshot::channel();
        message_sender
            .send(MessageWork::Barrier(barrier))
            .await
            .unwrap();
        drained.await.unwrap();

        app_sender
            .send(AppEventWork {
                generation: generation.saturating_add(1),
                event: Arc::new(Event::Receipt(
                    Receipt::builder()
                        .source(MessageSource {
                            chat: "1@s.whatsapp.net".parse().unwrap(),
                            sender: "1@s.whatsapp.net".parse().unwrap(),
                            ..Default::default()
                        })
                        .message_ids(vec!["stale".into()])
                        .timestamp(Utc::now())
                        .r#type(ReceiptType::Read)
                        .offline(false)
                        .build(),
                )),
                transport: Arc::clone(&transport),
                jobs: Arc::new(GenerationJobs::default()),
            })
            .await
            .unwrap();

        tokio::task::yield_now().await;
        message_task.abort();
        app_task.abort();
        assert!(message_task.await.unwrap_err().is_cancelled());
        assert!(app_task.await.unwrap_err().is_cancelled());
        assert_eq!(
            shared.database.list_chats(1).unwrap()[0].jid,
            "2@s.whatsapp.net"
        );
    }

    #[test]
    fn incoming_chat_presence_maps_text_audio_and_pause() {
        assert_eq!(
            protocol_chat_state(ChatPresence::Composing, ChatPresenceMedia::Text),
            ChatState::Typing
        );
        assert_eq!(
            protocol_chat_state(ChatPresence::Composing, ChatPresenceMedia::Audio),
            ChatState::Recording
        );
        assert_eq!(
            protocol_chat_state(ChatPresence::Paused, ChatPresenceMedia::Audio),
            ChatState::Paused
        );
    }

    #[test]
    fn chat_read_action_preserves_wire_boundary_and_named_messages() {
        let action = wa::sync_action_value::MarkChatAsReadAction {
            read: Some(true),
            message_range: MessageField::some(wa::sync_action_value::SyncActionMessageRange {
                last_message_timestamp: Some(1_700_000_000),
                messages: vec![
                    wa::sync_action_value::SyncActionMessage {
                        key: MessageField::some(wa::MessageKey {
                            id: Some("covered".into()),
                            ..Default::default()
                        }),
                        timestamp: Some(1_700_000_000),
                    },
                    wa::sync_action_value::SyncActionMessage::default(),
                ],
                ..Default::default()
            }),
        };

        assert_eq!(
            read_action_boundary(&action),
            (Some(1_700_000_000), vec!["covered".to_owned()])
        );
    }

    #[test]
    fn event_diagnostics_include_full_receipt_payload() {
        let event = Event::Receipt(
            Receipt::builder()
                .source(MessageSource {
                    chat: "120363000000000000@g.us".parse().unwrap(),
                    sender: "31600000000@s.whatsapp.net".parse().unwrap(),
                    ..Default::default()
                })
                .message_ids(vec![
                    "private-message-one".into(),
                    "private-message-two".into(),
                ])
                .timestamp(Utc::now())
                .r#type(ReceiptType::ReadSelf)
                .offline(true)
                .build(),
        );

        let diagnostic = event_diagnostic(&event);
        log_whatsapp_event(&event);
        for expected in [
            "Receipt",
            "120363000000000000",
            "31600000000",
            "private-message-one",
            "private-message-two",
            "ReadSelf",
            "\"offline\":true",
        ] {
            assert!(
                diagnostic.contains(expected),
                "missing {expected:?} from {diagnostic}"
            );
        }

        let encoded = whatsapp_rust::wacore_binary::marshal::marshal(
            &NodeBuilder::new("synthetic").attr("id", "node-1").build(),
        )
        .unwrap();
        let node = Arc::new(
            whatsapp_rust::wacore_binary::OwnedNodeRef::new(encoded[1..].to_vec()).unwrap(),
        );
        assert!(event_diagnostic(&Event::RawNode(Arc::clone(&node))).contains("RawNode"));
        assert!(event_diagnostic(&Event::Notification(node)).contains("Notification"));
    }
}
