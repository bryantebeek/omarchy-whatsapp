#![recursion_limit = "512"]

mod assets;
mod database;
mod event_coverage;
mod notification;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use clap::Parser;
use database::Database;
use futures::StreamExt;
use omarchy_whatsapp_protocol::{
    AppPaths, Chat, ClientFrame, Command, ConnectionStatus, Message, MessageMedia,
    PROTOCOL_VERSION, ServerEvent, ServerFrame,
};
use qrcode::{QrCode, render::svg};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::{error, info, warn};
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::store::DevicePropsOverride;
use whatsapp_rust::wacore_binary::JidExt;

const CHAT_LIST_LIMIT: u32 = 500;

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
    events: broadcast::Sender<ServerFrame>,
    pairing_qr: PathBuf,
    contact_sync_marker: PathBuf,
    contact_history_marker: PathBuf,
    event_sync_marker: PathBuf,
    avatar_dir: PathBuf,
    media_dir: PathBuf,
    avatar_revision: AtomicU64,
    app_state_failed: AtomicBool,
    group_name_sync: Mutex<()>,
    media_recovery_requested: RwLock<HashSet<String>>,
}

#[derive(Clone)]
enum PendingMedia {
    Image {
        image: wa::message::ImageMessage,
        path: PathBuf,
    },
    Document {
        document: wa::message::DocumentMessage,
        path: PathBuf,
    },
}

impl Shared {
    async fn set_status(&self, status: ConnectionStatus) {
        self.write_pairing_qr(&status);
        *self.status.write().await = status.clone();
        let total = self.database.unread_total().unwrap_or(0);
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
        if let Err(error) = write_private_marker(&self.event_sync_marker) {
            warn!(%error, "could not mark app-state event sync complete");
        }
    }

    fn avatars_changed(&self) {
        let revision = self.avatar_revision.fetch_add(1, Ordering::Relaxed) + 1;
        let jids = assets::available_avatar_jids(&self.avatar_dir);
        let _ = self
            .events
            .send(ServerFrame::event(ServerEvent::Avatars { revision, jids }));
    }

    async fn state_event(&self) -> ServerEvent {
        ServerEvent::State {
            status: self.status.read().await.clone(),
            unread_total: self.database.unread_total().unwrap_or(0),
        }
    }

    async fn receive_message(self: &Arc<Self>, context: MessageContext) {
        let info = &context.info;
        if info.source.chat.is_status_broadcast() || info.source.chat.is_newsletter() {
            return;
        }
        let chat_jid = canonical_contact_jid(self, &context.client, &info.source.chat).await;
        let sender_jid = canonical_contact_jid(self, &context.client, &info.source.sender).await;
        let sender_name = nonempty(&info.push_name).unwrap_or_else(|| sender_jid.clone());
        if sender_name != sender_jid
            && let Err(error) = self.database.update_contact_name(&sender_jid, &sender_name)
        {
            warn!(%error, %sender_jid, "could not persist message push name");
        }
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
        let media = message_media(
            base,
            &self.media_dir,
            &chat_jid,
            &info.id,
            info.timestamp.timestamp(),
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
            media,
            reactions: Vec::new(),
        };
        let focused = self.active_chat.read().await.as_deref() == Some(chat_jid.as_str());
        let unread = !message.from_me && !focused;
        let insert_result =
            self.database
                .insert_message(&message, &chat_name, info.source.is_group, unread);
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
                let total = self.database.unread_total().unwrap_or(0);
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
                if let Some(image) = base.image_message.as_option().cloned() {
                    let shared = Arc::clone(self);
                    let client = Arc::clone(&context.client);
                    let path = assets::message_image_path(&self.media_dir, &chat_jid, &info.id);
                    let media_chat_jid = chat_jid.clone();
                    tokio::spawn(async move {
                        match assets::download_message_image(client, image, path).await {
                            Ok(true) => broadcast_messages(&shared, &media_chat_jid),
                            Ok(false) => {}
                            Err(error) => {
                                warn!(%error, chat_jid = %media_chat_jid, "could not cache WhatsApp image")
                            }
                        }
                    });
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
                                warn!(%error, chat_jid = %media_chat_jid, "could not cache WhatsApp document")
                            }
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

    fn ingest_history(
        &self,
        lazy: &whatsapp_rust::types::events::LazyHistorySync,
    ) -> Result<Vec<PendingMedia>> {
        let mut pending_media = Vec::new();
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
                    if pending_media.len() < 128
                        && let Some(base) = wire
                            .message
                            .as_option()
                            .map(|message| message.get_base_message())
                        && let Some(message) = &result
                    {
                        if let Some(image) = base.image_message.as_option().cloned() {
                            pending_media.push(PendingMedia::Image {
                                path: assets::message_image_path(
                                    &self.media_dir,
                                    &chat_jid,
                                    &message.id,
                                ),
                                image,
                            });
                        } else if let Some(document) = base.document_message.as_option().cloned() {
                            let path = assets::message_document_path(
                                &self.media_dir,
                                &chat_jid,
                                &message.id,
                                document.file_name.as_deref().unwrap_or_default(),
                            );
                            pending_media.push(PendingMedia::Document { document, path });
                        }
                    }
                    result
                })
                .collect::<Vec<_>>();
            messages.sort_by_key(|message| message.timestamp);
            let last_timestamp = conversation
                .conversation_timestamp
                .or(conversation.last_msg_timestamp)
                .map(|timestamp| timestamp.min(i64::MAX as u64) as i64)
                .or_else(|| messages.last().map(|message| message.timestamp))
                .unwrap_or(0);
            let preview = messages
                .last()
                .map(|message| message.text.clone())
                .unwrap_or_default();
            self.database
                .insert_history_chat(&omarchy_whatsapp_protocol::Chat {
                    jid: chat_jid.clone(),
                    name: chat_name.clone(),
                    phone_number: None,
                    last_message: preview,
                    last_timestamp,
                    unread: conversation.unread_count.unwrap_or(0),
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
    } else if message.video_message.is_set() || message.ptv_message.is_set() {
        Some("[Video]")
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
        Some("[Poll]")
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
    if !path.exists()
        && let Err(error) = assets::write_private_bytes(&path, bytes)
    {
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
) -> Option<MessageMedia> {
    if let Some(image) = message.image_message.as_option() {
        let path = assets::message_image_path(directory, chat_jid, message_id);
        if !path.exists()
            && let Some(thumbnail) = image.jpeg_thumbnail.as_ref()
            && thumbnail.starts_with(&[0xff, 0xd8, 0xff])
            && let Err(error) = assets::write_private_bytes(&path, thumbnail)
        {
            warn!(%error, "could not cache WhatsApp image thumbnail");
        }
        assets::prune_media_cache(directory, &path);
        return Some(MessageMedia::Image {
            path: path.to_string_lossy().into_owned(),
            mime_type: image
                .mimetype
                .clone()
                .unwrap_or_else(|| "image/jpeg".to_owned()),
            width: image.width.unwrap_or(0),
            height: image.height.unwrap_or(0),
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
        });
    }
    None
}

fn normalize_jid(value: &str) -> String {
    value
        .parse::<Jid>()
        .map(|jid| jid.to_non_ad_string())
        .unwrap_or_else(|_| value.to_owned())
}

async fn canonical_contact_jid(shared: &Shared, client: &Client, jid: &Jid) -> String {
    let raw = jid.to_non_ad_string();
    if !jid.is_lid() && !jid.is_pn() {
        return raw;
    }
    let mapping = match client.get_lid_pn_entry(jid).await {
        Ok(Some(mapping)) => mapping,
        Ok(None) => {
            if jid.is_pn() {
                let _ = shared.database.update_chat_phone_number(&raw, &jid.user);
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
                        warn!(%error, %alias, %canonical, "could not preserve aliased WhatsApp avatar")
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

fn metadata_jid(value: &str, server: &str) -> String {
    if value.contains('@') {
        normalize_jid(value)
    } else {
        format!("{value}@{server}")
    }
}

fn broadcast_chats(shared: &Shared) {
    if let Ok(chats) = shared.database.list_chats(CHAT_LIST_LIMIT) {
        let _ = shared
            .events
            .send(ServerFrame::event(ServerEvent::Chats { chats }));
    }
}

fn broadcast_unread(shared: &Shared) {
    let total = shared.database.unread_total().unwrap_or(0);
    let _ = shared
        .events
        .send(ServerFrame::event(ServerEvent::Unread { total }));
}

fn broadcast_messages(shared: &Shared, chat_jid: &str) {
    if let (Ok(messages), Ok(first_unread_message_id)) = (
        shared.database.messages(chat_jid, 300),
        shared.database.first_unread_message_id(chat_jid),
    ) {
        let _ = shared
            .events
            .send(ServerFrame::event(ServerEvent::Messages {
                chat_jid: chat_jid.to_owned(),
                messages,
                first_unread_message_id,
            }));
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
            let unresolved = shared
                .database
                .unresolved_chat_jids(true, 32)
                .unwrap_or_default();
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
                                    warn!(%error, %jid, "could not persist recovered WhatsApp group subject")
                                }
                            }
                        }
                    }
                    Err(error) => {
                        warn!(%error, %jid, "could not recover WhatsApp group subject")
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
    let jids = shared
        .database
        .unresolved_chat_jids(false, 100)
        .unwrap_or_default()
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
                    if shared
                        .database
                        .update_contact_name(&jid, &name)
                        .unwrap_or(false)
                    {
                        updated += 1;
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
    let jids = shared
        .database
        .avatar_jids(500)
        .unwrap_or_default()
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
        async move {
            match pending {
                PendingMedia::Image { image, path } => {
                    assets::download_message_image(client, image, path).await
                }
                PendingMedia::Document { document, path } => {
                    assets::download_message_document(client, document, path).await
                }
            }
            .unwrap_or(false)
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
        if !shared.database.list_chats(1).unwrap_or_default().is_empty()
            && let Err(error) = write_private_marker(&shared.contact_history_marker)
        {
            warn!(%error, "could not mark contact history recovery complete");
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
                warn!(%error, jid = %cursor.chat_jid, "could not request WhatsApp history for contact-name recovery")
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
         WHERE name IN ('regular', 'regular_low', 'regular_high')",
        [],
    )?;
    transaction.execute(
        "DELETE FROM app_state_versions
         WHERE name IN ('regular', 'regular_low', 'regular_high')",
        [],
    )?;
    // A linked device only schedules all non-critical collections during its
    // bootstrap path. Clearing their versions is not sufficient on an already
    // paired session; an empty push name safely re-enters that path and is
    // restored by critical_block before the regular collections are fetched.
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
                .map(|timestamp| timestamp.min(i64::MAX as u64) as i64)
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
        .map(|timestamp| timestamp.min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    let text = base
        .text_content()
        .or_else(|| base.get_caption())
        .map(str::to_owned)
        .or_else(|| media_text(base, ""))?;
    let media = message_media(base, media_dir, chat_jid, &id, timestamp);
    Some(Message {
        id,
        chat_jid: chat_jid.to_owned(),
        sender_jid,
        sender_name,
        text,
        timestamp,
        from_me,
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

const APP_EVENT_KINDS: &[EventKind] = &[
    EventKind::PairError,
    EventKind::QrScannedWithoutMultidevice,
    EventKind::ClientOutdated,
    EventKind::Receipt,
    EventKind::UndecryptableMessage,
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
            let state = match receipt.r#type.as_wire_str() {
                "read" | "read-self" => 3,
                "played" | "played-self" => 4,
                "delivery" => 2,
                "sent" | "sender" => 1,
                _ => 0,
            };
            if state > 0
                && let Err(error) = shared.database.update_receipts(&receipt.message_ids, state)
            {
                warn!(%error, "could not persist WhatsApp receipts");
            }
        }
        Event::UndecryptableMessage(details) => {
            warn!(
                message_id = %details.info.id,
                unavailable = details.is_unavailable,
                "WhatsApp message could not be decrypted; library recovery remains active"
            );
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
            broadcast_chats(&shared);
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
        paths.state_dir = state_dir.clone();
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
    assets::private_dir(&avatar_dir)?;
    assets::private_dir(&media_dir)?;

    let (app_events, library_events, excluded_events) = event_coverage::counts();
    info!(
        app_events,
        library_events, excluded_events, "loaded exhaustive WhatsApp event policy"
    );

    if paths.socket.exists() {
        std::fs::remove_file(&paths.socket)
            .with_context(|| format!("removing stale socket {}", paths.socket.display()))?;
    }
    let listener = UnixListener::bind(&paths.socket)
        .with_context(|| format!("binding {}", paths.socket.display()))?;
    std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))?;

    let (events, _) = broadcast::channel(256);
    let shared = Arc::new(Shared {
        database: Database::open(&paths.history_db)?,
        status: RwLock::new(ConnectionStatus::Starting),
        client: RwLock::new(None),
        active_chat: RwLock::new(None),
        events,
        pairing_qr: paths.runtime_dir.join("pairing.svg"),
        contact_sync_marker: paths.state_dir.join("contact-names-v2"),
        contact_history_marker: paths.state_dir.join("contact-history-names-v1"),
        event_sync_marker: paths.state_dir.join("event-state-v3"),
        avatar_dir,
        media_dir,
        avatar_revision: AtomicU64::new(0),
        app_state_failed: AtomicBool::new(false),
        group_name_sync: Mutex::new(()),
        media_recovery_requested: RwLock::new(HashSet::new()),
    });

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

        let bot = Bot::builder()
            .with_backend(store)
            .with_device_props(
                DevicePropsOverride::new()
                    .with_os("Linux")
                    .with_platform_type(wa::device_props::PlatformType::DESKTOP),
            )
            .on_qr_code(move |code, timeout| {
                let shared = Arc::clone(&qr_shared);
                async move {
                    shared
                        .set_status(ConnectionStatus::Pairing {
                            code,
                            expires_at: Utc::now().timestamp() + timeout.as_secs() as i64,
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
                    let result = tokio::task::spawn_blocking(move || match &*event {
                        Event::HistorySync(history) => ingest_shared.ingest_history(history),
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
                            let total = shared.database.unread_total().unwrap_or(0);
                            let _ = shared
                                .events
                                .send(ServerFrame::event(ServerEvent::Unread { total }));
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
                            match shared.database.update_group_name(
                                &update.group_jid.to_non_ad_string(),
                                &metadata.subject,
                            ) {
                                Ok(true) => broadcast_chats(&shared),
                                Ok(false) => {}
                                Err(error) => {
                                    warn!(%error, "could not update WhatsApp group subject")
                                }
                            }
                        }
                        Err(error) => warn!(%error, "could not refresh WhatsApp group subject"),
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
            _ = &mut bot_handle => true,
            result = &mut ipc_task => {
                result.context("IPC task panicked")??;
                bail!("IPC server stopped unexpectedly");
            }
        };
        if !restart {
            break;
        }
        *shared.client.write().await = None;
        shared
            .set_status(ConnectionStatus::Disconnected {
                reason: "WhatsApp session ended; retrying".to_owned(),
            })
            .await;
        warn!("WhatsApp run loop ended; starting a fresh connection");
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

async fn serve_connection(stream: UnixStream, shared: Arc<Shared>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    // Bound memory even if another process owned by the same user sends a line
    // without a delimiter. Normal commands are only a few kilobytes.
    let mut lines = FramedRead::new(read, LinesCodec::new_with_max_length(128 * 1024));
    let mut events = shared.events.subscribe();
    write_frame(
        &mut write,
        &ServerFrame::event(ServerEvent::Hello {
            protocol_version: PROTOCOL_VERSION,
        }),
    )
    .await?;
    write_frame(&mut write, &ServerFrame::event(shared.state_event().await)).await?;

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
                let event = handle_command(frame.command, &shared).await
                    .unwrap_or_else(|error| ServerEvent::Error { message: error.to_string() });
                write_frame(&mut write, &ServerFrame::response(id, event)).await?;
            }
            event = events.recv() => match event {
                Ok(event) => write_frame(&mut write, &event).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    write_frame(&mut write, &ServerFrame::event(shared.state_event().await)).await?;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
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

async fn handle_command(command: Command, shared: &Arc<Shared>) -> Result<ServerEvent> {
    match command {
        Command::GetState => Ok(shared.state_event().await),
        Command::ListChats { limit } => Ok(ServerEvent::Chats {
            chats: list_chats_with_phone_numbers(shared, limit).await?,
        }),
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
                        tokio::spawn(async move {
                            if let Err(error) = client
                                .fetch_message_history(
                                    &jid,
                                    &cursor.message_id,
                                    cursor.from_me,
                                    cursor.timestamp_ms,
                                    50,
                                )
                                .await
                            {
                                warn!(%error, %jid, "could not request media history recovery");
                            }
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
            jids: assets::available_avatar_jids(&shared.avatar_dir),
        }),
        Command::SetActiveChat { chat_jid } => {
            *shared.active_chat.write().await = match chat_jid {
                Some(chat_jid) => Some(canonical_requested_jid(shared, &chat_jid).await),
                None => None,
            };
            Ok(ServerEvent::Ack)
        }
        Command::Ping => Ok(ServerEvent::Pong),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buffa::{Message as _, MessageField};
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn test_shared(directory: &tempfile::TempDir) -> Shared {
        let (events, _) = broadcast::channel(8);
        Shared {
            database: Database::open(&directory.path().join("history.db")).unwrap(),
            status: RwLock::new(ConnectionStatus::Starting),
            client: RwLock::new(None),
            active_chat: RwLock::new(None),
            events,
            pairing_qr: directory.path().join("pairing.svg"),
            contact_sync_marker: directory.path().join("contact-names-v2"),
            contact_history_marker: directory.path().join("contact-history-names-v1"),
            event_sync_marker: directory.path().join("event-state-v3"),
            avatar_dir: directory.path().join("avatars"),
            media_dir: directory.path().join("media"),
            avatar_revision: AtomicU64::new(0),
            app_state_failed: AtomicBool::new(false),
            group_name_sync: Mutex::new(()),
            media_recovery_requested: RwLock::new(HashSet::new()),
        }
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
    fn protocol_control_messages_have_no_user_visible_fallback() {
        let control = wa::Message {
            protocol_message: MessageField::some(wa::message::ProtocolMessage::default()),
            ..Default::default()
        };
        assert_eq!(media_text(&control, ""), None);
        assert_eq!(media_placeholder("unknown"), None);
    }

    #[test]
    fn image_document_and_live_location_payloads_become_private_ui_media() {
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
        let image_media = message_media(&image, &media_dir, "1@s.whatsapp.net", "photo", 10)
            .expect("image media");
        let MessageMedia::Image {
            path,
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
        assert_eq!(std::fs::read(&path).unwrap(), b"\xff\xd8\xffthumbnail");
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );

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
            message_media(&document, &media_dir, "1@s.whatsapp.net", "quote", 20),
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
            message_media(&live, &media_dir, "1@s.whatsapp.net", "live", 42),
            Some(MessageMedia::Location {
                latitude_e7: 523_701_600,
                longitude_e7: 48_951_680,
                accuracy_m: 7,
                name: "On my way".into(),
                address: String::new(),
                thumbnail_path: None,
                live: true,
                updated_at: 42,
            })
        );
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
    fn event_resync_resets_only_regular_collections() {
        let directory = tempfile::tempdir().unwrap();
        let protocol_db = directory.path().join("session.db");
        let marker = directory.path().join("event-state-v3");
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
        assert_eq!(critical, 1);
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

        shared.ingest_history(&lazy).unwrap();

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
