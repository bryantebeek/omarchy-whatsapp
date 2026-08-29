#![recursion_limit = "512"]

mod assets;
mod database;
mod event_coverage;
mod live_location;
mod notification;
mod voice_outbox;

use anyhow::{Context, Result, anyhow, bail};
use buffa::Message as _;
use chrono::Utc;
use clap::Parser;
use database::Database;
use futures::StreamExt;
use omarchy_whatsapp_protocol::{
    AppPaths, Chat, ChatParticipant, ChatState, ClientFrame, Command, ConnectionStatus, Message,
    MessageDelivery, MessageMedia, MessageReader, PROTOCOL_VERSION, PollOption, ServerEvent,
    ServerFrame,
};
use qrcode::{QrCode, render::svg};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, LazyLock, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::{debug, error, info, warn};
use whatsapp_rust::prelude::*;
use whatsapp_rust::types::jid::JidExt as SignalJidExt;
use whatsapp_rust::wacore::download::MediaType;
use whatsapp_rust::wacore::request::InfoQuery;
use whatsapp_rust::wacore::store::DevicePropsOverride;
use whatsapp_rust::wacore_binary::{JidExt, NodeContent, SERVER_JID, builder::NodeBuilder};
use whatsapp_rust::{SendOptions, UploadOptions, media};

const CHAT_LIST_LIMIT: u32 = 500;
const AVATAR_SYNC_LIMIT: u32 = 1_000;
static AVATAR_FINGERPRINTS: LazyLock<
    StdMutex<HashMap<PathBuf, std::collections::BTreeMap<String, assets::AvatarFingerprint>>>,
> = LazyLock::new(|| StdMutex::new(HashMap::new()));

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

struct Shared {
    database: Database,
    status: RwLock<ConnectionStatus>,
    client: RwLock<Option<Arc<Client>>>,
    active_chat: RwLock<Option<String>>,
    presence_available: AtomicBool,
    events: broadcast::Sender<ServerFrame>,
    pairing_qr: PathBuf,
    contact_sync_marker: PathBuf,
    contact_history_marker: PathBuf,
    event_sync_marker: PathBuf,
    avatar_dir: PathBuf,
    media_dir: PathBuf,
    voice_outbox_dir: PathBuf,
    avatar_revision: AtomicU64,
    app_state_failed: AtomicBool,
    logout_requested: AtomicBool,
    avatar_sync: Mutex<()>,
    group_name_sync: Mutex<()>,
    media_recovery_requested: RwLock<HashSet<String>>,
    media_downloads: Mutex<HashSet<String>>,
    voice_outbox_gate: Mutex<()>,
}

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

async fn clear_local_account_data(paths: &AppPaths, shared: &Shared) -> Result<()> {
    let _voice_outbox_guard = shared.voice_outbox_gate.lock().await;
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
    *shared.active_chat.write().await = None;
    shared.presence_available.store(false, Ordering::SeqCst);
    shared.media_recovery_requested.write().await.clear();
    shared.media_downloads.lock().await.clear();
    broadcast_chats(shared);
    let _ = shared
        .events
        .send(ServerFrame::event(ServerEvent::Unread { total: 0 }));
    shared.avatars_changed();
    Ok(())
}

#[derive(Clone)]
enum PendingMedia {
    Image {
        image: wa::message::ImageMessage,
        chat_jid: String,
        message_id: String,
    },
    Sticker {
        sticker: wa::message::StickerMessage,
        chat_jid: String,
        message_id: String,
    },
    Video {
        video: wa::message::VideoMessage,
        chat_jid: String,
        message_id: String,
    },
    Audio {
        audio: wa::message::AudioMessage,
        chat_jid: String,
        message_id: String,
    },
    Document {
        document: wa::message::DocumentMessage,
        path: PathBuf,
    },
}

impl Shared {
    fn unread_total_or_zero(&self) -> u32 {
        match self.database.unread_total() {
            Ok(total) => total,
            Err(error) => {
                warn!(%error, "could not read WhatsApp unread total");
                0
            }
        }
    }

    async fn set_status(&self, status: ConnectionStatus) {
        self.write_pairing_qr(&status);
        *self.status.write().await = status.clone();
        let total = self.unread_total_or_zero();
        let _ = self.events.send(ServerFrame::event(ServerEvent::State {
            status,
            unread_total: total,
        }));
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

    fn mark_contact_sync_complete(&self) {
        if self.contact_sync_marker.exists() {
            return;
        }
        if let Err(error) = write_private_marker(&self.contact_sync_marker) {
            warn!(%error, "could not mark contact-name sync complete");
        }
    }

    fn mark_event_sync_complete(&self) {
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

    fn avatars_changed(&self) {
        let current = assets::avatar_fingerprints(&self.avatar_dir);
        let mut snapshots = AVATAR_FINGERPRINTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = snapshots.entry(self.avatar_dir.clone()).or_default();
        let mut changed_jids = current
            .keys()
            .chain(previous.keys())
            .filter(|jid| current.get(*jid) != previous.get(*jid))
            .cloned()
            .collect::<Vec<_>>();
        changed_jids.sort();
        changed_jids.dedup();
        *previous = current;
        drop(snapshots);
        if changed_jids.is_empty() {
            return;
        }
        let revision = self.avatar_revision.fetch_add(1, Ordering::Relaxed) + 1;
        let jids = assets::available_avatar_jids(&self.avatar_dir);
        let _ = self.events.send(ServerFrame::event(ServerEvent::Avatars {
            revision,
            jids,
            changed_jids,
        }));
    }

    fn avatar_snapshot(&self) -> Vec<String> {
        let current = assets::avatar_fingerprints(&self.avatar_dir);
        let jids = current.keys().cloned().collect();
        AVATAR_FINGERPRINTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(self.avatar_dir.clone())
            .or_insert(current);
        jids
    }

    async fn state_event(&self) -> ServerEvent {
        ServerEvent::State {
            status: self.status.read().await.clone(),
            unread_total: self.unread_total_or_zero(),
        }
    }

    async fn receive_message(self: &Arc<Self>, context: MessageContext) {
        let info = &context.info;
        if info.source.chat.is_status_broadcast() || info.source.chat.is_newsletter() {
            return;
        }
        let chat_jid = canonical_contact_jid(self, &context.client, &info.source.chat).await;
        let sender_jid = canonical_contact_jid(self, &context.client, &info.source.sender).await;
        let push_name = nonempty(&info.push_name).unwrap_or_else(|| sender_jid.clone());
        if push_name != sender_jid
            && let Err(error) = self.database.update_contact_name(&sender_jid, &push_name)
        {
            warn!(%error, %sender_jid, "could not persist message push name");
        }
        let sender_name = self
            .database
            .contact_name(&sender_jid)
            .ok()
            .flatten()
            .unwrap_or(push_name);
        let existing_name = self.database.chat_name(&chat_jid).ok().flatten();
        let chat_name = if info.source.is_group {
            match existing_name {
                Some(name) if name != chat_jid => name,
                _ => context
                    .client
                    .groups()
                    .get_metadata(&info.source.chat)
                    .await
                    .ok()
                    .and_then(|metadata| nonempty(&metadata.subject))
                    .unwrap_or_else(|| chat_jid.clone()),
            }
        } else {
            nonempty(&info.push_name)
                .or(existing_name)
                .unwrap_or_else(|| chat_jid.clone())
        };
        if let Some(reaction) = find_reaction_message(&context.message)
            && let Some(target) = reaction.key.as_option()
            && let Some(message_id) = target.id.as_deref()
        {
            let reactor_jid = if info.source.is_from_me {
                "me"
            } else {
                sender_jid.as_str()
            };
            let emoji = reaction.text.as_deref().unwrap_or_default();
            let timestamp = reaction
                .sender_timestamp_ms
                .unwrap_or_else(|| info.timestamp.timestamp_millis())
                .div_euclid(1_000);
            match self.database.apply_reaction(
                &chat_jid,
                message_id,
                reactor_jid,
                emoji,
                info.source.is_from_me,
                timestamp,
            ) {
                Ok(true) => broadcast_messages(self, &chat_jid),
                Ok(false) => {}
                Err(error) => warn!(%error, %chat_jid, %message_id, "could not persist reaction"),
            }
            return;
        }
        let base = context.message.get_base_message();
        if let Some(update) = base.poll_update_message.as_option() {
            let Some(target_id) = update
                .poll_creation_message_key
                .as_option()
                .and_then(|key| key.id.as_deref())
            else {
                warn!(message_id = %info.id, "poll vote is missing its parent message ID");
                return;
            };
            let stored = match self.database.poll_for_voting(&chat_jid, target_id) {
                Ok(Some(stored)) => stored,
                Ok(None) => {
                    warn!(%chat_jid, poll_message_id = %target_id,
                        "could not apply poll vote because the parent poll is unavailable");
                    return;
                }
                Err(error) => {
                    warn!(%error, %chat_jid, poll_message_id = %target_id,
                        "could not load parent poll for incoming vote");
                    return;
                }
            };
            let Some(vote) = update.vote.as_option() else {
                return;
            };
            let (Some(enc_payload), Some(enc_iv)) =
                (vote.enc_payload.as_deref(), vote.enc_iv.as_deref())
            else {
                warn!(%chat_jid, poll_message_id = %target_id,
                    "incoming poll vote has no encrypted payload");
                return;
            };
            let creator = match stored.creator_jid.parse::<Jid>() {
                Ok(creator) => creator,
                Err(error) => {
                    warn!(%error, creator_jid = %stored.creator_jid,
                        "stored poll creator JID is invalid");
                    return;
                }
            };
            let raw_voter = info.source.sender.to_non_ad();
            let hashes = match context
                .client
                .polls()
                .decrypt_vote(
                    whatsapp_rust::PollVoteCiphertext {
                        enc_payload,
                        enc_iv,
                    },
                    &stored.message_secret,
                    target_id,
                    &creator,
                    &raw_voter,
                )
                .await
            {
                Ok(hashes) => hashes,
                Err(error) => {
                    warn!(%error, %chat_jid, poll_message_id = %target_id,
                        "could not decrypt incoming poll vote");
                    return;
                }
            };
            let Some(selected_options) = option_names_for_hashes(&stored.options, &hashes) else {
                warn!(%chat_jid, poll_message_id = %target_id,
                    "incoming poll vote references an unknown option");
                return;
            };
            let poll_timestamp = update
                .sender_timestamp_ms
                .unwrap_or_else(|| info.timestamp.timestamp_millis());
            let voter_key = if info.source.is_from_me {
                "me"
            } else {
                sender_jid.as_str()
            };
            match self.database.apply_poll_vote(
                &chat_jid,
                target_id,
                voter_key,
                &selected_options,
                info.source.is_from_me,
                poll_timestamp,
            ) {
                Ok(true) => broadcast_messages(self, &chat_jid),
                Ok(false) => {}
                Err(error) => warn!(%error, %chat_jid, poll_message_id = %target_id,
                    "could not persist incoming poll vote"),
            }
            return;
        }
        if let Some(distribution) = base
            .fast_ratchet_key_sender_key_distribution_message
            .as_option()
            && let Some(payload) = distribution
                .axolotl_sender_key_distribution_message
                .as_deref()
        {
            match live_location::FastRatchetState::from_distribution(payload) {
                Ok(state) => {
                    let signal_address = info.source.sender.to_signal_address_string();
                    if let Err(error) = self
                        .database
                        .store_fast_ratchet_state(&signal_address, &state)
                    {
                        warn!(%error, "could not persist live-location sender key");
                    }
                }
                Err(error) => warn!(%error, "could not read live-location sender key"),
            }
        }
        let media = message_media(
            base,
            &self.media_dir,
            &chat_jid,
            &info.id,
            info.timestamp.timestamp(),
            0,
        );
        let Some(text) = base
            .text_content()
            .map(str::to_owned)
            .or_else(|| base.get_caption().map(str::to_owned))
            .or_else(|| media_text(base, &info.media_type))
        else {
            tracing::debug!(message_id = %info.id, media_type = %info.media_type,
                "ignored non-renderable WhatsApp control message");
            return;
        };
        let message = Message {
            id: info.id.clone(),
            chat_jid: chat_jid.clone(),
            sender_jid,
            sender_name,
            text,
            timestamp: info.timestamp.timestamp(),
            from_me: info.source.is_from_me,
            receipt: u8::from(info.source.is_from_me),
            delivered_at: None,
            read_at: None,
            delivered_to: Vec::new(),
            read_by: Vec::new(),
            media,
            reactions: Vec::new(),
        };
        let focused = self.active_chat.read().await.as_deref() == Some(chat_jid.as_str());
        let unread = !message.from_me && !focused;
        let insert_result =
            self.database
                .insert_message(&message, &chat_name, info.source.is_group, unread);
        if insert_result.is_ok()
            && matches!(message.media, Some(MessageMedia::Poll { .. }))
            && let Some(secret) = message_secret(&context.message, base)
            && let Err(error) = self.database.store_poll_secret(
                &chat_jid,
                &info.id,
                &info.source.sender.to_non_ad_string(),
                secret,
            )
        {
            warn!(%error, %chat_jid, message_id = %info.id,
                "could not persist poll message secret");
        }
        if let Some(image) = base.image_message.as_option()
            && let Err(error) =
                self.database
                    .store_media_download(&chat_jid, &info.id, &image.encode_to_vec())
        {
            warn!(%error, %chat_jid, message_id = %info.id,
                "could not persist WhatsApp image download metadata");
        }
        if let Some(video) = video_message(base)
            && let Err(error) =
                self.database
                    .store_media_download(&chat_jid, &info.id, &video.encode_to_vec())
        {
            warn!(%error, %chat_jid, message_id = %info.id,
                "could not persist WhatsApp video download metadata");
        }
        if let Some(audio) = base.audio_message.as_option()
            && let Err(error) =
                self.database
                    .store_media_download(&chat_jid, &info.id, &audio.encode_to_vec())
        {
            warn!(%error, %chat_jid, message_id = %info.id,
                "could not persist WhatsApp audio download metadata");
        }
        if let Some((sticker, lottie)) = sticker_message(base) {
            let mut sticker = sticker.clone();
            if lottie {
                sticker.is_lottie = Some(true);
            }
            if let Err(error) =
                self.database
                    .store_media_download(&chat_jid, &info.id, &sticker.encode_to_vec())
            {
                warn!(%error, %chat_jid, message_id = %info.id,
                    "could not persist WhatsApp sticker download metadata");
            }
        }
        if insert_result.is_ok()
            && info.source.is_group
            && chat_name != chat_jid
            && let Err(error) = self.database.update_group_name(&chat_jid, &chat_name)
        {
            warn!(%error, %chat_jid, "could not persist incoming group subject");
        }
        match insert_result {
            Ok(true) => {
                let _ = self.events.send(ServerFrame::event(ServerEvent::Message {
                    message: message.clone(),
                }));
                let total = self.unread_total_or_zero();
                let _ = self
                    .events
                    .send(ServerFrame::event(ServerEvent::Unread { total }));
                if unread
                    && !info.is_offline
                    && !self
                        .database
                        .is_muted(&chat_jid, Utc::now().timestamp())
                        .unwrap_or(false)
                {
                    notification::send(&message, &chat_name, info.source.is_group);
                }
                if let Some(document) = base.document_message.as_option().cloned() {
                    let shared = Arc::clone(self);
                    let client = Arc::clone(&context.client);
                    let path = assets::message_document_path(
                        &self.media_dir,
                        &chat_jid,
                        &info.id,
                        document.file_name.as_deref().unwrap_or_default(),
                    );
                    let media_chat_jid = chat_jid.clone();
                    tokio::spawn(async move {
                        match assets::download_message_document(client, document, path).await {
                            Ok(true) => broadcast_messages(&shared, &media_chat_jid),
                            Ok(false) => {}
                            Err(error) => {
                                warn!(%error, chat_jid = %media_chat_jid, "could not cache WhatsApp document");
                            }
                        }
                    });
                }
                if matches!(
                    message.media,
                    Some(MessageMedia::Location { live: true, .. })
                ) {
                    let client = Arc::clone(&context.client);
                    let target = info.source.chat.clone();
                    let is_group = info.source.is_group;
                    tokio::spawn(async move {
                        if let Err(error) = subscribe_live_location(&client, target, is_group).await
                        {
                            warn!(%error, "could not subscribe to incoming live location");
                        }
                    });
                }
            }
            Ok(false) => {
                if let Some(media) = &message.media
                    && self
                        .database
                        .update_message_media(&chat_jid, &message.id, media)
                        .unwrap_or(false)
                {
                    broadcast_messages(self, &chat_jid);
                }
            }
            Err(error) => error!(%error, "could not persist incoming message"),
        }
    }

    async fn receive_live_location_update(self: &Arc<Self>, context: MessageContext) -> Result<()> {
        let base = context.message.get_base_message();
        if base.live_location_message.is_unset()
            && !base
                .location_message
                .as_option()
                .is_some_and(|location| location.is_live.unwrap_or(false))
        {
            bail!("fast-ratchet payload does not contain a live location");
        }

        let sender_jid =
            canonical_contact_jid(self, &context.client, &context.info.source.sender).await;
        let targets = self
            .database
            .active_live_locations_for_sender(&sender_jid, Utc::now().timestamp())?;
        for target in targets {
            let Some(media) = message_media(
                base,
                &self.media_dir,
                &target.chat_jid,
                &target.message_id,
                context.info.timestamp.timestamp(),
                target.duration_seconds,
            ) else {
                continue;
            };
            if self
                .database
                .update_message_media(&target.chat_jid, &target.message_id, &media)?
            {
                broadcast_messages(self, &target.chat_jid);
            }
        }
        Ok(())
    }

    fn ingest_history(
        &self,
        lazy: &whatsapp_rust::types::events::LazyHistorySync,
        own_pn: Option<&str>,
    ) -> Result<Vec<PendingMedia>> {
        let mut pending_media = Vec::new();
        let mut pending_documents = 0;
        let mut stream = lazy.stream();
        while let Some(conversation) = stream.next_conversation()? {
            let chat_jid = normalize_jid(&conversation.id);
            if chat_jid.ends_with("@broadcast") || chat_jid.ends_with("@newsletter") {
                continue;
            }
            let is_group = chat_jid.ends_with("@g.us");
            let chat_name = conversation
                .name
                .as_deref()
                .or(conversation.display_name.as_deref())
                .and_then(nonempty)
                .or_else(|| self.database.contact_name(&chat_jid).ok().flatten())
                .unwrap_or_else(|| chat_jid.clone());
            let mut messages = conversation
                .messages
                .iter()
                .filter_map(|history| history.message.as_option())
                .filter_map(|wire| {
                    if let Some(reaction) = history_reaction(&chat_jid, wire) {
                        if let Err(error) = self.database.apply_reaction(
                            &chat_jid,
                            &reaction.message_id,
                            &reaction.reactor_jid,
                            &reaction.emoji,
                            reaction.from_me,
                            reaction.timestamp,
                        ) {
                            warn!(%error, chat = %chat_jid, "could not persist history reaction");
                        }
                        return None;
                    }
                    let result = history_message(&chat_jid, &chat_name, wire, &self.media_dir);
                    if let Some(base) = wire
                        .message
                        .as_option()
                        .map(whatsapp_rust::prelude::MessageExt::get_base_message)
                        && let Some(message) = &result
                    {
                        if let Some(image) = base.image_message.as_option().cloned() {
                            pending_media.push(PendingMedia::Image {
                                image,
                                chat_jid: chat_jid.clone(),
                                message_id: message.id.clone(),
                            });
                        } else if let Some((sticker, lottie)) = sticker_message(base) {
                            let mut sticker = sticker.clone();
                            if lottie {
                                sticker.is_lottie = Some(true);
                            }
                            pending_media.push(PendingMedia::Sticker {
                                sticker,
                                chat_jid: chat_jid.clone(),
                                message_id: message.id.clone(),
                            });
                        } else if let Some(video) = video_message(base).cloned() {
                            pending_media.push(PendingMedia::Video {
                                video,
                                chat_jid: chat_jid.clone(),
                                message_id: message.id.clone(),
                            });
                        } else if let Some(audio) = base.audio_message.as_option().cloned() {
                            pending_media.push(PendingMedia::Audio {
                                audio,
                                chat_jid: chat_jid.clone(),
                                message_id: message.id.clone(),
                            });
                        } else if pending_documents < 128
                            && let Some(document) = base.document_message.as_option().cloned()
                        {
                            let path = assets::message_document_path(
                                &self.media_dir,
                                &chat_jid,
                                &message.id,
                                document.file_name.as_deref().unwrap_or_default(),
                            );
                            pending_media.push(PendingMedia::Document { document, path });
                            pending_documents += 1;
                        }
                    }
                    result
                })
                .collect::<Vec<_>>();
            messages.sort_by_key(|message| message.timestamp);
            let last_timestamp = conversation
                .conversation_timestamp
                .or(conversation.last_msg_timestamp)
                .map(|timestamp| i64::try_from(timestamp).unwrap_or(i64::MAX))
                .or_else(|| messages.last().map(|message| message.timestamp))
                .unwrap_or(0);
            let preview = messages
                .last()
                .map(|message| message.text.clone())
                .unwrap_or_default();
            let last_sender_name = messages
                .last()
                .map(|message| message.sender_name.clone())
                .unwrap_or_default();
            self.database
                .insert_history_chat(&omarchy_whatsapp_protocol::Chat {
                    jid: chat_jid.clone(),
                    name: chat_name.clone(),
                    phone_number: None,
                    last_message: preview,
                    last_sender_name,
                    last_timestamp,
                    unread: conversation.unread_count.unwrap_or(0),
                    pinned: false,
                    muted: false,
                    is_group,
                })?;
            for message in messages {
                if !message.from_me && message.sender_name != message.sender_jid {
                    self.database
                        .update_contact_name(&message.sender_jid, &message.sender_name)?;
                }
                let inserted = self
                    .database
                    .insert_history_message(&message, &chat_name, is_group)?;
                if !inserted && let Some(media) = &message.media {
                    self.database
                        .update_message_media(&message.chat_jid, &message.id, media)?;
                }
            }
            for wire in conversation
                .messages
                .iter()
                .filter_map(|history| history.message.as_option())
            {
                let (Some(key), Some(outer)) = (wire.key.as_option(), wire.message.as_option())
                else {
                    continue;
                };
                let Some(message_id) = key.id.as_deref() else {
                    continue;
                };
                let base = outer.get_base_message();
                let Some(MessageMedia::Poll { options, .. }) = poll_media(base) else {
                    continue;
                };
                let option_names: Vec<String> =
                    options.into_iter().map(|option| option.name).collect();
                if let (Some(creator_jid), Some(secret)) = (
                    history_poll_creator(&chat_jid, wire, own_pn),
                    wire.message_secret
                        .as_deref()
                        .or_else(|| message_secret(outer, base)),
                ) && let Err(error) =
                    self.database
                        .store_poll_secret(&chat_jid, message_id, &creator_jid, secret)
                {
                    warn!(%error, %chat_jid, %message_id,
                        "could not persist history poll message secret");
                }
                for update in &wire.poll_updates {
                    let (Some(update_key), Some(vote)) = (
                        update.poll_update_message_key.as_option(),
                        update.vote.as_option(),
                    ) else {
                        continue;
                    };
                    let Some(selected_options) =
                        option_names_for_hashes(&option_names, &vote.selected_options)
                    else {
                        warn!(%chat_jid, %message_id,
                            "history poll vote references an unknown option");
                        continue;
                    };
                    let from_me = update_key.from_me.unwrap_or(false);
                    let voter_jid = if from_me {
                        "me".to_owned()
                    } else if is_group {
                        let Some(participant) = update_key.participant.as_deref() else {
                            warn!(%chat_jid, %message_id,
                                "history group poll vote is missing its participant");
                            continue;
                        };
                        normalize_jid(participant)
                    } else {
                        chat_jid.clone()
                    };
                    let timestamp = update
                        .sender_timestamp_ms
                        .or(update.server_timestamp_ms)
                        .unwrap_or_else(|| {
                            wire.message_timestamp
                                .and_then(|value| i64::try_from(value).ok())
                                .unwrap_or(0)
                                .saturating_mul(1_000)
                        });
                    if let Err(error) = self.database.apply_poll_vote(
                        &chat_jid,
                        message_id,
                        &voter_jid,
                        &selected_options,
                        from_me,
                        timestamp,
                    ) {
                        warn!(%error, %chat_jid, %message_id,
                            "could not persist history poll vote");
                    }
                }
            }
        }
        if stream.skipped_conversations() > 0 {
            warn!(
                skipped = stream.skipped_conversations(),
                "history sync contained undecodable conversations"
            );
        }
        let remainder = stream.remainder()?;
        for push_name in remainder.pushnames {
            if let (Some(jid), Some(name)) = (push_name.id, push_name.pushname) {
                self.database
                    .update_contact_name(&normalize_jid(&jid), &name)?;
            }
        }
        Ok(pending_media)
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}

fn find_reaction_message(message: &wa::Message) -> Option<&wa::message::ReactionMessage> {
    fn find(message: &wa::Message, depth: u8) -> Option<&wa::message::ReactionMessage> {
        if depth >= 16 {
            return None;
        }
        if let Some(reaction) = message.reaction_message.as_option() {
            return Some(reaction);
        }
        let protocol_edit = message
            .protocol_message
            .as_option()
            .and_then(|protocol| protocol.edited_message.as_option());
        let nested = [
            message
                .device_sent_message
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            message
                .ephemeral_message
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            message
                .view_once_message
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            message
                .view_once_message_v2
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            message
                .view_once_message_v2_extension
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            message
                .document_with_caption_message
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            message
                .edited_message
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            message
                .group_mentioned_message
                .as_option()
                .and_then(|wrapper| wrapper.message.as_option()),
            protocol_edit,
        ];
        nested
            .into_iter()
            .flatten()
            .find_map(|inner| find(inner, depth + 1))
    }
    find(message, 0)
}

fn media_placeholder(media_type: &str) -> Option<String> {
    Some(
        match media_type.to_ascii_lowercase().as_str() {
            "image" => "[Image]",
            "video" | "ptv" => "[Video]",
            "audio" | "ptt" => "[Voice message]",
            "document" => "[Document]",
            "sticker" => "[Sticker]",
            "contact" => "[Contact]",
            "location" | "live_location" => "[Location]",
            "poll" => "[Poll]",
            _ => return None,
        }
        .to_owned(),
    )
}

fn media_text(message: &wa::Message, fallback_type: &str) -> Option<String> {
    if let Some(location) = message.location_message.as_option() {
        return Some(
            nonempty(location.name.as_deref().unwrap_or_default())
                .or_else(|| nonempty(location.address.as_deref().unwrap_or_default()))
                .unwrap_or_else(|| {
                    if location.is_live.unwrap_or(false) {
                        "[Live location]".to_owned()
                    } else {
                        "[Location]".to_owned()
                    }
                }),
        );
    }
    if message.live_location_message.is_set() {
        return Some("[Live location]".to_owned());
    }
    let known = if message.image_message.is_set() {
        Some("[Image]")
    } else if let Some(video) = video_message(message) {
        Some(if video.gif_playback.unwrap_or(false) {
            "[GIF]"
        } else {
            "[Video]"
        })
    } else if message.audio_message.is_set() {
        Some("[Voice message]")
    } else if message.document_message.is_set() {
        Some("[Document]")
    } else if message.sticker_message.is_set() || message.lottie_sticker_message.is_set() {
        Some("[Sticker]")
    } else if message.contact_message.is_set() || message.contacts_array_message.is_set() {
        Some("[Contact]")
    } else if message.poll_creation_message.is_set()
        || message.poll_creation_message_v2.is_set()
        || message.poll_creation_message_v3.is_set()
        || message.poll_creation_message_v5.is_set()
        || message.poll_creation_message_v6.is_set()
    {
        return Some(
            poll_creation_message(message)
                .and_then(|poll| poll.name.as_deref())
                .and_then(nonempty)
                .map_or_else(|| "[Poll]".to_owned(), |name| format!("[Poll] {name}")),
        );
    } else if message.event_message.is_set() || message.event_invite_message.is_set() {
        Some("[Event]")
    } else if message.group_invite_message.is_set() {
        Some("[Group invite]")
    } else if message.product_message.is_set() {
        Some("[Product]")
    } else if message.call_log_messsage.is_set() {
        Some("[Call]")
    } else {
        None
    };
    if let Some(text) = known {
        return Some(text.to_owned());
    }
    media_placeholder(fallback_type)
}

fn video_message(message: &wa::Message) -> Option<&wa::message::VideoMessage> {
    message
        .video_message
        .as_option()
        .or_else(|| message.ptv_message.as_option())
}

fn sticker_message(message: &wa::Message) -> Option<(&wa::message::StickerMessage, bool)> {
    if let Some(sticker) = message.sticker_message.as_option() {
        return Some((sticker, sticker.is_lottie.unwrap_or(false)));
    }
    let inner = message
        .lottie_sticker_message
        .as_option()?
        .message
        .as_option()?;
    sticker_message(inner).map(|(sticker, _)| (sticker, true))
}

fn poll_creation_message(message: &wa::Message) -> Option<&wa::message::PollCreationMessage> {
    message
        .poll_creation_message_v6
        .as_option()
        .or_else(|| message.poll_creation_message_v5.as_option())
        .or_else(|| message.poll_creation_message_v3.as_option())
        .or_else(|| message.poll_creation_message_v2.as_option())
        .or_else(|| message.poll_creation_message.as_option())
}

fn poll_media(message: &wa::Message) -> Option<MessageMedia> {
    let poll = poll_creation_message(message)?;
    let options: Vec<PollOption> = poll
        .options
        .iter()
        .filter_map(|option| option.option_name.as_deref().and_then(nonempty))
        .map(|name| PollOption {
            name,
            votes: 0,
            selected_by_me: false,
        })
        .collect();
    if options.len() < 2 {
        return None;
    }
    let correct_option_index = poll.correct_answer.as_option().and_then(|correct| {
        let name = correct.option_name.as_deref()?;
        options
            .iter()
            .position(|option| option.name == name)
            .and_then(|index| u32::try_from(index).ok())
    });
    let mut end_timestamp = poll.end_time.unwrap_or(0);
    if end_timestamp > 10_000_000_000 {
        end_timestamp = end_timestamp.div_euclid(1_000);
    }
    Some(MessageMedia::Poll {
        question: poll
            .name
            .as_deref()
            .and_then(nonempty)
            .unwrap_or_else(|| "Poll".to_owned()),
        selectable_count: poll
            .selectable_options_count
            .unwrap_or(1)
            .clamp(1, u32::try_from(options.len()).unwrap_or(u32::MAX)),
        options,
        total_voters: 0,
        quiz: poll.poll_type == Some(wa::message::PollType::QUIZ),
        correct_option_index,
        end_timestamp,
    })
}

fn message_secret<'a>(outer: &'a wa::Message, base: &'a wa::Message) -> Option<&'a [u8]> {
    base.message_context_info
        .as_option()
        .and_then(|context| context.message_secret.as_deref())
        .or_else(|| {
            outer
                .message_context_info
                .as_option()
                .and_then(|context| context.message_secret.as_deref())
        })
}

#[allow(clippy::cast_possible_truncation)]
fn coordinate_e7(value: Option<f64>) -> i64 {
    let value = value.unwrap_or_default();
    if value.is_finite() {
        (value.clamp(-180.0, 180.0) * 10_000_000.0).round() as i64
    } else {
        0
    }
}

fn cache_location_thumbnail(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    bytes: Option<&Vec<u8>>,
) -> Option<String> {
    let bytes = bytes.filter(|bytes| bytes.starts_with(&[0xff, 0xd8, 0xff]))?;
    let path = assets::location_thumbnail_path(directory, chat_jid, message_id);
    let unchanged = std::fs::read(&path).is_ok_and(|existing| existing == **bytes);
    if !unchanged && let Err(error) = assets::write_private_bytes(&path, bytes) {
        warn!(%error, "could not cache WhatsApp location thumbnail");
        return None;
    }
    assets::prune_media_cache(directory, &path);
    Some(path.to_string_lossy().into_owned())
}

fn message_media(
    message: &wa::Message,
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    timestamp: i64,
    live_duration_seconds: u32,
) -> Option<MessageMedia> {
    if let Some(poll) = poll_media(message) {
        return Some(poll);
    }
    if let Some(image) = message.image_message.as_option() {
        let path = assets::message_image_path(directory, chat_jid, message_id);
        let thumbnail_path = assets::cache_message_image_thumbnail(
            directory,
            chat_jid,
            message_id,
            image.jpeg_thumbnail.as_ref(),
            image.file_length,
        )
        .unwrap_or_else(|error| {
            warn!(%error, "could not cache WhatsApp image thumbnail");
            assets::message_image_thumbnail_path(directory, chat_jid, message_id)
        });
        assets::prune_media_cache(
            directory,
            if path.exists() {
                &path
            } else {
                &thumbnail_path
            },
        );
        return Some(MessageMedia::Image {
            path: path.to_string_lossy().into_owned(),
            thumbnail_path: thumbnail_path.to_string_lossy().into_owned(),
            downloaded: path.exists(),
            mime_type: image
                .mimetype
                .clone()
                .unwrap_or_else(|| "image/jpeg".to_owned()),
            width: image.width.unwrap_or(0),
            height: image.height.unwrap_or(0),
        });
    }
    if let Some((sticker, lottie)) = sticker_message(message) {
        let path = assets::message_sticker_path(directory, chat_jid, message_id);
        let thumbnail_path = assets::cache_message_sticker_thumbnail(
            directory,
            chat_jid,
            message_id,
            sticker.png_thumbnail.as_ref(),
        )
        .unwrap_or_else(|error| {
            warn!(%error, "could not cache WhatsApp sticker thumbnail");
            assets::message_sticker_thumbnail_path(directory, chat_jid, message_id)
        });
        assets::prune_media_cache(
            directory,
            if path.exists() {
                &path
            } else {
                &thumbnail_path
            },
        );
        return Some(MessageMedia::Sticker {
            path: path.to_string_lossy().into_owned(),
            thumbnail_path: thumbnail_path.to_string_lossy().into_owned(),
            downloaded: !lottie && path.exists(),
            mime_type: sticker.mimetype.clone().unwrap_or_else(|| {
                if lottie {
                    "application/json".to_owned()
                } else {
                    "image/webp".to_owned()
                }
            }),
            width: sticker.width.unwrap_or(0),
            height: sticker.height.unwrap_or(0),
            animated: sticker.is_animated.unwrap_or(false) || lottie,
            lottie,
            accessibility_label: sticker.accessibility_label.clone().unwrap_or_default(),
        });
    }
    if let Some(video) = video_message(message) {
        let path =
            assets::message_video_path(directory, chat_jid, message_id, video.mimetype.as_deref());
        let thumbnail_path = assets::cache_message_video_thumbnail(
            directory,
            chat_jid,
            message_id,
            video.mimetype.as_deref(),
            video.jpeg_thumbnail.as_ref(),
            video.file_length,
        )
        .unwrap_or_else(|error| {
            warn!(%error, "could not cache WhatsApp video thumbnail");
            assets::message_video_thumbnail_path(directory, chat_jid, message_id)
        });
        assets::prune_media_cache(
            directory,
            if path.exists() {
                &path
            } else {
                &thumbnail_path
            },
        );
        return Some(MessageMedia::Video {
            path: path.to_string_lossy().into_owned(),
            thumbnail_path: thumbnail_path.to_string_lossy().into_owned(),
            downloaded: path.exists(),
            mime_type: video
                .mimetype
                .clone()
                .unwrap_or_else(|| "video/mp4".to_owned()),
            width: video.width.unwrap_or(0),
            height: video.height.unwrap_or(0),
            duration_seconds: video.seconds.unwrap_or(0),
            gif_playback: video.gif_playback.unwrap_or(false),
        });
    }
    if let Some(audio) = message.audio_message.as_option() {
        let path =
            assets::message_audio_path(directory, chat_jid, message_id, audio.mimetype.as_deref());
        assets::prune_media_cache(directory, &path);
        return Some(MessageMedia::Audio {
            path: path.to_string_lossy().into_owned(),
            downloaded: path.exists(),
            mime_type: audio
                .mimetype
                .clone()
                .unwrap_or_else(|| "audio/ogg; codecs=opus".to_owned()),
            duration_seconds: audio.seconds.unwrap_or(0),
            voice_message: audio.ptt.unwrap_or(false),
        });
    }
    if let Some(document) = message.document_message.as_option() {
        let file_name = document
            .file_name
            .as_deref()
            .and_then(nonempty)
            .or_else(|| document.title.as_deref().and_then(nonempty))
            .unwrap_or_else(|| "Document".to_owned());
        let path = assets::message_document_path(directory, chat_jid, message_id, &file_name);
        return Some(MessageMedia::Document {
            path: path.to_string_lossy().into_owned(),
            file_name,
            mime_type: document
                .mimetype
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
            file_size: document.file_length.unwrap_or(0),
            page_count: document.page_count.unwrap_or(0),
        });
    }
    if let Some(location) = message.location_message.as_option() {
        return Some(MessageMedia::Location {
            latitude_e7: coordinate_e7(location.degrees_latitude),
            longitude_e7: coordinate_e7(location.degrees_longitude),
            accuracy_m: location.accuracy_in_meters.unwrap_or(0),
            name: location.name.clone().unwrap_or_default(),
            address: location.address.clone().unwrap_or_default(),
            thumbnail_path: cache_location_thumbnail(
                directory,
                chat_jid,
                message_id,
                location.jpeg_thumbnail.as_ref(),
            ),
            live: location.is_live.unwrap_or(false),
            updated_at: timestamp,
            duration_seconds: live_duration_seconds,
        });
    }
    if let Some(location) = message.live_location_message.as_option() {
        return Some(MessageMedia::Location {
            latitude_e7: coordinate_e7(location.degrees_latitude),
            longitude_e7: coordinate_e7(location.degrees_longitude),
            accuracy_m: location.accuracy_in_meters.unwrap_or(0),
            name: location.caption.clone().unwrap_or_default(),
            address: String::new(),
            thumbnail_path: cache_location_thumbnail(
                directory,
                chat_jid,
                message_id,
                location.jpeg_thumbnail.as_ref(),
            ),
            live: true,
            updated_at: timestamp,
            duration_seconds: live_duration_seconds,
        });
    }
    None
}

fn normalize_jid(value: &str) -> String {
    value
        .parse::<Jid>()
        .map_or_else(|_| value.to_owned(), |jid| jid.to_non_ad_string())
}

async fn canonical_contact_jid(shared: &Shared, client: &Client, jid: &Jid) -> String {
    let raw = jid.to_non_ad_string();
    if !jid.is_lid() && !jid.is_pn() {
        return raw;
    }
    let mapping = match client.get_lid_pn_entry(jid).await {
        Ok(Some(mapping)) => mapping,
        Ok(None) => {
            if jid.is_pn()
                && let Err(error) = shared.database.update_chat_phone_number(&raw, &jid.user)
            {
                warn!(%error, %raw, "could not persist WhatsApp phone number");
            }
            return raw;
        }
        Err(error) => {
            warn!(%error, %raw, "could not resolve WhatsApp contact alias");
            return raw;
        }
    };
    let canonical = format!("{}@s.whatsapp.net", mapping.phone_number);
    let alias = format!("{}@lid", mapping.lid);
    if alias != canonical {
        match shared.database.migrate_contact_jid(&alias, &canonical) {
            Ok(true) => {
                let replacements = match assets::copy_chat_media_alias(
                    &shared.media_dir,
                    &alias,
                    &canonical,
                ) {
                    Ok(replacements) => replacements,
                    Err(error) => {
                        warn!(%error, %alias, %canonical, "could not preserve aliased WhatsApp media paths");
                        Vec::new()
                    }
                };
                if let Err(error) = shared.database.rewrite_media_paths(&replacements) {
                    warn!(%error, %alias, %canonical, "could not rewrite aliased WhatsApp media paths");
                }
                match assets::copy_avatar_alias(&shared.avatar_dir, &alias, &canonical) {
                    Ok(true) => shared.avatars_changed(),
                    Ok(false) => {}
                    Err(error) => {
                        warn!(%error, %alias, %canonical, "could not preserve aliased WhatsApp avatar");
                    }
                }
            }
            Ok(false) => {}
            Err(error) => {
                warn!(%error, %alias, %canonical, "could not merge WhatsApp contact alias");
                return raw;
            }
        }
    }
    if let Err(error) = shared
        .database
        .update_chat_phone_number(&canonical, &mapping.phone_number)
    {
        warn!(%error, %canonical, "could not cache canonical WhatsApp phone number");
    }
    canonical
}

async fn own_poll_creator_jid(client: &Client, chat: &Jid) -> Result<Jid> {
    if chat.is_group()
        && client
            .groups()
            .get_metadata(chat)
            .await
            .is_ok_and(|metadata| {
                metadata.addressing_mode
                    == whatsapp_rust::wacore::types::message::AddressingMode::Lid
            })
    {
        return client
            .lid()
            .map(|jid| jid.to_non_ad())
            .ok_or_else(|| anyhow!("own LID is unavailable for this group poll"));
    }
    client
        .pn()
        .map(|jid| jid.to_non_ad())
        .ok_or_else(|| anyhow!("own WhatsApp JID is unavailable"))
}

async fn reconcile_direct_chat_aliases(shared: &Shared, client: &Client) {
    let jids = match shared.database.direct_chat_jids(CHAT_LIST_LIMIT) {
        Ok(jids) => jids,
        Err(error) => {
            warn!(%error, "could not enumerate WhatsApp contact aliases");
            return;
        }
    };
    for raw in jids {
        if let Ok(jid) = raw.parse::<Jid>() {
            canonical_contact_jid(shared, client, &jid).await;
        }
    }
}

async fn list_chats_with_phone_numbers(shared: &Shared, limit: u32) -> Result<Vec<Chat>> {
    let client = shared.client.read().await.clone();
    if let Some(client) = client.as_ref() {
        reconcile_direct_chat_aliases(shared, client).await;
    }
    let mut chats = shared.database.list_chats(limit)?;
    for chat in &mut chats {
        if chat.is_group || chat.phone_number.is_some() {
            continue;
        }
        let Ok(jid) = chat.jid.parse::<Jid>() else {
            continue;
        };
        let phone_number = if jid.is_pn() {
            Some(jid.user.to_string())
        } else if jid.is_lid() {
            let Some(client) = client.as_ref() else {
                continue;
            };
            match client.get_lid_pn_entry(&jid).await {
                Ok(Some(mapping)) => Some(mapping.phone_number.to_string()),
                Ok(None) => None,
                Err(error) => {
                    warn!(%error, "could not resolve WhatsApp contact phone number");
                    None
                }
            }
        } else {
            None
        };
        if let Some(phone_number) = phone_number.filter(|value| !value.is_empty()) {
            if let Err(error) = shared
                .database
                .update_chat_phone_number(&chat.jid, &phone_number)
            {
                warn!(%error, "could not cache WhatsApp contact phone number");
            }
            chat.phone_number = Some(phone_number);
        }
    }
    Ok(chats)
}

async fn resolve_group_participants(
    shared: &Shared,
    client: &Client,
    participant_jids: Vec<Jid>,
) -> Vec<ChatParticipant> {
    let own_pn = client.pn().map(|jid| jid.to_non_ad_string());
    let own_lid = client.lid().map(|jid| jid.to_non_ad_string());
    let mut seen = HashSet::new();
    let mut participants = Vec::with_capacity(participant_jids.len());

    for participant_jid in participant_jids {
        let raw = participant_jid.to_non_ad_string();
        let is_me =
            own_pn.as_deref() == Some(raw.as_str()) || own_lid.as_deref() == Some(raw.as_str());
        let jid = canonical_contact_jid(shared, client, &participant_jid).await;
        if !seen.insert(jid.clone()) {
            continue;
        }
        let name = if is_me {
            String::new()
        } else {
            shared
                .database
                .contact_name(&jid)
                .ok()
                .flatten()
                .or_else(|| shared.database.chat_name(&jid).ok().flatten())
                .filter(|name| name != &jid)
                .unwrap_or_default()
        };
        participants.push(ChatParticipant { jid, name, is_me });
    }

    participants.sort_by_key(|participant| {
        (
            !participant.is_me,
            participant.name.to_lowercase(),
            participant.jid.clone(),
        )
    });
    participants
}

fn metadata_jid(value: &str, server: &str) -> String {
    if value.contains('@') {
        normalize_jid(value)
    } else {
        format!("{value}@{server}")
    }
}

fn broadcast_chats(shared: &Shared) {
    match shared.database.list_chats(CHAT_LIST_LIMIT) {
        Ok(chats) => {
            let _ = shared
                .events
                .send(ServerFrame::event(ServerEvent::Chats { chats }));
        }
        Err(error) => warn!(%error, "could not publish WhatsApp chat state"),
    }
}

fn broadcast_unread(shared: &Shared) {
    match shared.database.unread_total() {
        Ok(total) => {
            let _ = shared
                .events
                .send(ServerFrame::event(ServerEvent::Unread { total }));
        }
        Err(error) => warn!(%error, "could not publish WhatsApp unread state"),
    }
}

fn broadcast_messages(shared: &Shared, chat_jid: &str) {
    match (
        shared.database.messages(chat_jid, 300),
        shared.database.first_unread_message_id(chat_jid),
    ) {
        (Ok(messages), Ok(first_unread_message_id)) => {
            let _ = shared
                .events
                .send(ServerFrame::event(ServerEvent::Messages {
                    chat_jid: chat_jid.to_owned(),
                    messages,
                    first_unread_message_id,
                }));
        }
        (Err(error), _) | (_, Err(error)) => {
            warn!(%error, %chat_jid, "could not publish WhatsApp message state");
        }
    }
}

fn voice_outbox_event(shared: &Shared) -> Result<ServerEvent> {
    Ok(ServerEvent::VoiceOutbox {
        entries: voice_outbox::entries(&shared.voice_outbox_dir)?,
    })
}

fn broadcast_voice_outbox(shared: &Shared) {
    match voice_outbox_event(shared) {
        Ok(event) => {
            let _ = shared.events.send(ServerFrame::event(event));
        }
        Err(error) => warn!(%error, "could not publish voice outbox state"),
    }
}

fn broadcast_snapshot(shared: &Shared) {
    broadcast_chats(shared);
    broadcast_unread(shared);
}

fn ingest_contact_name(shared: &Shared, update: &whatsapp_rust::types::events::ContactUpdate) {
    let action = &update.action;
    let Some(name) = action
        .full_name
        .as_deref()
        .and_then(nonempty)
        .or_else(|| action.first_name.as_deref().and_then(nonempty))
        .or_else(|| action.username.as_deref().and_then(nonempty))
    else {
        if update.from_full_sync {
            shared.mark_contact_sync_complete();
        }
        return;
    };

    let mut jids = HashSet::new();
    jids.insert(update.jid.to_non_ad_string());
    if let Some(pn) = action.pn_jid.as_deref().and_then(nonempty) {
        jids.insert(metadata_jid(&pn, "s.whatsapp.net"));
    }
    if let Some(lid) = action.lid_jid.as_deref().and_then(nonempty) {
        jids.insert(metadata_jid(&lid, "lid"));
    }

    let mut changed = false;
    for jid in jids {
        match shared.database.update_address_book_name(&jid, &name) {
            Ok(updated) => changed |= updated,
            Err(error) => warn!(%error, %jid, "could not persist WhatsApp contact name"),
        }
    }
    if update.from_full_sync {
        shared.mark_contact_sync_complete();
    }
    if changed {
        broadcast_chats(shared);
    }
}

async fn sync_group_names(shared: Arc<Shared>, client: Arc<Client>) {
    let _guard = shared.group_name_sync.lock().await;
    match client.groups().get_participating().await {
        Ok(groups) => {
            let mut updated = 0usize;
            for (jid, metadata) in groups {
                match shared
                    .database
                    .update_group_name(&jid.to_non_ad_string(), &metadata.subject)
                {
                    Ok(true) => updated += 1,
                    Ok(false) => {}
                    Err(error) => warn!(%error, %jid, "could not persist WhatsApp group subject"),
                }
            }

            // Participating metadata covers current groups in one request. Old
            // conversations can be absent from that response, so make a small
            // best-effort pass over the few unresolved subjects as well.
            let unresolved = match shared.database.unresolved_chat_jids(true, 32) {
                Ok(unresolved) => unresolved,
                Err(error) => {
                    warn!(%error, "could not select unresolved WhatsApp group subjects");
                    return;
                }
            };
            for raw_jid in unresolved {
                let Ok(jid) = raw_jid.parse::<Jid>() else {
                    continue;
                };
                match client.groups().get_metadata(&jid).await {
                    Ok(metadata) => {
                        if nonempty(&metadata.subject).is_none() {
                            warn!(%jid, "WhatsApp returned an empty group subject");
                        } else {
                            match shared
                                .database
                                .update_group_name(&raw_jid, &metadata.subject)
                            {
                                Ok(true) => updated += 1,
                                Ok(false) => {}
                                Err(error) => {
                                    warn!(%error, %jid, "could not persist recovered WhatsApp group subject");
                                }
                            }
                        }
                    }
                    Err(error) => {
                        warn!(%error, %jid, "could not recover WhatsApp group subject");
                    }
                }
            }
            if updated > 0 {
                broadcast_chats(&shared);
            }
            info!(updated, "synchronized WhatsApp group subjects");
        }
        Err(error) => warn!(%error, "could not synchronize WhatsApp group subjects"),
    }
}

async fn sync_missing_contact_names(shared: Arc<Shared>, client: Arc<Client>) {
    let unresolved = match shared.database.unresolved_chat_jids(false, 100) {
        Ok(unresolved) => unresolved,
        Err(error) => {
            warn!(%error, "could not select unresolved WhatsApp contact names");
            return;
        }
    };
    let jids = unresolved
        .into_iter()
        .filter_map(|raw| raw.parse::<Jid>().ok())
        .filter(|jid| (jid.is_pn() || jid.is_lid()) && jid.user.as_str() != "0")
        .collect::<Vec<_>>();
    if jids.is_empty() {
        return;
    }

    match client.contacts().get_user_info(&jids).await {
        Ok(infos) => {
            let mut updated = 0usize;
            for info in infos.into_values() {
                let Some(name) = info
                    .verified_name
                    .as_ref()
                    .and_then(|verified| verified.name.as_deref())
                    .and_then(nonempty)
                else {
                    continue;
                };
                let mut candidates = vec![info.jid.to_non_ad_string()];
                if let Some(lid) = info.lid {
                    candidates.push(lid.to_non_ad_string());
                }
                for jid in candidates {
                    match shared.database.update_contact_name(&jid, &name) {
                        Ok(true) => updated += 1,
                        Ok(false) => {}
                        Err(error) => {
                            warn!(%error, %jid, "could not persist WhatsApp business profile name");
                        }
                    }
                }
            }
            if updated > 0 {
                broadcast_chats(&shared);
            }
            info!(updated, "synchronized WhatsApp business profile names");
        }
        Err(error) => warn!(%error, "could not synchronize WhatsApp business profile names"),
    }
}

async fn refresh_avatar(shared: Arc<Shared>, client: Arc<Client>, jid: Jid, force: bool) {
    let raw_jid = jid.to_non_ad_string();
    if force {
        assets::remove_avatar(&shared.avatar_dir, &raw_jid);
    } else if assets::avatar_path(&shared.avatar_dir, &raw_jid).exists()
        || assets::avatar_missing_path(&shared.avatar_dir, &raw_jid).exists()
    {
        return;
    }
    match assets::fetch_avatar(client, shared.avatar_dir.clone(), jid).await {
        Ok(changed) => {
            if changed || force {
                shared.avatars_changed();
            }
        }
        Err(error) => warn!(%error, %raw_jid, "could not refresh WhatsApp avatar"),
    }
}

async fn sync_avatars(shared: Arc<Shared>, client: Arc<Client>) {
    // Connected can precede the initial history import on a fresh link. Keep
    // sync passes serialized so a pass queued by each history chunk observes
    // the chats imported by the preceding pass without fetching duplicates.
    let _sync_guard = shared.avatar_sync.lock().await;
    let avatar_jids = match shared.database.avatar_jids(AVATAR_SYNC_LIMIT) {
        Ok(jids) => jids,
        Err(error) => {
            warn!(%error, "could not select WhatsApp avatars to synchronize");
            return;
        }
    };
    let jids = avatar_jids
        .into_iter()
        .filter_map(|raw| raw.parse::<Jid>().ok())
        .filter(|jid| !jid.is_status_broadcast() && !jid.is_newsletter())
        .filter(|jid| {
            let raw = jid.to_non_ad_string();
            !assets::avatar_path(&shared.avatar_dir, &raw).exists()
                && !assets::avatar_missing_path(&shared.avatar_dir, &raw).exists()
        })
        .collect::<Vec<_>>();
    let total = jids.len();
    futures::stream::iter(jids.into_iter().map(|jid| {
        let shared = Arc::clone(&shared);
        let client = Arc::clone(&client);
        async move { refresh_avatar(shared, client, jid, false).await }
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    info!(total, "completed bounded WhatsApp avatar sync");
}

async fn download_pending_media(
    shared: Arc<Shared>,
    client: Arc<Client>,
    media: Vec<PendingMedia>,
) {
    let downloaded = futures::stream::iter(media.into_iter().map(|pending| {
        let client = Arc::clone(&client);
        let shared = Arc::clone(&shared);
        async move {
            let result = match pending {
                PendingMedia::Image {
                    image,
                    chat_jid,
                    message_id,
                } => shared.database.store_media_download(
                    &chat_jid,
                    &message_id,
                    &image.encode_to_vec(),
                ),
                PendingMedia::Sticker {
                    sticker,
                    chat_jid,
                    message_id,
                } => shared.database.store_media_download(
                    &chat_jid,
                    &message_id,
                    &sticker.encode_to_vec(),
                ),
                PendingMedia::Video {
                    video,
                    chat_jid,
                    message_id,
                } => shared.database.store_media_download(
                    &chat_jid,
                    &message_id,
                    &video.encode_to_vec(),
                ),
                PendingMedia::Audio {
                    audio,
                    chat_jid,
                    message_id,
                } => shared.database.store_media_download(
                    &chat_jid,
                    &message_id,
                    &audio.encode_to_vec(),
                ),
                PendingMedia::Document { document, path } => {
                    assets::download_message_document(client, document, path).await
                }
            };
            match result {
                Ok(changed) => changed,
                Err(error) => {
                    warn!(%error, "could not cache WhatsApp history media");
                    false
                }
            }
        }
    }))
    .buffer_unordered(3)
    .filter(|changed| std::future::ready(*changed))
    .count()
    .await;
    if downloaded > 0
        && let Some(chat_jid) = shared.active_chat.read().await.clone()
    {
        broadcast_messages(&shared, &chat_jid);
    }
    info!(downloaded, "cached WhatsApp history media");
}

async fn backfill_video_previews(shared: Arc<Shared>) {
    let media_dir = shared.media_dir.clone();
    match tokio::task::spawn_blocking(move || assets::backfill_message_video_thumbnails(&media_dir))
        .await
    {
        Ok(Ok(generated)) => {
            info!(generated, "generated missing video previews");
            if generated > 0
                && let Some(chat_jid) = shared.active_chat.read().await.clone()
            {
                broadcast_messages(&shared, &chat_jid);
            }
        }
        Ok(Err(error)) => warn!(%error, "could not scan cached videos for missing previews"),
        Err(error) => warn!(%error, "video preview worker panicked"),
    }
}

async fn request_missing_contact_history(shared: Arc<Shared>, client: Arc<Client>) {
    if shared.contact_history_marker.exists() {
        return;
    }

    let cursors = match shared.database.unresolved_contact_history_cursors(24) {
        Ok(cursors) => cursors,
        Err(error) => {
            warn!(%error, "could not select unresolved chats for history recovery");
            return;
        }
    };
    let requested = cursors.len();
    if requested == 0 {
        // On a fresh link, Connected can arrive before the first history chunk.
        // Do not consume the one-time recovery until at least one real chat is
        // present; the HistorySync handler will retry after importing it.
        match shared.database.list_chats(1) {
            Ok(chats) if !chats.is_empty() => {
                if let Err(error) = write_private_marker(&shared.contact_history_marker) {
                    warn!(%error, "could not mark contact history recovery complete");
                }
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "could not inspect chats after contact history recovery");
            }
        }
        return;
    }
    let mut queued = 0usize;
    for cursor in cursors {
        let Ok(jid) = cursor.chat_jid.parse::<Jid>() else {
            warn!(jid = %cursor.chat_jid, "could not parse chat for history recovery");
            continue;
        };
        match client
            .fetch_message_history(
                &jid,
                &cursor.message_id,
                cursor.from_me,
                cursor.timestamp_ms,
                25,
            )
            .await
        {
            Ok(_) => queued += 1,
            Err(error) => {
                warn!(%error, jid = %cursor.chat_jid, "could not request WhatsApp history for contact-name recovery");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    if queued == requested
        && let Err(error) = write_private_marker(&shared.contact_history_marker)
    {
        warn!(%error, "could not mark contact history recovery complete");
    }
    info!(
        requested,
        queued, "requested one-time WhatsApp history for missing contact names"
    );
}

fn write_private_marker(path: &Path) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, b"1\n")?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(temporary, path)
}

fn prepare_contact_name_resync(protocol_db: &Path, marker: &Path) -> Result<()> {
    if marker.exists() || !protocol_db.exists() {
        return Ok(());
    }

    let mut connection = rusqlite::Connection::open(protocol_db)
        .with_context(|| format!("opening session database at {}", protocol_db.display()))?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM app_state_mutation_macs
         WHERE name IN ('critical_block', 'critical_unblock_low')",
        [],
    )?;
    transaction.execute(
        "DELETE FROM app_state_versions
         WHERE name IN ('critical_block', 'critical_unblock_low')",
        [],
    )?;
    // whatsapp-rust enters its critical bootstrap when the persisted push name
    // is empty. critical_block restores that name while critical_unblock_low
    // replays the address-book ContactUpdate events we need.
    transaction.execute("UPDATE device SET push_name = ''", [])?;
    transaction.commit()?;
    info!("scheduled one-time WhatsApp contact-name resync");
    Ok(())
}

fn prepare_event_state_resync(protocol_db: &Path, marker: &Path) -> Result<()> {
    if marker.exists() || !protocol_db.exists() {
        return Ok(());
    }
    let mut connection = rusqlite::Connection::open(protocol_db)
        .with_context(|| format!("opening session database at {}", protocol_db.display()))?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM app_state_mutation_macs
         WHERE name IN ('critical_block', 'regular', 'regular_low', 'regular_high')",
        [],
    )?;
    transaction.execute(
        "DELETE FROM app_state_versions
         WHERE name IN ('critical_block', 'regular', 'regular_low', 'regular_high')",
        [],
    )?;
    // A linked device only schedules all non-critical collections during its
    // bootstrap path. Clearing their versions is not sufficient on an already
    // paired session; an empty push name safely re-enters that path. Reset
    // critical_block too so its setting_pushName mutation restores the name
    // before the regular collections are fetched.
    transaction.execute("UPDATE device SET push_name = ''", [])?;
    transaction.commit()?;
    info!("scheduled one-time WhatsApp chat-state event resync");
    Ok(())
}

struct HistoryReaction {
    message_id: String,
    reactor_jid: String,
    emoji: String,
    from_me: bool,
    timestamp: i64,
}

fn history_reaction(chat_jid: &str, wire: &wa::WebMessageInfo) -> Option<HistoryReaction> {
    let envelope_key = wire.key.as_option()?;
    let reaction = find_reaction_message(wire.message.as_option()?)?;
    let target = reaction.key.as_option()?;
    let from_me = envelope_key.from_me.unwrap_or(false);
    let reactor_jid = if from_me {
        "me".to_owned()
    } else {
        normalize_jid(
            wire.participant
                .as_deref()
                .or(envelope_key.participant.as_deref())
                .unwrap_or(chat_jid),
        )
    };
    let timestamp = reaction
        .sender_timestamp_ms
        .map(|timestamp| timestamp.div_euclid(1_000))
        .or_else(|| {
            wire.message_timestamp
                .map(|timestamp| i64::try_from(timestamp).unwrap_or(i64::MAX))
        })
        .unwrap_or(0);
    Some(HistoryReaction {
        message_id: target.id.clone()?,
        reactor_jid,
        emoji: reaction.text.clone().unwrap_or_default(),
        from_me,
        timestamp,
    })
}

fn option_names_for_hashes(options: &[String], hashes: &[Vec<u8>]) -> Option<Vec<String>> {
    let selected: Vec<String> = options
        .iter()
        .filter(|option| {
            let hash = whatsapp_rust::wacore::poll::compute_option_hash(option);
            hashes.iter().any(|selected| selected.as_slice() == hash)
        })
        .cloned()
        .collect();
    (selected.len() == hashes.len()).then_some(selected)
}

fn history_poll_creator(
    chat_jid: &str,
    wire: &wa::WebMessageInfo,
    own_pn: Option<&str>,
) -> Option<String> {
    let key = wire.key.as_option()?;
    if key.from_me.unwrap_or(false) {
        key.participant
            .as_deref()
            .or(wire.participant.as_deref())
            .map(normalize_jid)
            .or_else(|| own_pn.map(str::to_owned))
    } else if chat_jid.ends_with("@g.us") {
        wire.participant
            .as_deref()
            .or(key.participant.as_deref())
            .map(normalize_jid)
    } else {
        Some(chat_jid.to_owned())
    }
}

fn history_message(
    chat_jid: &str,
    chat_name: &str,
    wire: &wa::WebMessageInfo,
    media_dir: &Path,
) -> Option<Message> {
    let key = wire.key.as_option()?;
    let id = key.id.clone()?;
    let body = wire.message.as_option()?;
    let from_me = key.from_me.unwrap_or(false);
    let sender_jid = if from_me {
        "me".to_owned()
    } else {
        normalize_jid(
            wire.participant
                .as_deref()
                .or(key.participant.as_deref())
                .unwrap_or(chat_jid),
        )
    };
    let sender_name = if from_me {
        "You".to_owned()
    } else {
        wire.push_name
            .as_deref()
            .and_then(nonempty)
            .unwrap_or_else(|| {
                if sender_jid == chat_jid {
                    chat_name.to_owned()
                } else {
                    sender_jid.clone()
                }
            })
    };
    let base = body.get_base_message();
    let timestamp = wire
        .message_timestamp
        .map_or(0, |timestamp| i64::try_from(timestamp).unwrap_or(i64::MAX));
    let text = base
        .text_content()
        .or_else(|| base.get_caption())
        .map(str::to_owned)
        .or_else(|| media_text(base, ""))?;
    let final_location = wire.final_live_location.as_option();
    let final_message = final_location.map(|location| wa::Message {
        live_location_message: buffa::MessageField::some(location.clone()),
        ..Default::default()
    });
    let media_source = final_message.as_ref().unwrap_or(base);
    let mut media = message_media(
        media_source,
        media_dir,
        chat_jid,
        &id,
        timestamp,
        wire.duration.unwrap_or(0),
    );
    if final_location.is_some()
        && let Some(MessageMedia::Location {
            live, updated_at, ..
        }) = &mut media
    {
        *live = false;
        *updated_at = timestamp.saturating_add(i64::from(
            final_location
                .and_then(|location| location.time_offset)
                .unwrap_or(0),
        ));
    }
    Some(Message {
        id,
        chat_jid: chat_jid.to_owned(),
        sender_jid,
        sender_name,
        text,
        timestamp,
        from_me,
        receipt: u8::from(from_me),
        delivered_at: None,
        read_at: None,
        delivered_to: Vec::new(),
        read_by: Vec::new(),
        media,
        reactions: Vec::new(),
    })
}

fn display_name(shared: &Shared, jid: &Jid) -> String {
    let raw = jid.to_non_ad_string();
    shared
        .database
        .chat_name(&raw)
        .ok()
        .flatten()
        .or_else(|| shared.database.contact_name(&raw).ok().flatten())
        .unwrap_or(raw)
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

const APP_EVENT_KINDS: &[EventKind] = &[
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
    EventKind::StarUpdate,
    EventKind::MarkChatAsReadUpdate,
    EventKind::DeleteChatUpdate,
    EventKind::ClearChatUpdate,
    EventKind::UserStatusMuteUpdate,
    EventKind::DeleteMessageForMeUpdate,
    EventKind::LabelEditUpdate,
    EventKind::LabelAssociationUpdate,
    EventKind::OfflineSyncCompleted,
    EventKind::DirtyState,
    EventKind::IdentityChange,
    EventKind::BusinessStatusUpdate,
    EventKind::StreamReplaced,
    EventKind::TemporaryBan,
    EventKind::DisappearingModeChanged,
    EventKind::ServerAck,
    EventKind::PairingQrCodesExhausted,
    EventKind::AppStateSyncFailed,
];

async fn handle_app_event(shared: Arc<Shared>, event: Arc<Event>, client: Arc<Client>) {
    match &*event {
        Event::Receipt(receipt) => {
            let receipt_type = receipt.r#type.as_wire_str();
            if matches!(receipt_type, "read-self" | "played-self") {
                let jid = canonical_contact_jid(&shared, &client, &receipt.source.chat).await;
                match shared.database.apply_self_read_receipt(
                    &jid,
                    &receipt.message_ids,
                    receipt.timestamp.timestamp(),
                ) {
                    Ok(true) => broadcast_snapshot(&shared),
                    Ok(false) => {}
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
                let chat_jid = canonical_contact_jid(&shared, &client, &receipt.source.chat).await;
                let recipient = if state >= 2
                    && (!receipt.source.chat.is_group()
                        || receipt.source.sender != receipt.source.chat)
                {
                    let jid = canonical_contact_jid(&shared, &client, &receipt.source.sender).await;
                    let name = shared
                        .database
                        .contact_name(&jid)
                        .ok()
                        .flatten()
                        .or_else(|| shared.database.chat_name(&jid).ok().flatten())
                        .filter(|name| name != &jid)
                        .unwrap_or_default();
                    Some((jid, name))
                } else {
                    None
                };
                let delivery = if state == 2 {
                    recipient.as_ref().map(|(jid, name)| MessageDelivery {
                        jid: jid.clone(),
                        name: name.clone(),
                        delivered_at: Some(receipt.timestamp.timestamp()),
                    })
                } else {
                    None
                };
                let reader = if state >= 3 {
                    recipient.as_ref().map(|(jid, name)| MessageReader {
                        jid: jid.clone(),
                        name: name.clone(),
                        read_at: Some(receipt.timestamp.timestamp()),
                    })
                } else {
                    None
                };
                match shared.database.update_receipts(
                    &chat_jid,
                    &receipt.message_ids,
                    state,
                    recipient.as_ref().map(|(jid, _)| jid.as_str()),
                    receipt.timestamp.timestamp(),
                ) {
                    Ok(true) => {
                        let _ = shared
                            .events
                            .send(ServerFrame::event(ServerEvent::Receipts {
                                message_ids: receipt.message_ids.clone(),
                                receipt: state,
                                timestamp: receipt.timestamp.timestamp(),
                                delivery,
                                reader,
                            }));
                    }
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
            let chat_jid = canonical_contact_jid(&shared, &client, &update.source.chat).await;
            let sender_jid = canonical_contact_jid(&shared, &client, &update.source.sender).await;
            let sender_name = shared
                .database
                .contact_name(&sender_jid)
                .ok()
                .flatten()
                .or_else(|| shared.database.chat_name(&sender_jid).ok().flatten())
                .filter(|name| name != &sender_jid)
                .unwrap_or_default();
            let _ = shared
                .events
                .send(ServerFrame::event(ServerEvent::ChatState {
                    chat_jid,
                    sender_jid,
                    sender_name,
                    state,
                }));
        }
        Event::Presence(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.from).await;
            let _ = shared
                .events
                .send(ServerFrame::event(ServerEvent::Presence {
                    jid,
                    available: !update.unavailable,
                    last_seen: update.last_seen.map(|last_seen| last_seen.timestamp()),
                }));
        }
        Event::PictureUpdate(update) => {
            let raw = canonical_contact_jid(&shared, &client, &update.jid).await;
            let jid = raw.parse::<Jid>().unwrap_or_else(|_| update.jid.clone());
            assets::remove_avatar(&shared.avatar_dir, &raw);
            if update.removed {
                let marker = assets::avatar_missing_path(&shared.avatar_dir, &raw);
                if let Err(error) = assets::write_private_bytes(&marker, b"none\n") {
                    warn!(%error, %raw, "could not persist missing-avatar marker");
                }
                shared.avatars_changed();
            } else {
                tokio::spawn(refresh_avatar(shared, client, jid, true));
            }
        }
        Event::ContactUpdated(update) => {
            tokio::spawn(refresh_avatar(
                Arc::clone(&shared),
                Arc::clone(&client),
                update.jid.clone(),
                true,
            ));
            tokio::spawn(sync_missing_contact_names(shared, client));
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
            tokio::spawn(refresh_avatar(
                Arc::clone(&shared),
                client,
                update.new_jid.clone(),
                true,
            ));
            broadcast_snapshot(&shared);
        }
        Event::ContactSyncRequested(_) => {
            tokio::spawn(sync_missing_contact_names(shared, client));
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
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
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
            if shared.presence_available.load(Ordering::SeqCst)
                && !update.new_name.is_empty()
                && let Err(error) = client.presence().set_available().await
            {
                warn!(%error, "could not restore deferred available WhatsApp presence");
            }
        }
        Event::PinUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
            if let Some(pinned) = update.action.pinned
                && let Err(error) = shared.database.apply_pin(&jid, pinned)
            {
                warn!(%error, "could not apply WhatsApp pin state");
            }
            broadcast_chats(&shared);
        }
        Event::MuteUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
            if let Some(muted) = update.action.muted
                && let Err(error) = shared.database.apply_mute(
                    &jid,
                    muted,
                    update.action.mute_end_timestamp.unwrap_or(0),
                )
            {
                warn!(%error, "could not apply WhatsApp mute state");
            }
        }
        Event::ArchiveUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
            if let Some(archived) = update.action.archived
                && let Err(error) = shared.database.apply_archive(&jid, archived)
            {
                warn!(%error, "could not apply WhatsApp archive state");
            }
            broadcast_snapshot(&shared);
        }
        Event::StarUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.chat_jid).await;
            if let Some(starred) = update.action.starred
                && let Err(error) = shared
                    .database
                    .star_message(&jid, &update.message_id, starred)
            {
                warn!(%error, "could not apply WhatsApp star state");
            }
        }
        Event::MarkChatAsReadUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
            if let Some(read) = update.action.read
                && let Err(error) = shared.database.apply_read_state(&jid, read)
            {
                warn!(%error, "could not apply cross-device WhatsApp read state");
            }
            broadcast_snapshot(&shared);
        }
        Event::DeleteChatUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
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
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
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
        Event::UserStatusMuteUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
            if let Err(error) = shared.database.apply_status_mute(&jid, update.muted) {
                warn!(%error, "could not apply WhatsApp status mute");
            }
        }
        Event::DeleteMessageForMeUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.chat_jid).await;
            if let Err(error) = shared.database.delete_message(&jid, &update.message_id) {
                warn!(%error, "could not apply WhatsApp message deletion");
            }
            assets::remove_message_media(&shared.media_dir, &jid, &update.message_id);
            broadcast_chats(&shared);
            broadcast_messages(&shared, &jid);
        }
        Event::LabelEditUpdate(update) => {
            if let Err(error) = shared.database.update_label(
                &update.label_id,
                update.action.name.as_deref(),
                update.action.color,
                update.action.deleted.unwrap_or(false),
            ) {
                warn!(%error, "could not persist WhatsApp label");
            }
        }
        Event::LabelAssociationUpdate(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.chat_jid).await;
            if let Err(error) = shared.database.associate_label(
                &jid,
                &update.label_id,
                update.action.labeled.unwrap_or(false),
            ) {
                warn!(%error, "could not persist WhatsApp label association");
            }
        }
        Event::OfflineSyncCompleted(details) => {
            info!(count = details.count, "WhatsApp offline sync completed");
            broadcast_snapshot(&shared);
            tokio::spawn(sync_group_names(Arc::clone(&shared), Arc::clone(&client)));
        }
        Event::DirtyState(details) => {
            info!(kind = ?details.dirty_type, "WhatsApp requested derived-state refresh");
            tokio::spawn(sync_group_names(Arc::clone(&shared), Arc::clone(&client)));
            tokio::spawn(sync_missing_contact_names(
                Arc::clone(&shared),
                Arc::clone(&client),
            ));
            tokio::spawn(sync_avatars(shared, client));
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
            let jid = canonical_contact_jid(&shared, &client, &update.jid).await;
            if let Some(name) = update.verified_name.as_deref()
                && let Err(error) = shared.database.update_contact_name(&jid, name)
            {
                warn!(%error, "could not update WhatsApp business name");
            }
            broadcast_chats(&shared);
            tokio::spawn(refresh_avatar(shared, client, update.jid.clone(), true));
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
        Event::DisappearingModeChanged(update) => {
            let jid = canonical_contact_jid(&shared, &client, &update.from).await;
            if let Err(error) = shared.database.apply_disappearing_mode(
                &jid,
                update.duration,
                update.setting_timestamp.timestamp(),
            ) {
                warn!(%error, "could not persist WhatsApp disappearing-message setting");
            }
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

async fn subscribe_live_location(client: &Client, target: Jid, is_group: bool) -> Result<()> {
    let subscribe = if is_group {
        NodeBuilder::new("subscribe")
            .attr("participants", "true")
            .build()
    } else {
        NodeBuilder::new("subscribe").build()
    };
    let server = SERVER_JID
        .parse::<Jid>()
        .context("parsing WhatsApp server JID")?;
    client
        .send_iq(
            InfoQuery::get(
                "location",
                server,
                Some(NodeContent::Nodes(vec![subscribe])),
            )
            .with_target(target)
            .with_timeout(std::time::Duration::from_secs(20)),
        )
        .await
        .context("subscribing to WhatsApp live location")?;
    Ok(())
}

async fn restore_live_location_subscriptions(shared: Arc<Shared>, client: Arc<Client>) {
    let targets = match shared
        .database
        .active_live_location_targets(Utc::now().timestamp())
    {
        Ok(targets) => targets,
        Err(error) => {
            warn!(%error, "could not list active live locations");
            return;
        }
    };
    for (target, is_group) in targets {
        let Ok(target) = target.parse::<Jid>() else {
            warn!("could not parse live-location subscription target");
            continue;
        };
        if let Err(error) = subscribe_live_location(&client, target, is_group).await {
            warn!(%error, "could not restore live-location subscription");
        }
    }
}

async fn maintain_live_location_subscriptions(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(25));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let Some(client) = shared.client.read().await.clone() else {
            continue;
        };
        restore_live_location_subscriptions(Arc::clone(&shared), client).await;
    }
}

async fn bind_private_listener(socket: &Path) -> Result<UnixListener> {
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                bail!(
                    "refusing to replace non-socket IPC path {}",
                    socket.display()
                );
            }
            match UnixStream::connect(socket).await {
                Ok(_) => bail!("another omarchy-whatsapp daemon is already running"),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    std::fs::remove_file(socket)
                        .with_context(|| format!("removing stale socket {}", socket.display()))?;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("checking existing socket {}", socket.display()));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting socket {}", socket.display()));
        }
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[tokio::main]
async fn main() -> Result<()> {
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

    let (app_events, library_events, excluded_events) = event_coverage::counts();
    info!(
        app_events,
        library_events, excluded_events, "loaded exhaustive WhatsApp event policy"
    );

    let listener = bind_private_listener(&paths.socket).await?;

    let (events, _) = broadcast::channel(256);
    let shared = Arc::new(Shared {
        database: Database::open(&paths.history_db)?,
        status: RwLock::new(ConnectionStatus::Starting),
        client: RwLock::new(None),
        active_chat: RwLock::new(None),
        presence_available: AtomicBool::new(false),
        events,
        pairing_qr: paths.runtime_dir.join("pairing.svg"),
        contact_sync_marker: paths.state_dir.join("contact-names-v2"),
        contact_history_marker: paths.state_dir.join("contact-history-names-v1"),
        event_sync_marker: paths.state_dir.join("event-state-v5"),
        avatar_dir,
        media_dir,
        voice_outbox_dir,
        avatar_revision: AtomicU64::new(0),
        app_state_failed: AtomicBool::new(false),
        logout_requested: AtomicBool::new(false),
        avatar_sync: Mutex::new(()),
        group_name_sync: Mutex::new(()),
        media_recovery_requested: RwLock::new(HashSet::new()),
        media_downloads: Mutex::new(HashSet::new()),
        voice_outbox_gate: Mutex::new(()),
    });

    tokio::spawn(backfill_video_previews(Arc::clone(&shared)));
    tokio::spawn(maintain_live_location_subscriptions(Arc::clone(&shared)));

    let ipc_shared = Arc::clone(&shared);
    let mut ipc_task = tokio::spawn(async move { serve(listener, ipc_shared).await });

    // An unpaired upstream client deliberately ends its run loop after the QR
    // window expires. Keep the lightweight daemon and its IPC socket alive,
    // rebuilding only the protocol client so the UI receives a fresh code.
    loop {
        shared.app_state_failed.store(false, Ordering::Relaxed);
        if let Err(error) =
            prepare_contact_name_resync(&paths.protocol_db, &shared.contact_sync_marker)
        {
            warn!(%error, "could not prepare WhatsApp contact-name resync");
        }
        if let Err(error) =
            prepare_event_state_resync(&paths.protocol_db, &shared.event_sync_marker)
        {
            warn!(%error, "could not prepare WhatsApp chat-state event resync");
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
        let fast_ratchet_shared = Arc::clone(&shared);

        let bot = Bot::builder()
            .with_backend(store)
            .with_device_props(
                DevicePropsOverride::new()
                    .with_os("Linux")
                    .with_platform_type(wa::device_props::PlatformType::DESKTOP),
            )
            .with_enc_handler(
                "frskmsg",
                live_location::FastRatchetHandler::new(fast_ratchet_shared),
            )
            .on_qr_code(move |code, timeout| {
                let shared = Arc::clone(&qr_shared);
                async move {
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
                async move {
                    *shared.client.write().await = Some(Arc::clone(&client));
                    shared.set_status(ConnectionStatus::Connected).await;
                    info!("connected to WhatsApp");
                    if shared.presence_available.load(Ordering::SeqCst)
                        && !client.push_name().is_empty()
                        && let Err(error) = client.presence().set_available().await
                    {
                        warn!(%error, "could not restore available WhatsApp presence");
                    }
                    if let Some(active_chat) = shared.active_chat.read().await.clone()
                        && let Ok(jid) = active_chat.parse::<Jid>()
                        && !jid.is_group()
                        && let Err(error) = client.presence().subscribe(jid).await
                    {
                        warn!(%error, "could not restore active-chat presence subscription");
                    }
                    let alias_shared = Arc::clone(&shared);
                    let alias_client = Arc::clone(&client);
                    tokio::spawn(async move {
                        reconcile_direct_chat_aliases(&alias_shared, &alias_client).await;
                        broadcast_snapshot(&alias_shared);
                    });
                    if !shared.event_sync_marker.exists() {
                        let marker_shared = Arc::clone(&shared);
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                            if !marker_shared.app_state_failed.load(Ordering::Relaxed) {
                                match marker_shared.database.reconcile_unread_after_full_sync() {
                                    Ok(changed) => {
                                        info!(changed, "reconciled imported unread counters with full app-state");
                                        broadcast_snapshot(&marker_shared);
                                    }
                                    Err(error) => warn!(%error, "could not reconcile WhatsApp unread counters"),
                                }
                                marker_shared.mark_event_sync_complete();
                            }
                        });
                    }
                    tokio::spawn(sync_group_names(Arc::clone(&shared), Arc::clone(&client)));
                    tokio::spawn(sync_avatars(Arc::clone(&shared), Arc::clone(&client)));
                    tokio::spawn(restore_live_location_subscriptions(
                        Arc::clone(&shared),
                        Arc::clone(&client),
                    ));
                    tokio::spawn(async move {
                        sync_missing_contact_names(Arc::clone(&shared), Arc::clone(&client)).await;
                        request_missing_contact_history(shared, client).await;
                    });
                }
            })
            .on_logged_out(move |_details| {
                let shared = Arc::clone(&logged_out_shared);
                async move {
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
                async move {
                    let ingest_shared = Arc::clone(&shared);
                    let own_pn = client.pn().map(|jid| jid.to_non_ad_string());
                    let result = tokio::task::spawn_blocking(move || match &*event {
                        Event::HistorySync(history) => {
                            ingest_shared.ingest_history(history, own_pn.as_deref())
                        }
                        _ => Ok(Vec::new()),
                    })
                    .await;
                    match result {
                        Ok(Ok(pending_media)) => {
                            if let Ok(chats) =
                                list_chats_with_phone_numbers(&shared, CHAT_LIST_LIMIT).await
                            {
                                let _ = shared
                                    .events
                                    .send(ServerFrame::event(ServerEvent::Chats { chats }));
                            }
                            let total = shared.unread_total_or_zero();
                            let _ = shared
                                .events
                                .send(ServerFrame::event(ServerEvent::Unread { total }));
                            tokio::spawn(sync_avatars(
                                Arc::clone(&shared),
                                Arc::clone(&client),
                            ));
                            tokio::spawn(restore_live_location_subscriptions(
                                Arc::clone(&shared),
                                Arc::clone(&client),
                            ));
                            let names_shared = Arc::clone(&shared);
                            let names_client = Arc::clone(&client);
                            tokio::spawn(async move {
                                sync_missing_contact_names(
                                    Arc::clone(&names_shared),
                                    Arc::clone(&names_client),
                                )
                                .await;
                                request_missing_contact_history(names_shared, names_client).await;
                            });
                            if !pending_media.is_empty() {
                                tokio::spawn(download_pending_media(
                                    Arc::clone(&shared),
                                    Arc::clone(&client),
                                    pending_media,
                                ));
                            }
                        }
                        Ok(Err(error)) => error!(%error, "could not ingest history sync"),
                        Err(error) => error!(%error, "history sync worker panicked"),
                    }
                }
            })
            .on_event_for(&[EventKind::ContactUpdate], move |event, client| {
                let shared = Arc::clone(&contact_shared);
                async move {
                    if let Event::ContactUpdate(update) = &*event {
                        ingest_contact_name(&shared, update);
                        canonical_contact_jid(&shared, &client, &update.jid).await;
                    }
                }
            })
            .on_event_for(&[EventKind::GroupUpdate], move |event, client| {
                let shared = Arc::clone(&group_shared);
                async move {
                    let Event::GroupUpdate(update) = &*event else {
                        return;
                    };
                    match client.groups().get_metadata(&update.group_jid).await {
                        Ok(metadata) => {
                            let participant_jids = metadata
                                .participants
                                .into_iter()
                                .map(|participant| {
                                    participant.phone_number.unwrap_or(participant.jid)
                                })
                                .collect();
                            match shared.database.update_group_name(
                                &update.group_jid.to_non_ad_string(),
                                &metadata.subject,
                            ) {
                                Ok(true) => broadcast_chats(&shared),
                                Ok(false) => {}
                                Err(error) => {
                                    warn!(%error, "could not update WhatsApp group subject");
                                }
                            }
                            let chat_jid = update.group_jid.to_non_ad_string();
                            let participants = resolve_group_participants(
                                &shared,
                                &client,
                                participant_jids,
                            )
                            .await;
                            let _ = shared.events.send(ServerFrame::event(
                                ServerEvent::GroupParticipants {
                                    chat_jid,
                                    participants,
                                },
                            ));
                        }
                        Err(error) => warn!(%error, "could not refresh WhatsApp group metadata"),
                    }
                }
            })
            .on_event_for(APP_EVENT_KINDS, move |event, client| {
                handle_app_event(Arc::clone(&app_event_shared), event, client)
            })
            .on_message(move |context| {
                let shared = Arc::clone(&message_shared);
                async move { shared.receive_message(context).await }
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
            result = &mut ipc_task => {
                result.context("IPC task panicked")??;
                bail!("IPC server stopped unexpectedly");
            }
        };
        if !restart {
            break;
        }
        *shared.client.write().await = None;
        if shared.logout_requested.swap(false, Ordering::SeqCst) {
            clear_local_account_data(&paths, &shared)
                .await
                .context("clearing local WhatsApp account data after logout")?;
            shared.set_status(ConnectionStatus::LoggedOut).await;
            info!("cleared local WhatsApp account data after logout");
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

async fn serve(listener: UnixListener, shared: Arc<Shared>) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let connection_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, connection_shared).await {
                tracing::debug!(%error, "IPC client disconnected");
            }
        });
    }
}

async fn write_connection_sync(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    shared: &Shared,
) -> Result<()> {
    write_frame(
        write,
        &ServerFrame::event(ServerEvent::Hello {
            protocol_version: PROTOCOL_VERSION,
        }),
    )
    .await?;
    write_frame(write, &ServerFrame::event(shared.state_event().await)).await
}

async fn serve_connection(stream: UnixStream, shared: Arc<Shared>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    // Bound memory even if another process owned by the same user sends a line
    // without a delimiter. Normal commands are only a few kilobytes.
    let mut lines = FramedRead::new(read, LinesCodec::new_with_max_length(128 * 1024));
    let mut events = shared.events.subscribe();
    let (commands, command_queue) = mpsc::channel::<(Option<u64>, Command)>(32);
    let (responses, mut response_queue) = mpsc::channel::<ServerFrame>(32);
    let command_shared = Arc::clone(&shared);
    tokio::spawn(process_command_queue(
        command_queue,
        responses,
        command_shared,
    ));
    write_connection_sync(&mut write, &shared).await?;

    loop {
        tokio::select! {
            line = lines.next() => {
                let Some(line) = line else { return Ok(()); };
                let line = line.context("reading IPC request")?;
                let frame = match serde_json::from_str::<ClientFrame>(&line) {
                    Ok(frame) => frame,
                    Err(error) => {
                        write_frame(&mut write, &ServerFrame::response(None, ServerEvent::Error {
                            message: format!("invalid request: {error}"),
                        })).await?;
                        continue;
                    }
                };
                let id = frame.id;
                if commands.try_send((id, frame.command)).is_err() {
                    write_frame(&mut write, &ServerFrame::response(id, ServerEvent::Error {
                        message: "too many queued requests".to_owned(),
                    })).await?;
                }
            },
            event = events.recv() => match event {
                Ok(event) => write_frame(&mut write, &event).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Repeating the versioned hello makes lag recovery explicit:
                    // compatible clients refresh every authoritative snapshot,
                    // rather than treating a state frame as a complete resync.
                    write_connection_sync(&mut write, &shared).await?;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            response = response_queue.recv() => {
                let Some(response) = response else { return Ok(()); };
                write_frame(&mut write, &response).await?;
            },
        }
    }
}

async fn process_command_queue(
    mut commands: mpsc::Receiver<(Option<u64>, Command)>,
    responses: mpsc::Sender<ServerFrame>,
    shared: Arc<Shared>,
) {
    while let Some((id, command)) = commands.recv().await {
        let event = handle_command(command, &shared)
            .await
            .unwrap_or_else(|error| ServerEvent::Error {
                message: error.to_string(),
            });
        let _ = responses.send(ServerFrame::response(id, event)).await;
    }
}

async fn write_frame(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    frame: &ServerFrame,
) -> Result<()> {
    let mut json = serde_json::to_vec(frame)?;
    json.push(b'\n');
    write.write_all(&json).await?;
    Ok(())
}

async fn canonical_requested_jid(shared: &Shared, raw: &str) -> String {
    let Ok(jid) = raw.parse::<Jid>() else {
        return raw.to_owned();
    };
    let client = shared.client.read().await.clone();
    match client {
        Some(client) => canonical_contact_jid(shared, &client, &jid).await,
        None => jid.to_non_ad_string(),
    }
}

async fn media_download_payload(
    shared: &Arc<Shared>,
    client: &Arc<Client>,
    chat_jid: &str,
    message_id: &str,
    media_label: &str,
) -> Result<Vec<u8>> {
    if let Some(payload) = shared.database.media_download(chat_jid, message_id)? {
        return Ok(payload);
    }

    let cursor = shared
        .database
        .message_history_cursor(chat_jid, message_id)?
        .ok_or_else(|| anyhow!("{media_label} message is no longer in local history"))?;
    let jid: Jid = chat_jid.parse().context("invalid media chat JID")?;
    request_exact_message(client, &cursor)
        .await
        .with_context(|| format!("requesting exact {media_label} message"))?;
    client
        .fetch_message_history(
            &jid,
            &cursor.message_id,
            cursor.from_me,
            cursor.timestamp_ms,
            3,
        )
        .await
        .with_context(|| format!("requesting {media_label} download metadata"))?;

    // History responses arrive through the normal event pipeline. Give that
    // pipeline a short bounded window to persist the exact media keys before
    // reporting that an old image is unavailable.
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Some(payload) = shared.database.media_download(chat_jid, message_id)? {
            return Ok(payload);
        }
    }
    bail!("WhatsApp did not return download details for this {media_label}")
}

async fn request_exact_message(
    client: &Arc<Client>,
    cursor: &database::HistoryCursor,
) -> Result<()> {
    let chat: Jid = cursor
        .chat_jid
        .parse()
        .context("invalid recovery chat JID")?;
    let sender = if cursor.from_me {
        chat.clone()
    } else {
        cursor
            .sender_jid
            .parse()
            .context("invalid recovery sender JID")?
    };
    let timestamp = chrono::DateTime::from_timestamp(cursor.timestamp_ms.div_euclid(1_000), 0)
        .unwrap_or_else(Utc::now);
    let info = Arc::new(MessageInfo {
        source: whatsapp_rust::wacore::types::message::MessageSource {
            chat: chat.clone(),
            sender,
            is_from_me: cursor.from_me,
            is_group: chat.is_group(),
            ..Default::default()
        },
        id: cursor.message_id.clone(),
        timestamp,
        ..Default::default()
    });
    client.send_pdo_placeholder_resend_request(&info).await
}

async fn finish_media_recovery_attempt(shared: &Shared, chat_jid: &str, succeeded: bool) -> bool {
    // The set is a successful-attempt marker as well as an in-flight guard.
    // Only a total transient failure should re-arm the next UI refresh.
    if succeeded {
        return false;
    }
    shared
        .media_recovery_requested
        .write()
        .await
        .remove(chat_jid)
}

async fn handle_command(command: Command, shared: &Arc<Shared>) -> Result<ServerEvent> {
    match command {
        Command::GetState => Ok(shared.state_event().await),
        Command::ListChats { limit } => Ok(ServerEvent::Chats {
            chats: list_chats_with_phone_numbers(shared, limit).await?,
        }),
        Command::GetGroupParticipants { chat_jid } => {
            let jid = chat_jid.parse::<Jid>().context("invalid group chat JID")?;
            if !jid.is_group() {
                bail!("participant lists are only available for group chats");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let info = client
                .groups()
                .query_info(&jid)
                .await
                .context("loading WhatsApp group participants")?;
            Ok(ServerEvent::GroupParticipants {
                chat_jid: jid.to_non_ad_string(),
                participants: resolve_group_participants(
                    shared,
                    &client,
                    info.participants.clone(),
                )
                .await,
            })
        }
        Command::GetMessages { chat_jid, limit } => {
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            let first_unread_message_id = shared.database.first_unread_message_id(&chat_jid)?;
            Ok(ServerEvent::Messages {
                messages: {
                    if let Some(cursor) = shared.database.media_recovery_cursor(&chat_jid)?
                        && let Some(client) = shared.client.read().await.clone()
                        && shared
                            .media_recovery_requested
                            .write()
                            .await
                            .insert(chat_jid.clone())
                        && let Ok(jid) = chat_jid.parse::<Jid>()
                    {
                        let recovery_chat_jid = chat_jid.clone();
                        let recovery_shared = Arc::clone(shared);
                        tokio::spawn(async move {
                            let exact_result = request_exact_message(&client, &cursor).await;
                            if let Err(error) = &exact_result {
                                warn!(%error, %jid, message_id = %cursor.message_id,
                                    "could not request exact media recovery");
                            }
                            let history_result = client
                                .fetch_message_history(
                                    &jid,
                                    &cursor.message_id,
                                    cursor.from_me,
                                    cursor.timestamp_ms,
                                    50,
                                )
                                .await;
                            if let Err(error) = &history_result {
                                warn!(%error, %jid, "could not request media history recovery");
                            }
                            finish_media_recovery_attempt(
                                &recovery_shared,
                                &recovery_chat_jid,
                                exact_result.is_ok() || history_result.is_ok(),
                            )
                            .await;
                        });
                    }
                    shared.database.messages(&chat_jid, limit)?
                },
                chat_jid,
                first_unread_message_id,
            })
        }
        Command::SendMessage { chat_jid, text } => {
            let text = text.trim().to_owned();
            if text.is_empty() {
                bail!("message cannot be empty");
            }
            if text.len() > 65_536 {
                bail!("message is too large");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
            let canonical = canonical_contact_jid(shared, &client, &requested).await;
            let jid: Jid = canonical.parse().context("invalid canonical chat JID")?;
            let result = client
                .send_message(&jid, wa::Message::text(text.clone()))
                .await?;
            let message = Message {
                id: result.message_id,
                chat_jid: jid.to_non_ad_string(),
                sender_jid: "me".into(),
                sender_name: "You".into(),
                text,
                timestamp: Utc::now().timestamp(),
                from_me: true,
                receipt: 1,
                delivered_at: None,
                read_at: None,
                delivered_to: Vec::new(),
                read_by: Vec::new(),
                media: None,
                reactions: Vec::new(),
            };
            let chat_name = shared
                .database
                .chat_name(&message.chat_jid)?
                .or_else(|| {
                    shared
                        .database
                        .contact_name(&message.chat_jid)
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| message.chat_jid.clone());
            shared
                .database
                .insert_message(&message, &chat_name, jid.is_group(), false)?;
            let _ = shared.events.send(ServerFrame::event(ServerEvent::Sent {
                message: message.clone(),
            }));
            Ok(ServerEvent::Sent { message })
        }
        Command::SendVoiceMessage {
            chat_jid,
            recording_id,
        } => {
            // IPC queues are per connection. Serialize the small voice outbox
            // globally so the shell and CLI cannot prepare/send one recording
            // concurrently with each other.
            let _voice_outbox_guard = shared.voice_outbox_gate.lock().await;
            let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
            let outbox_dir = shared.voice_outbox_dir.clone();
            let prepare_recording_id = recording_id.clone();
            let prepare_chat_jid = requested.to_non_ad_string();
            let mut prepared = tokio::task::spawn_blocking(move || {
                voice_outbox::prepare(
                    &outbox_dir,
                    &prepare_recording_id,
                    &prepare_chat_jid,
                    Utc::now().timestamp(),
                )
            })
            .await
            .context("voice outbox preparation task failed")??;
            broadcast_voice_outbox(shared);
            let result: Result<Message> = async {
                let client = shared
                    .client
                    .read()
                    .await
                    .clone()
                    .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
                let requested: Jid = prepared
                    .job
                    .chat_jid
                    .parse()
                    .context("invalid persisted voice message chat JID")?;
                let canonical = canonical_contact_jid(shared, &client, &requested).await;
                let jid: Jid = canonical.parse().context("invalid canonical chat JID")?;
                let delivery_id = prepared
                    .job
                    .message_id
                    .clone()
                    .unwrap_or_else(|| client.generate_message_id());
                voice_outbox::assign_delivery(
                    &shared.voice_outbox_dir,
                    &mut prepared.job,
                    &jid.to_non_ad_string(),
                    &delivery_id,
                    Utc::now().timestamp(),
                )?;
                broadcast_voice_outbox(shared);
                if let Some(message) = shared
                    .database
                    .message_by_id(&jid.to_non_ad_string(), &delivery_id)?
                {
                    return Ok(message);
                }
                let upload = client
                    .upload(
                        std::mem::take(&mut prepared.bytes),
                        MediaType::Audio,
                        UploadOptions::new(),
                    )
                    .await
                    .context("uploading voice message")?;
                let duration_seconds =
                    u32::try_from(prepared.job.duration_ms.div_ceil(1_000)).unwrap_or(u32::MAX);
                let outbound = media::audio_message(
                    upload,
                    media::AudioOptions {
                        mimetype: Some("audio/ogg; codecs=opus".into()),
                        duration_seconds: Some(duration_seconds),
                        ptt: Some(true),
                        ..Default::default()
                    },
                );
                let sent = client
                    .send_message_with_options(
                        &jid,
                        outbound,
                        SendOptions::default().with_message_id(&delivery_id),
                    )
                    .await?;
                if sent.message_id != delivery_id {
                    bail!("WhatsApp returned a different voice message ID");
                }
                let cached_path = assets::message_audio_path(
                    &shared.media_dir,
                    &jid.to_non_ad_string(),
                    &delivery_id,
                    Some("audio/ogg; codecs=opus"),
                );
                let recording_path =
                    voice_outbox::recording_path(&shared.voice_outbox_dir, &recording_id)?;
                let copy_source = recording_path.clone();
                let copy_destination = cached_path.clone();
                let downloaded = match tokio::task::spawn_blocking(move || {
                    assets::copy_private_file(&copy_source, &copy_destination)
                })
                .await
                .context("voice cache copy task failed")?
                {
                    Ok(()) => true,
                    Err(error) => {
                        warn!(%error, "could not retain sent voice message in the private cache");
                        false
                    }
                };
                assets::prune_media_cache(
                    &shared.media_dir,
                    if downloaded {
                        &cached_path
                    } else {
                        &recording_path
                    },
                );
                let message = Message {
                    id: delivery_id,
                    chat_jid: jid.to_non_ad_string(),
                    sender_jid: "me".into(),
                    sender_name: "You".into(),
                    text: "[Voice message]".into(),
                    timestamp: Utc::now().timestamp(),
                    from_me: true,
                    receipt: 1,
                    delivered_at: None,
                    read_at: None,
                    delivered_to: Vec::new(),
                    read_by: Vec::new(),
                    media: Some(MessageMedia::Audio {
                        path: cached_path.to_string_lossy().into_owned(),
                        downloaded,
                        mime_type: "audio/ogg; codecs=opus".into(),
                        duration_seconds,
                        voice_message: true,
                    }),
                    reactions: Vec::new(),
                };
                let chat_name = shared
                    .database
                    .chat_name(&message.chat_jid)?
                    .or_else(|| {
                        shared
                            .database
                            .contact_name(&message.chat_jid)
                            .ok()
                            .flatten()
                    })
                    .unwrap_or_else(|| message.chat_jid.clone());
                shared
                    .database
                    .insert_message(&message, &chat_name, jid.is_group(), false)?;
                let _ = shared.events.send(ServerFrame::event(ServerEvent::Sent {
                    message: message.clone(),
                }));
                Ok(message)
            }
            .await;
            match result {
                Ok(message) => {
                    if let Err(error) = voice_outbox::finish_sent(
                        &shared.voice_outbox_dir,
                        &mut prepared.job,
                        Utc::now().timestamp(),
                    ) {
                        warn!(%error, "could not finalize a sent voice outbox entry");
                    }
                    broadcast_voice_outbox(shared);
                    Ok(ServerEvent::Sent { message })
                }
                Err(error) => {
                    if let Err(persist_error) = voice_outbox::mark_failed(
                        &shared.voice_outbox_dir,
                        &mut prepared.job,
                        &error.to_string(),
                        Utc::now().timestamp(),
                    ) {
                        warn!(%persist_error, "could not retain a failed voice outbox entry");
                    }
                    broadcast_voice_outbox(shared);
                    Err(error)
                }
            }
        }
        Command::DiscardVoiceRecording { recording_id } => {
            let _voice_outbox_guard = shared.voice_outbox_gate.lock().await;
            voice_outbox::discard(&shared.voice_outbox_dir, &recording_id)?;
            broadcast_voice_outbox(shared);
            Ok(ServerEvent::Ack)
        }
        Command::ListVoiceOutbox => {
            let _voice_outbox_guard = shared.voice_outbox_gate.lock().await;
            voice_outbox_event(shared)
        }
        Command::CreatePoll {
            chat_jid,
            question,
            options,
            selectable_count,
            correct_option_index,
        } => {
            let question = question.trim().to_owned();
            if question.is_empty() {
                bail!("poll question cannot be empty");
            }
            let options: Vec<String> = options
                .into_iter()
                .map(|option| option.trim().to_owned())
                .filter(|option| !option.is_empty())
                .collect();
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
            let canonical = canonical_contact_jid(shared, &client, &requested).await;
            let jid: Jid = canonical.parse().context("invalid canonical chat JID")?;
            let creator_jid = own_poll_creator_jid(&client, &jid).await?;
            let (result, message_secret) = if let Some(correct_index) = correct_option_index {
                client
                    .polls()
                    .create_quiz(
                        jid.clone(),
                        &question,
                        &options,
                        usize::try_from(correct_index).context("invalid correct option index")?,
                    )
                    .await?
            } else {
                client
                    .polls()
                    .create(jid.clone(), &question, &options, selectable_count)
                    .await?
            };
            let message = Message {
                id: result.message_id,
                chat_jid: jid.to_non_ad_string(),
                sender_jid: "me".into(),
                sender_name: "You".into(),
                text: format!("[Poll] {question}"),
                timestamp: Utc::now().timestamp(),
                from_me: true,
                receipt: 1,
                delivered_at: None,
                read_at: None,
                delivered_to: Vec::new(),
                read_by: Vec::new(),
                media: Some(MessageMedia::Poll {
                    question,
                    options: options
                        .into_iter()
                        .map(|name| PollOption {
                            name,
                            votes: 0,
                            selected_by_me: false,
                        })
                        .collect(),
                    selectable_count: if correct_option_index.is_some() {
                        1
                    } else {
                        selectable_count
                    },
                    total_voters: 0,
                    quiz: correct_option_index.is_some(),
                    correct_option_index,
                    end_timestamp: 0,
                }),
                reactions: Vec::new(),
            };
            let chat_name = shared
                .database
                .chat_name(&message.chat_jid)?
                .or_else(|| {
                    shared
                        .database
                        .contact_name(&message.chat_jid)
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| message.chat_jid.clone());
            shared
                .database
                .insert_message(&message, &chat_name, jid.is_group(), false)?;
            shared.database.store_poll_secret(
                &message.chat_jid,
                &message.id,
                &creator_jid.to_non_ad_string(),
                &message_secret,
            )?;
            let _ = shared.events.send(ServerFrame::event(ServerEvent::Sent {
                message: message.clone(),
            }));
            Ok(ServerEvent::Sent { message })
        }
        Command::VotePoll {
            chat_jid,
            message_id,
            selected_options,
        } => {
            if message_id.is_empty() || message_id.len() > 512 {
                bail!("invalid poll message ID");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            let jid: Jid = chat_jid.parse().context("invalid chat JID")?;
            let poll = shared
                .database
                .poll_for_voting(&chat_jid, &message_id)?
                .ok_or_else(|| {
                    anyhow!("poll details are unavailable; its history may need to be recovered")
                })?;
            if poll.end_timestamp > 0 && poll.end_timestamp <= Utc::now().timestamp() {
                bail!("this poll has ended");
            }
            let mut selected = Vec::new();
            for option in selected_options {
                if !poll.options.contains(&option) {
                    bail!("poll vote contains an unknown option");
                }
                if !selected.contains(&option) {
                    selected.push(option);
                }
            }
            if selected.len() > usize::try_from(poll.selectable_count).unwrap_or(usize::MAX) {
                bail!("poll vote selects more options than the poll allows");
            }
            let creator_jid: Jid = poll
                .creator_jid
                .parse()
                .context("stored poll creator JID is invalid")?;
            client
                .polls()
                .vote(
                    jid,
                    &message_id,
                    &creator_jid,
                    &poll.message_secret,
                    &selected,
                )
                .await?;
            if shared.database.apply_poll_vote(
                &chat_jid,
                &message_id,
                "me",
                &selected,
                true,
                Utc::now().timestamp_millis(),
            )? {
                broadcast_messages(shared, &chat_jid);
            }
            Ok(ServerEvent::Ack)
        }
        Command::DownloadImage {
            chat_jid,
            message_id,
        } => {
            if message_id.is_empty() || message_id.len() > 512 {
                bail!("invalid image message ID");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            if shared
                .database
                .message_media_kind(&chat_jid, &message_id)?
                .as_deref()
                != Some("image")
            {
                bail!("message is not an image");
            }
            let key = format!("{chat_jid}\0{message_id}");
            {
                let mut downloads = shared.media_downloads.lock().await;
                if !downloads.insert(key.clone()) {
                    bail!("this image is already downloading");
                }
            }

            let result = async {
                let payload =
                    media_download_payload(shared, &client, &chat_jid, &message_id, "image")
                        .await?;
                let image = wa::message::ImageMessage::decode_from_slice(&payload)
                    .context("reading image download metadata")?;
                let path = assets::message_image_path(&shared.media_dir, &chat_jid, &message_id);
                let thumbnail_path = assets::cache_message_image_thumbnail(
                    &shared.media_dir,
                    &chat_jid,
                    &message_id,
                    image.jpeg_thumbnail.as_ref(),
                    image.file_length,
                )?;
                let mut media = MessageMedia::Image {
                    path: path.to_string_lossy().into_owned(),
                    thumbnail_path: thumbnail_path.to_string_lossy().into_owned(),
                    downloaded: path.exists(),
                    mime_type: image
                        .mimetype
                        .clone()
                        .unwrap_or_else(|| "image/jpeg".to_owned()),
                    width: image.width.unwrap_or(0),
                    height: image.height.unwrap_or(0),
                };
                shared
                    .database
                    .update_message_media(&chat_jid, &message_id, &media)?;
                assets::download_message_image(client, image, path).await?;
                if let MessageMedia::Image { downloaded, .. } = &mut media {
                    *downloaded = true;
                }
                Result::<MessageMedia>::Ok(media)
            }
            .await;
            shared.media_downloads.lock().await.remove(&key);
            Ok(ServerEvent::MediaDownloaded {
                media: result?,
                chat_jid,
                message_id,
            })
        }
        Command::DownloadSticker {
            chat_jid,
            message_id,
        } => {
            if message_id.is_empty() || message_id.len() > 512 {
                bail!("invalid sticker message ID");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            if shared
                .database
                .message_media_kind(&chat_jid, &message_id)?
                .as_deref()
                != Some("sticker")
            {
                bail!("message is not a sticker");
            }
            let key = format!("{chat_jid}\0{message_id}");
            {
                let mut downloads = shared.media_downloads.lock().await;
                if !downloads.insert(key.clone()) {
                    bail!("this sticker is already downloading");
                }
            }

            let result = async {
                let payload =
                    media_download_payload(shared, &client, &chat_jid, &message_id, "sticker")
                        .await?;
                let sticker = wa::message::StickerMessage::decode_from_slice(&payload)
                    .context("reading sticker download metadata")?;
                if sticker.is_lottie.unwrap_or(false) {
                    bail!("Lottie sticker animation is not supported safely");
                }
                let path = assets::message_sticker_path(&shared.media_dir, &chat_jid, &message_id);
                let thumbnail_path = assets::cache_message_sticker_thumbnail(
                    &shared.media_dir,
                    &chat_jid,
                    &message_id,
                    sticker.png_thumbnail.as_ref(),
                )?;
                let mut media = MessageMedia::Sticker {
                    path: path.to_string_lossy().into_owned(),
                    thumbnail_path: thumbnail_path.to_string_lossy().into_owned(),
                    downloaded: path.exists(),
                    mime_type: sticker
                        .mimetype
                        .clone()
                        .unwrap_or_else(|| "image/webp".to_owned()),
                    width: sticker.width.unwrap_or(0),
                    height: sticker.height.unwrap_or(0),
                    animated: sticker.is_animated.unwrap_or(false),
                    lottie: false,
                    accessibility_label: sticker.accessibility_label.clone().unwrap_or_default(),
                };
                shared
                    .database
                    .update_message_media(&chat_jid, &message_id, &media)?;
                assets::download_message_sticker(client, sticker, path).await?;
                if let MessageMedia::Sticker { downloaded, .. } = &mut media {
                    *downloaded = true;
                }
                Result::<MessageMedia>::Ok(media)
            }
            .await;
            shared.media_downloads.lock().await.remove(&key);
            Ok(ServerEvent::MediaDownloaded {
                media: result?,
                chat_jid,
                message_id,
            })
        }
        Command::DownloadVideo {
            chat_jid,
            message_id,
        } => {
            if message_id.is_empty() || message_id.len() > 512 {
                bail!("invalid video message ID");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            if shared
                .database
                .message_media_kind(&chat_jid, &message_id)?
                .as_deref()
                != Some("video")
            {
                bail!("message is not a video");
            }
            let key = format!("{chat_jid}\0{message_id}");
            {
                let mut downloads = shared.media_downloads.lock().await;
                if !downloads.insert(key.clone()) {
                    bail!("this video is already downloading");
                }
            }

            let result = async {
                let payload =
                    media_download_payload(shared, &client, &chat_jid, &message_id, "video")
                        .await?;
                let video = wa::message::VideoMessage::decode_from_slice(&payload)
                    .context("reading video download metadata")?;
                let path = assets::message_video_path(
                    &shared.media_dir,
                    &chat_jid,
                    &message_id,
                    video.mimetype.as_deref(),
                );
                let thumbnail_path = assets::cache_message_video_thumbnail(
                    &shared.media_dir,
                    &chat_jid,
                    &message_id,
                    video.mimetype.as_deref(),
                    video.jpeg_thumbnail.as_ref(),
                    video.file_length,
                )?;
                let mut media = MessageMedia::Video {
                    path: path.to_string_lossy().into_owned(),
                    thumbnail_path: thumbnail_path.to_string_lossy().into_owned(),
                    downloaded: path.exists(),
                    mime_type: video
                        .mimetype
                        .clone()
                        .unwrap_or_else(|| "video/mp4".to_owned()),
                    width: video.width.unwrap_or(0),
                    height: video.height.unwrap_or(0),
                    duration_seconds: video.seconds.unwrap_or(0),
                    gif_playback: video.gif_playback.unwrap_or(false),
                };
                shared
                    .database
                    .update_message_media(&chat_jid, &message_id, &media)?;
                assets::download_message_video(client, video, path.clone()).await?;
                let preview_result = tokio::task::spawn_blocking(move || {
                    assets::ensure_message_video_thumbnail(&path, &thumbnail_path)
                })
                .await;
                match preview_result {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        warn!(%error, "could not generate downloaded video preview");
                    }
                    Err(error) => warn!(%error, "video preview worker panicked"),
                }
                if let MessageMedia::Video { downloaded, .. } = &mut media {
                    *downloaded = true;
                }
                Result::<MessageMedia>::Ok(media)
            }
            .await;
            shared.media_downloads.lock().await.remove(&key);
            Ok(ServerEvent::MediaDownloaded {
                media: result?,
                chat_jid,
                message_id,
            })
        }
        Command::DownloadAudio {
            chat_jid,
            message_id,
        } => {
            if message_id.is_empty() || message_id.len() > 512 {
                bail!("invalid audio message ID");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            if shared
                .database
                .message_media_kind(&chat_jid, &message_id)?
                .as_deref()
                != Some("audio")
            {
                bail!("message is not audio");
            }
            let key = format!("{chat_jid}\0{message_id}");
            {
                let mut downloads = shared.media_downloads.lock().await;
                if !downloads.insert(key.clone()) {
                    bail!("this audio is already downloading");
                }
            }

            let result = async {
                let payload =
                    media_download_payload(shared, &client, &chat_jid, &message_id, "audio")
                        .await?;
                let audio = wa::message::AudioMessage::decode_from_slice(&payload)
                    .context("reading audio download metadata")?;
                let path = assets::message_audio_path(
                    &shared.media_dir,
                    &chat_jid,
                    &message_id,
                    audio.mimetype.as_deref(),
                );
                let mut media = MessageMedia::Audio {
                    path: path.to_string_lossy().into_owned(),
                    downloaded: path.exists(),
                    mime_type: audio
                        .mimetype
                        .clone()
                        .unwrap_or_else(|| "audio/ogg; codecs=opus".to_owned()),
                    duration_seconds: audio.seconds.unwrap_or(0),
                    voice_message: audio.ptt.unwrap_or(false),
                };
                shared
                    .database
                    .update_message_media(&chat_jid, &message_id, &media)?;
                assets::download_message_audio(client, audio, path).await?;
                if let MessageMedia::Audio { downloaded, .. } = &mut media {
                    *downloaded = true;
                }
                Result::<MessageMedia>::Ok(media)
            }
            .await;
            shared.media_downloads.lock().await.remove(&key);
            Ok(ServerEvent::MediaDownloaded {
                media: result?,
                chat_jid,
                message_id,
            })
        }
        Command::React {
            chat_jid,
            message_id,
            sender_jid,
            target_from_me,
            emoji,
        } => {
            if message_id.is_empty() {
                bail!("reaction target is missing");
            }
            if emoji.len() > 64 || emoji.chars().any(char::is_control) {
                bail!("reaction must be a short emoji");
            }
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
            let chat_jid = canonical_contact_jid(shared, &client, &requested).await;
            let chat: Jid = chat_jid.parse().context("invalid canonical chat JID")?;
            let participant = if chat.is_group() {
                if target_from_me {
                    Some(
                        client
                            .lid()
                            .or_else(|| client.pn())
                            .ok_or_else(|| anyhow!("WhatsApp identity is unavailable"))?
                            .to_non_ad_string(),
                    )
                } else {
                    Some(
                        sender_jid
                            .parse::<Jid>()
                            .context("invalid reaction target sender JID")?
                            .to_non_ad_string(),
                    )
                }
            } else {
                None
            };
            let target_key = wa::MessageKey {
                remote_jid: Some(chat.to_non_ad_string()),
                from_me: Some(target_from_me),
                id: Some(message_id.clone()),
                participant,
            };
            client.send_reaction(&chat, target_key, &emoji).await?;
            shared.database.apply_reaction(
                &chat_jid,
                &message_id,
                "me",
                &emoji,
                true,
                Utc::now().timestamp(),
            )?;
            broadcast_messages(shared, &chat_jid);
            Ok(ServerEvent::Ack)
        }
        Command::MarkRead { chat_jid } => {
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            let receipts = shared.database.unread_receipts(&chat_jid)?;
            shared.database.mark_read(&chat_jid)?;
            if let Some(client) = shared.client.read().await.clone() {
                let chat: Jid = chat_jid.parse().context("invalid chat JID")?;
                if receipts.first().is_some_and(|receipt| receipt.is_group) {
                    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
                    for receipt in receipts {
                        grouped
                            .entry(receipt.sender_jid)
                            .or_default()
                            .push(receipt.message_id);
                    }
                    for (sender, ids) in grouped {
                        let sender: Jid = sender.parse().context("invalid sender JID")?;
                        let refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
                        client.mark_as_read(&chat, Some(&sender), &refs).await?;
                    }
                } else {
                    let ids = receipts
                        .iter()
                        .map(|receipt| receipt.message_id.as_str())
                        .collect::<Vec<_>>();
                    if !ids.is_empty() {
                        client.mark_as_read(&chat, None, &ids).await?;
                    }
                }
                client
                    .chat_actions()
                    .mark_chat_as_read(&chat, true, None)
                    .await?;
            }
            let total = shared.database.unread_total()?;
            broadcast_chats(shared);
            let _ = shared
                .events
                .send(ServerFrame::event(ServerEvent::Unread { total }));
            Ok(ServerEvent::Ack)
        }
        Command::SetChatPinned { chat_jid, pinned } => {
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
            let chat_jid = canonical_contact_jid(shared, &client, &requested).await;
            let chat: Jid = chat_jid.parse().context("invalid canonical chat JID")?;
            if pinned {
                client.chat_actions().pin_chat(&chat).await?;
            } else {
                client.chat_actions().unpin_chat(&chat).await?;
            }
            shared.database.apply_pin(&chat_jid, pinned)?;
            broadcast_chats(shared);
            Ok(ServerEvent::Ack)
        }
        Command::RequestAvatar { jid } => {
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let requested: Jid = jid.parse().context("invalid avatar JID")?;
            let canonical = canonical_contact_jid(shared, &client, &requested).await;
            let parsed: Jid = canonical.parse().context("invalid canonical avatar JID")?;
            let avatar_shared = Arc::clone(shared);
            tokio::spawn(async move {
                refresh_avatar(avatar_shared, client, parsed, false).await;
            });
            Ok(ServerEvent::Ack)
        }
        Command::ListAvatars => Ok(ServerEvent::Avatars {
            revision: shared.avatar_revision.load(Ordering::Relaxed),
            jids: shared.avatar_snapshot(),
            changed_jids: Vec::new(),
        }),
        Command::SetActiveChat { chat_jid } => {
            let client = shared.client.read().await.clone();
            let next = match chat_jid {
                Some(chat_jid) => {
                    let requested: Jid = chat_jid.parse().context("invalid active chat JID")?;
                    Some(match client.as_ref() {
                        Some(client) => canonical_contact_jid(shared, client, &requested).await,
                        None => requested.to_non_ad_string(),
                    })
                }
                None => None,
            };
            let previous = {
                let mut active_chat = shared.active_chat.write().await;
                if *active_chat == next {
                    return Ok(ServerEvent::Ack);
                }
                std::mem::replace(&mut *active_chat, next.clone())
            };
            if let Some(client) = client {
                if let Some(previous) = previous
                    && let Ok(jid) = previous.parse::<Jid>()
                    && !jid.is_group()
                    && let Err(error) = client.presence().unsubscribe(&jid).await
                {
                    warn!(%error, %jid, "could not unsubscribe from prior chat presence");
                }
                if let Some(next) = next
                    && let Ok(jid) = next.parse::<Jid>()
                    && !jid.is_group()
                    && let Err(error) = client.presence().subscribe(jid.clone()).await
                {
                    warn!(%error, %jid, "could not subscribe to active chat presence");
                }
            }
            Ok(ServerEvent::Ack)
        }
        Command::SetPresence { available } => {
            shared.presence_available.store(available, Ordering::SeqCst);
            if let Some(client) = shared.client.read().await.clone() {
                if client.push_name().is_empty() {
                    debug!(
                        available,
                        "deferring WhatsApp presence until the push name is restored"
                    );
                } else if available {
                    client.presence().set_available().await?;
                } else {
                    client.presence().set_unavailable().await?;
                }
            }
            Ok(ServerEvent::Ack)
        }
        Command::SetChatState { chat_jid, state } => {
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let requested: Jid = chat_jid.parse().context("invalid chat-state JID")?;
            let canonical = canonical_contact_jid(shared, &client, &requested).await;
            let jid: Jid = canonical
                .parse()
                .context("invalid canonical chat-state JID")?;
            match state {
                ChatState::Typing => client.chatstate().send_composing(&jid).await?,
                ChatState::Recording => client.chatstate().send_recording(&jid).await?,
                ChatState::Paused => client.chatstate().send_paused(&jid).await?,
            }
            Ok(ServerEvent::Ack)
        }
        Command::Logout => {
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            shared.logout_requested.store(true, Ordering::SeqCst);
            client.logout().await;
            Ok(ServerEvent::Ack)
        }
        Command::Ping => Ok(ServerEvent::Pong),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buffa::MessageField;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;
    use tokio::io::AsyncReadExt;

    fn test_shared(directory: &tempfile::TempDir) -> Shared {
        let (events, _) = broadcast::channel(8);
        Shared {
            database: Database::open(&directory.path().join("history.db")).unwrap(),
            status: RwLock::new(ConnectionStatus::Starting),
            client: RwLock::new(None),
            active_chat: RwLock::new(None),
            presence_available: AtomicBool::new(false),
            events,
            pairing_qr: directory.path().join("pairing.svg"),
            contact_sync_marker: directory.path().join("contact-names-v2"),
            contact_history_marker: directory.path().join("contact-history-names-v1"),
            event_sync_marker: directory.path().join("event-state-v5"),
            avatar_dir: directory.path().join("avatars"),
            media_dir: directory.path().join("media"),
            voice_outbox_dir: directory.path().join("outbox"),
            avatar_revision: AtomicU64::new(0),
            app_state_failed: AtomicBool::new(false),
            logout_requested: AtomicBool::new(false),
            avatar_sync: Mutex::new(()),
            group_name_sync: Mutex::new(()),
            media_recovery_requested: RwLock::new(HashSet::new()),
            media_downloads: Mutex::new(HashSet::new()),
            voice_outbox_gate: Mutex::new(()),
        }
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

    #[tokio::test]
    async fn offline_presence_and_active_chat_intent_are_retained() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));

        assert_eq!(
            handle_command(Command::SetPresence { available: true }, &shared)
                .await
                .unwrap(),
            ServerEvent::Ack
        );
        assert!(shared.presence_available.load(Ordering::SeqCst));

        assert_eq!(
            handle_command(
                Command::SetActiveChat {
                    chat_jid: Some("1@s.whatsapp.net".into()),
                },
                &shared,
            )
            .await
            .unwrap(),
            ServerEvent::Ack
        );
        assert_eq!(
            shared.active_chat.read().await.as_deref(),
            Some("1@s.whatsapp.net")
        );
        assert!(
            handle_command(
                Command::SetActiveChat {
                    chat_jid: Some("not a jid".into()),
                },
                &shared,
            )
            .await
            .is_err()
        );
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
        shared.event_sync_marker = directory.path().join("event-state-v5");
        shared.mark_event_sync_complete();
        assert!(shared.event_sync_marker.exists());
    }

    #[tokio::test]
    async fn command_worker_reports_errors_and_closes_with_its_queue() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let (commands, command_queue) = mpsc::channel(1);
        let (responses, mut response_queue) = mpsc::channel(1);
        commands
            .send((
                Some(4),
                Command::SendMessage {
                    chat_jid: "chat@s.whatsapp.net".into(),
                    text: "  ".into(),
                },
            ))
            .await
            .unwrap();
        drop(commands);

        process_command_queue(command_queue, responses, shared).await;

        let response = response_queue.recv().await.unwrap();
        assert_eq!(response.id, Some(4));
        assert_eq!(
            response.event,
            ServerEvent::Error {
                message: "message cannot be empty".into(),
            }
        );
    }

    #[tokio::test]
    async fn listener_rejects_live_daemons_and_non_socket_paths_but_replaces_stale_sockets() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        std::fs::write(&socket, b"do not replace").unwrap();
        assert!(bind_private_listener(&socket).await.is_err());
        assert_eq!(std::fs::read(&socket).unwrap(), b"do not replace");
        std::fs::remove_file(&socket).unwrap();

        let live = UnixListener::bind(&socket).unwrap();
        let error = bind_private_listener(&socket).await.unwrap_err();
        assert!(error.to_string().contains("already running"));
        drop(live);

        let rebound = bind_private_listener(&socket).await.unwrap();
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(rebound);
    }

    #[tokio::test]
    async fn failed_media_recovery_is_rearmed_but_a_successful_attempt_stays_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        shared
            .media_recovery_requested
            .write()
            .await
            .insert("chat@s.whatsapp.net".into());
        assert!(finish_media_recovery_attempt(&shared, "chat@s.whatsapp.net", false).await);
        assert!(shared.media_recovery_requested.read().await.is_empty());

        shared
            .media_recovery_requested
            .write()
            .await
            .insert("chat@s.whatsapp.net".into());
        assert!(!finish_media_recovery_attempt(&shared, "chat@s.whatsapp.net", true).await);
        assert!(
            shared
                .media_recovery_requested
                .read()
                .await
                .contains("chat@s.whatsapp.net")
        );
    }

    #[tokio::test]
    async fn ipc_broadcasts_continue_while_a_command_is_waiting() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let client_guard = shared.client.write().await;
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..2 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        let request =
            serde_json::to_vec(&ClientFrame::new(Some(7), Command::ListChats { limit: 10 }))
                .unwrap();
        client_stream.write_all(&request).await.unwrap();
        client_stream.write_all(b"\n").await.unwrap();
        tokio::task::yield_now().await;
        shared
            .events
            .send(ServerFrame::event(ServerEvent::Unread { total: 9 }))
            .unwrap();

        buffer.clear();
        let broadcast = read_test_frame(&mut client_stream, &mut buffer).await;
        assert_eq!(broadcast.event, ServerEvent::Unread { total: 9 });

        drop(client_guard);
        buffer.clear();
        let response = read_test_frame(&mut client_stream, &mut buffer).await;
        assert_eq!(response.id, Some(7));
        assert!(matches!(response.event, ServerEvent::Chats { .. }));

        drop(client_stream);
        assert!(server.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn lagged_ipc_clients_receive_a_versioned_resync_handshake() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..2 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        let oversized = "x".repeat(96 * 1024);
        for _ in 0..24 {
            shared
                .events
                .send(ServerFrame::event(ServerEvent::Error {
                    message: oversized.clone(),
                }))
                .unwrap();
        }

        let hello = loop {
            buffer.clear();
            let frame = read_test_frame(&mut client_stream, &mut buffer).await;
            if matches!(frame.event, ServerEvent::Hello { .. }) {
                break frame;
            }
        };
        assert_eq!(
            hello.event,
            ServerEvent::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
        buffer.clear();
        assert!(matches!(
            read_test_frame(&mut client_stream, &mut buffer).await.event,
            ServerEvent::State { .. }
        ));

        drop(client_stream);
        // Pending oversized broadcasts can observe the intentional client
        // close as a broken pipe; the connection task itself must not panic.
        let _ = server.await.unwrap();
    }

    #[tokio::test]
    async fn ipc_command_queue_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let client_guard = shared.client.write().await;
        let (server_stream, mut client_stream) = UnixStream::pair().unwrap();
        let server_shared = Arc::clone(&shared);
        let server =
            tokio::spawn(async move { serve_connection(server_stream, server_shared).await });

        let mut buffer = Vec::new();
        for _ in 0..2 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }

        for id in 0..35 {
            let request = serde_json::to_vec(&ClientFrame::new(
                Some(id),
                Command::ListChats { limit: 10 },
            ))
            .unwrap();
            client_stream.write_all(&request).await.unwrap();
            client_stream.write_all(b"\n").await.unwrap();
        }

        buffer.clear();
        let response = read_test_frame(&mut client_stream, &mut buffer).await;
        assert_eq!(
            response.event,
            ServerEvent::Error {
                message: "too many queued requests".into(),
            }
        );

        drop(client_guard);
        for _ in 1..35 {
            buffer.clear();
            read_test_frame(&mut client_stream, &mut buffer).await;
        }
        drop(client_stream);
        assert!(server.await.unwrap().is_ok());
    }

    async fn read_test_frame(stream: &mut UnixStream, buffer: &mut Vec<u8>) -> ServerFrame {
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

    #[tokio::test]
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
        let _ = events.try_recv().unwrap();
        let paths = AppPaths {
            runtime_dir: directory.path().join("runtime"),
            state_dir: directory.path().to_path_buf(),
            socket: directory.path().join("runtime/daemon.sock"),
            protocol_db: directory.path().join("session.db"),
            history_db: directory.path().join("history.db"),
        };

        clear_local_account_data(&paths, &shared).await.unwrap();

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

    #[tokio::test]
    async fn listing_avatars_is_a_snapshot_without_changed_jids() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        assets::private_dir(&shared.avatar_dir).unwrap();
        let jid = "1@s.whatsapp.net";
        assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, jid), b"avatar")
            .unwrap();

        let event = handle_command(Command::ListAvatars, &shared).await.unwrap();

        assert_eq!(
            event,
            ServerEvent::Avatars {
                revision: 0,
                jids: vec![jid.into()],
                changed_jids: Vec::new(),
            }
        );
    }

    #[test]
    fn avatar_events_only_revise_the_files_that_changed() {
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
        assert_eq!(shared.avatar_snapshot(), vec![first_jid, second_jid]);
        shared.avatars_changed();
        assert_eq!(
            events.try_recv().unwrap().event,
            ServerEvent::Avatars {
                revision: 3,
                jids: vec![first_jid.into(), second_jid.into()],
                changed_jids: vec![first_jid.into()],
            }
        );

        shared.avatars_changed();
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn history_reaction_targets_parent_without_becoming_a_message() {
        let wire = wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                remote_jid: Some("123-456@g.us".into()),
                from_me: Some(false),
                id: Some("reaction-envelope".into()),
                participant: Some("2:5@s.whatsapp.net".into()),
            }),
            // This deliberately uses a wrapper order that get_base_message()
            // does not fully peel: ephemeral -> device-sent -> reaction.
            message: MessageField::some(wa::Message {
                ephemeral_message: MessageField::some(wa::message::FutureProofMessage {
                    message: MessageField::some(wa::Message {
                        device_sent_message: MessageField::some(wa::message::DeviceSentMessage {
                            destination_jid: Some("1@s.whatsapp.net".into()),
                            message: MessageField::some(wa::Message {
                                reaction_message: MessageField::some(
                                    wa::message::ReactionMessage {
                                        key: MessageField::some(wa::MessageKey {
                                            remote_jid: Some("123-456@g.us".into()),
                                            from_me: Some(false),
                                            id: Some("parent-message".into()),
                                            participant: Some("3@s.whatsapp.net".into()),
                                        }),
                                        text: Some("👍".into()),
                                        sender_timestamp_ms: Some(12_345),
                                        ..Default::default()
                                    },
                                ),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let reaction = history_reaction("123-456@g.us", &wire).unwrap();
        assert_eq!(reaction.message_id, "parent-message");
        assert_eq!(reaction.reactor_jid, "2@s.whatsapp.net");
        assert_eq!(reaction.emoji, "👍");
        assert!(!reaction.from_me);
        assert_eq!(reaction.timestamp, 12);
    }

    #[test]
    fn poll_creation_payload_becomes_interactive_ui_media() {
        let message = wa::Message {
            poll_creation_message_v3: MessageField::some(wa::message::PollCreationMessage {
                name: Some("Lunch?".into()),
                options: vec![
                    wa::message::poll_creation_message::Option {
                        option_name: Some("Soup".into()),
                        ..Default::default()
                    },
                    wa::message::poll_creation_message::Option {
                        option_name: Some("Salad".into()),
                        ..Default::default()
                    },
                ],
                selectable_options_count: Some(1),
                poll_type: Some(wa::message::PollType::QUIZ),
                correct_answer: MessageField::some(wa::message::poll_creation_message::Option {
                    option_name: Some("Soup".into()),
                    ..Default::default()
                }),
                end_time: Some(1_700_000_000_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(
            media_text(&message, "poll").as_deref(),
            Some("[Poll] Lunch?")
        );
        let Some(MessageMedia::Poll {
            question,
            options,
            selectable_count,
            quiz,
            correct_option_index,
            end_timestamp,
            ..
        }) = poll_media(&message)
        else {
            panic!("expected poll media");
        };
        assert_eq!(question, "Lunch?");
        assert_eq!(
            options
                .iter()
                .map(|option| option.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Soup", "Salad"]
        );
        assert_eq!(selectable_count, 1);
        assert!(quiz);
        assert_eq!(correct_option_index, Some(0));
        assert_eq!(end_timestamp, 1_700_000_000);
    }

    #[test]
    fn protocol_control_messages_have_no_user_visible_fallback() {
        let control = wa::Message {
            protocol_message: MessageField::some(wa::message::ProtocolMessage::default()),
            ..Default::default()
        };
        assert_eq!(media_text(&control, ""), None);
        assert_eq!(media_placeholder("unknown"), None);
    }

    #[test]
    fn static_animated_and_lottie_stickers_become_structured_media() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media");
        assets::private_dir(&media_dir).unwrap();
        let sticker = wa::message::StickerMessage {
            mimetype: Some("image/webp".into()),
            file_length: Some(1_024),
            width: Some(512),
            height: Some(384),
            is_animated: Some(true),
            png_thumbnail: Some(b"\x89PNG\r\n\x1a\nthumbnail".to_vec()),
            accessibility_label: Some("Dancing parrot".into()),
            ..Default::default()
        };
        let direct = wa::Message {
            sticker_message: MessageField::some(sticker.clone()),
            ..Default::default()
        };
        let direct_media =
            message_media(&direct, &media_dir, "1@s.whatsapp.net", "sticker", 10, 0).unwrap();
        let MessageMedia::Sticker {
            path,
            thumbnail_path,
            downloaded,
            animated,
            lottie,
            accessibility_label,
            width,
            height,
            ..
        } = direct_media
        else {
            panic!("expected sticker media")
        };
        assert!(path.ends_with(".sticker.webp"));
        assert_eq!(
            std::fs::read(thumbnail_path).unwrap(),
            b"\x89PNG\r\n\x1a\nthumbnail"
        );
        assert!(!downloaded);
        assert!(animated);
        assert!(!lottie);
        assert_eq!((width, height), (512, 384));
        assert_eq!(accessibility_label, "Dancing parrot");

        let lottie_message = wa::Message {
            lottie_sticker_message: MessageField::some(wa::message::FutureProofMessage {
                message: MessageField::some(wa::Message {
                    sticker_message: MessageField::some(sticker),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let MessageMedia::Sticker {
            downloaded,
            animated,
            lottie,
            ..
        } = message_media(
            &lottie_message,
            &media_dir,
            "1@s.whatsapp.net",
            "lottie",
            11,
            0,
        )
        .unwrap()
        else {
            panic!("expected Lottie sticker media")
        };
        assert!(!downloaded);
        assert!(animated);
        assert!(lottie);
        assert_eq!(
            media_text(&lottie_message, "sticker"),
            Some("[Sticker]".into())
        );
    }

    #[test]
    fn visual_audio_document_and_live_location_payloads_become_private_ui_media() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media");
        assets::private_dir(&media_dir).unwrap();
        let image = wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                mimetype: Some("image/jpeg".into()),
                width: Some(640),
                height: Some(480),
                jpeg_thumbnail: Some(b"\xff\xd8\xffthumbnail".to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let image_media = message_media(&image, &media_dir, "1@s.whatsapp.net", "photo", 10, 0)
            .expect("image media");
        let MessageMedia::Image {
            path,
            thumbnail_path,
            downloaded,
            mime_type,
            width,
            height,
        } = image_media
        else {
            panic!("expected image media")
        };
        assert_eq!(
            (mime_type.as_str(), width, height),
            ("image/jpeg", 640, 480)
        );
        assert!(!downloaded);
        assert!(!Path::new(&path).exists());
        assert_eq!(
            std::fs::read(&thumbnail_path).unwrap(),
            b"\xff\xd8\xffthumbnail"
        );
        assert_eq!(
            std::fs::metadata(thumbnail_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let video = wa::Message {
            video_message: MessageField::some(wa::message::VideoMessage {
                mimetype: Some("video/mp4".into()),
                file_length: Some(2_000_000),
                seconds: Some(12),
                width: Some(1920),
                height: Some(1080),
                gif_playback: Some(true),
                jpeg_thumbnail: Some(b"\xff\xd8\xffvideo-thumbnail".to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let video_media = message_media(&video, &media_dir, "1@s.whatsapp.net", "clip", 15, 0)
            .expect("video media");
        let MessageMedia::Video {
            path,
            thumbnail_path,
            downloaded,
            mime_type,
            width,
            height,
            duration_seconds,
            gif_playback,
        } = video_media
        else {
            panic!("expected video media")
        };
        assert_eq!(
            (mime_type.as_str(), width, height, duration_seconds),
            ("video/mp4", 1920, 1080, 12)
        );
        assert!(!downloaded);
        assert!(gif_playback);
        assert_eq!(media_text(&video, ""), Some("[GIF]".to_owned()));
        assert!(!Path::new(&path).exists());
        assert_eq!(
            std::fs::read(thumbnail_path).unwrap(),
            b"\xff\xd8\xffvideo-thumbnail"
        );

        let audio = wa::Message {
            audio_message: MessageField::some(wa::message::AudioMessage {
                mimetype: Some("audio/ogg; codecs=opus".into()),
                file_length: Some(120_000),
                seconds: Some(18),
                ptt: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let audio_media = message_media(&audio, &media_dir, "1@s.whatsapp.net", "note", 18, 0)
            .expect("audio media");
        let MessageMedia::Audio {
            path,
            downloaded,
            mime_type,
            duration_seconds,
            voice_message,
        } = audio_media
        else {
            panic!("expected audio media")
        };
        assert_eq!(mime_type, "audio/ogg; codecs=opus");
        assert_eq!(duration_seconds, 18);
        assert!(voice_message);
        assert!(!downloaded);
        assert!(path.ends_with(".audio.ogg"));
        assert_eq!(media_text(&audio, ""), Some("[Voice message]".to_owned()));

        let document = wa::Message {
            document_message: MessageField::some(wa::message::DocumentMessage {
                mimetype: Some("application/pdf".into()),
                file_name: Some("Garden quote.pdf".into()),
                file_length: Some(42_000),
                page_count: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            message_media(&document, &media_dir, "1@s.whatsapp.net", "quote", 20, 0,),
            Some(MessageMedia::Document {
                path: assets::message_document_path(
                    &media_dir,
                    "1@s.whatsapp.net",
                    "quote",
                    "Garden quote.pdf"
                )
                .to_string_lossy()
                .into_owned(),
                file_name: "Garden quote.pdf".into(),
                mime_type: "application/pdf".into(),
                file_size: 42_000,
                page_count: 3,
            })
        );

        let live = wa::Message {
            live_location_message: MessageField::some(wa::message::LiveLocationMessage {
                degrees_latitude: Some(52.370_16),
                degrees_longitude: Some(4.895_168),
                accuracy_in_meters: Some(7),
                caption: Some("On my way".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            message_media(&live, &media_dir, "1@s.whatsapp.net", "live", 42, 3_600,),
            Some(MessageMedia::Location {
                latitude_e7: 523_701_600,
                longitude_e7: 48_951_680,
                accuracy_m: 7,
                name: "On my way".into(),
                address: String::new(),
                thumbnail_path: None,
                live: true,
                updated_at: 42,
                duration_seconds: 3_600,
            })
        );
    }

    #[test]
    fn live_location_thumbnail_is_refreshed_in_place() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media");
        assets::private_dir(&media_dir).unwrap();
        let live = |thumbnail: &[u8]| wa::Message {
            live_location_message: MessageField::some(wa::message::LiveLocationMessage {
                degrees_latitude: Some(52.37),
                degrees_longitude: Some(4.89),
                jpeg_thumbnail: Some(thumbnail.to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let first = b"\xff\xd8\xfffirst";
        let second = b"\xff\xd8\xffsecond";
        let first_media = message_media(
            &live(first),
            &media_dir,
            "1@s.whatsapp.net",
            "live",
            10,
            3_600,
        )
        .unwrap();
        let Some(path) = (match first_media {
            MessageMedia::Location { thumbnail_path, .. } => thumbnail_path,
            _ => None,
        }) else {
            panic!("expected live-location thumbnail")
        };
        assert_eq!(std::fs::read(&path).unwrap(), first);

        message_media(
            &live(second),
            &media_dir,
            "1@s.whatsapp.net",
            "live",
            20,
            3_600,
        )
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), second);
    }

    #[test]
    fn contact_resync_only_resets_the_contact_collection_once() {
        let directory = tempfile::tempdir().unwrap();
        let protocol_db = directory.path().join("session.db");
        let marker = directory.path().join("contact-names-v2");
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_state_versions (name TEXT PRIMARY KEY, state_data BLOB);
                 CREATE TABLE app_state_mutation_macs (
                   name TEXT NOT NULL, version INTEGER NOT NULL,
                   index_mac BLOB NOT NULL, value_mac BLOB NOT NULL
                 );
                 CREATE TABLE device (push_name TEXT NOT NULL);
                 INSERT INTO device VALUES ('Own profile name');
                 INSERT INTO app_state_versions VALUES ('critical_block', X'00');
                 INSERT INTO app_state_versions VALUES ('critical_unblock_low', X'01');
                 INSERT INTO app_state_versions VALUES ('regular', X'02');
                 INSERT INTO app_state_mutation_macs VALUES
                   ('critical_block', 1, X'00', X'01');
                 INSERT INTO app_state_mutation_macs VALUES
                   ('critical_unblock_low', 1, X'01', X'02');
                 INSERT INTO app_state_mutation_macs VALUES
                   ('regular', 1, X'03', X'04');",
            )
            .unwrap();
        drop(connection);

        prepare_contact_name_resync(&protocol_db, &marker).unwrap();

        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        let critical_versions: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions
                 WHERE name IN ('critical_block', 'critical_unblock_low')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let regular_versions: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name = 'regular'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(critical_versions, 0);
        assert_eq!(regular_versions, 1);
        let push_name: String = connection
            .query_row("SELECT push_name FROM device", [], |row| row.get(0))
            .unwrap();
        assert!(push_name.is_empty());
        assert!(!marker.exists());

        connection
            .execute(
                "INSERT INTO app_state_versions VALUES ('critical_unblock_low', X'03')",
                [],
            )
            .unwrap();
        drop(connection);
        write_private_marker(&marker).unwrap();
        assert_eq!(
            std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        prepare_contact_name_resync(&protocol_db, &marker).unwrap();
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        let preserved: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name = 'critical_unblock_low'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved, 1);
    }

    #[test]
    fn event_resync_resets_bootstrap_and_regular_collections() {
        let directory = tempfile::tempdir().unwrap();
        let protocol_db = directory.path().join("session.db");
        let marker = directory.path().join("event-state-v5");
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_state_versions (name TEXT PRIMARY KEY, state_data BLOB);
                 CREATE TABLE app_state_mutation_macs (
                   name TEXT NOT NULL, version INTEGER NOT NULL,
                   index_mac BLOB NOT NULL, value_mac BLOB NOT NULL
                 );
                 CREATE TABLE device (push_name TEXT NOT NULL);
                 INSERT INTO device VALUES ('Own profile name');
                 INSERT INTO app_state_versions VALUES ('critical_block', X'00');
                 INSERT INTO app_state_versions VALUES ('regular', X'01');
                 INSERT INTO app_state_versions VALUES ('regular_low', X'02');
                 INSERT INTO app_state_versions VALUES ('regular_high', X'03');
                 INSERT INTO app_state_mutation_macs VALUES ('critical_block', 1, X'00', X'01');
                 INSERT INTO app_state_mutation_macs VALUES ('regular_low', 1, X'01', X'02');",
            )
            .unwrap();
        drop(connection);

        prepare_event_state_resync(&protocol_db, &marker).unwrap();
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        let regular: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name LIKE 'regular%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let critical: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name = 'critical_block'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(regular, 0);
        assert_eq!(critical, 0);
        let push_name: String = connection
            .query_row("SELECT push_name FROM device", [], |row| row.get(0))
            .unwrap();
        assert!(push_name.is_empty());
    }

    #[test]
    fn history_sync_populates_initial_chat_list() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let jid = "31612345678@s.whatsapp.net";
        let history = wa::HistorySync {
            sync_type: wa::history_sync::HistorySyncType::INITIAL_BOOTSTRAP,
            conversations: vec![wa::Conversation {
                id: jid.into(),
                name: Some("Ada".into()),
                unread_count: Some(1),
                conversation_timestamp: Some(1_700_000_000),
                messages: vec![wa::HistorySyncMsg {
                    message: MessageField::some(wa::WebMessageInfo {
                        key: MessageField::some(wa::MessageKey {
                            remote_jid: Some(jid.into()),
                            from_me: Some(false),
                            id: Some("MSG-1".into()),
                            ..Default::default()
                        }),
                        message: MessageField::some(wa::Message::text("hello from history")),
                        message_timestamp: Some(1_700_000_000),
                        push_name: Some("Ada".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let raw = history.encode_to_vec();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        let lazy = whatsapp_rust::types::events::LazyHistorySync::new(
            encoder.finish().unwrap().into(),
            raw.len(),
            wa::history_sync::HistorySyncType::INITIAL_BOOTSTRAP as i32,
            None,
            Some(100),
        );

        shared.ingest_history(&lazy, None).unwrap();

        let chats = shared.database.list_chats(10).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].name, "Ada");
        assert_eq!(chats[0].unread, 1);
        assert_eq!(chats[0].last_message, "hello from history");
        let messages = shared.database.messages(jid, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "hello from history");
        assert_eq!(messages[0].sender_name, "Ada");
    }
}
