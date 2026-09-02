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

// This lifetime loop only schedules already-instrumented durable reductions.
#[cfg_attr(coverage_nightly, coverage(off))]
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

// This lifetime loop only schedules the upstream event adapter.
#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

// Waiting for the replay is a scheduling shim around measured helpers: the
// quiet window, both settled outcomes, and the deadline are unit tested. Each
// completeness check opens the session database, so the waiter sleeps until
// the quiet period elapses instead of polling it.
#[cfg_attr(coverage_nightly, coverage(off))]
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

// Upstream event variants terminate in SDK queries, acknowledgements, and
// transport writes. Pure decoding, durable reduction, identity, and state
// transition helpers used by this adapter remain instrumented and unit tested.
#[cfg_attr(coverage_nightly, coverage(off))]
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
    use buffa::MessageField;
    use tokio::sync::{broadcast, oneshot};
    use whatsapp_rust::wacore::types::{
        events::Receipt, message::MessageSource, presence::ReceiptType,
    };
    use whatsapp_rust::wacore_binary::builder::NodeBuilder;

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
        use whatsapp_rust::wacore::types::presence::{ChatPresence, ChatPresenceMedia};

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
