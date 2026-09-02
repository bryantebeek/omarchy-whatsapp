// Inbound message modelling: the live reduction adapter and the pure decoders
// that turn WhatsApp protobuf payloads into the local UI model.

use crate::history::option_names_for_hashes;
use crate::identity::canonical_contact_jid;
use crate::state::{Shared, broadcast_messages};
use crate::transport::Transport;
use crate::util::nonempty;
use crate::{assets, database, notification};
use buffa::Message as _;
use chrono::Utc;
use omarchy_whatsapp_protocol::{Message, MessageMedia, PollOption, ServerEvent};
use std::path::Path;
use std::sync::Arc;
use tracing::{error, warn};
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

impl Shared {
    // Adapter from upstream protobuf contexts into the durable local model.
    pub(crate) async fn receive_message(
        self: &Arc<Self>,
        generation: u64,
        message: Arc<wa::Message>,
        info: MessageInfo,
        transport: Arc<dyn Transport>,
    ) -> bool {
        let info = &info;
        if info.source.chat.is_status_broadcast() || info.source.chat.is_newsletter() {
            return true;
        }
        let chat_jid = canonical_contact_jid(self, transport.as_ref(), &info.source.chat).await;
        let sender_jid = canonical_contact_jid(self, transport.as_ref(), &info.source.sender).await;
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
                _ => transport
                    .group_metadata(&info.source.chat)
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
        if !self.clock.is_current(generation) {
            return false;
        }
        if let Some(reaction) = find_reaction_message(&message)
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
            let persisted = match self.database.apply_reaction(
                &chat_jid,
                message_id,
                reactor_jid,
                emoji,
                info.source.is_from_me,
                timestamp,
            ) {
                Ok(true) => {
                    broadcast_messages(self, &chat_jid);
                    true
                }
                Ok(false) => true,
                Err(error) => {
                    warn!(%error, %chat_jid, %message_id, "could not persist reaction");
                    false
                }
            };
            return persisted;
        }
        let base = message.get_base_message();
        if let Some(update) = base.poll_update_message.as_option() {
            let Some(target_id) = update
                .poll_creation_message_key
                .as_option()
                .and_then(|key| key.id.as_deref())
            else {
                warn!(message_id = %info.id, "poll vote is missing its parent message ID");
                return true;
            };
            let stored = match self.database.poll_for_voting(&chat_jid, target_id) {
                Ok(Some(stored)) => stored,
                Ok(None) => {
                    warn!(%chat_jid, poll_message_id = %target_id,
                        "could not apply poll vote because the parent poll is unavailable");
                    return false;
                }
                Err(error) => {
                    warn!(%error, %chat_jid, poll_message_id = %target_id,
                        "could not load parent poll for incoming vote");
                    return false;
                }
            };
            let Some(vote) = update.vote.as_option() else {
                return true;
            };
            let (Some(enc_payload), Some(enc_iv)) =
                (vote.enc_payload.as_deref(), vote.enc_iv.as_deref())
            else {
                warn!(%chat_jid, poll_message_id = %target_id,
                    "incoming poll vote has no encrypted payload");
                return true;
            };
            let creator = match stored.creator_jid.parse::<Jid>() {
                Ok(creator) => creator,
                Err(error) => {
                    warn!(%error, creator_jid = %stored.creator_jid,
                        "stored poll creator JID is invalid");
                    return true;
                }
            };
            let raw_voter = info.source.sender.to_non_ad();
            let hashes = match transport
                .decrypt_poll_vote(
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
                    return true;
                }
            };
            let Some(selected_options) = option_names_for_hashes(&stored.options, &hashes) else {
                warn!(%chat_jid, poll_message_id = %target_id,
                    "incoming poll vote references an unknown option");
                return true;
            };
            if !self.clock.is_current(generation) {
                return false;
            }
            let poll_timestamp = update
                .sender_timestamp_ms
                .unwrap_or_else(|| info.timestamp.timestamp_millis());
            let voter_key = if info.source.is_from_me {
                "me"
            } else {
                sender_jid.as_str()
            };
            let persisted = match self.database.apply_poll_vote(
                &chat_jid,
                target_id,
                voter_key,
                &selected_options,
                info.source.is_from_me,
                poll_timestamp,
            ) {
                Ok(true) => {
                    broadcast_messages(self, &chat_jid);
                    true
                }
                Ok(false) => true,
                Err(error) => {
                    warn!(%error, %chat_jid, poll_message_id = %target_id,
                        "could not persist incoming poll vote");
                    false
                }
            };
            return persisted;
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
            return true;
        };
        let persisted_message = Message {
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
        if !self.clock.is_current(generation) {
            return false;
        }
        let focused = self.chat_is_focused(&chat_jid);
        let unread = !persisted_message.from_me && !focused;
        let insert_result = if focused && !persisted_message.from_me {
            self.database.insert_message_with_read_intent(
                &persisted_message,
                &chat_name,
                info.source.is_group,
                &database::UnreadReceipt {
                    message_id: persisted_message.id.clone(),
                    sender_jid: persisted_message.sender_jid.clone(),
                    is_group: info.source.is_group,
                },
            )
        } else {
            self.database.insert_message(
                &persisted_message,
                &chat_name,
                info.source.is_group,
                unread,
            )
        };
        if insert_result.is_ok() && focused && !persisted_message.from_me {
            self.read_outbox_notify.notify_one();
        }
        if insert_result.is_ok()
            && matches!(persisted_message.media, Some(MessageMedia::Poll { .. }))
            && let Some(secret) = message_secret(&message, base)
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
                self.publish(ServerEvent::Message {
                    message: persisted_message.clone(),
                });
                let total = self.unread_total_or_zero();
                self.publish(ServerEvent::Unread { total });
                if unread
                    && !info.is_offline
                    && !self
                        .database
                        .is_muted(&chat_jid, Utc::now().timestamp())
                        .unwrap_or(false)
                {
                    notification::send(&persisted_message, &chat_name, info.source.is_group);
                }
                if let Some(document) = base.document_message.as_option().cloned() {
                    let shared = Arc::clone(self);
                    let transport = Arc::clone(&transport);
                    let path = assets::message_document_path(
                        &self.media_dir,
                        &chat_jid,
                        &info.id,
                        document.file_name.as_deref().unwrap_or_default(),
                    );
                    let media_chat_jid = chat_jid.clone();
                    tokio::spawn(async move {
                        match assets::download_message_document(transport, document, path).await {
                            Ok(true) if shared.clock.is_current(generation) => {
                                broadcast_messages(&shared, &media_chat_jid);
                            }
                            Ok(_) => {}
                            Err(error) => {
                                warn!(%error, chat_jid = %media_chat_jid, "could not cache WhatsApp document");
                            }
                        }
                    });
                }
                true
            }
            Ok(false) => {
                if let Some(media) = &persisted_message.media
                    && self
                        .database
                        .update_message_media(&chat_jid, &persisted_message.id, media)
                        .unwrap_or(false)
                {
                    broadcast_messages(self, &chat_jid);
                }
                true
            }
            Err(error) => {
                error!(%error, "could not persist incoming message");
                false
            }
        }
    }
}

pub(crate) fn find_reaction_message(
    message: &wa::Message,
) -> Option<&wa::message::ReactionMessage> {
    find_reaction_message_at_depth(message, 0)
}

fn find_reaction_message_at_depth(
    message: &wa::Message,
    depth: u8,
) -> Option<&wa::message::ReactionMessage> {
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
        .find_map(|inner| find_reaction_message_at_depth(inner, depth + 1))
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

pub(crate) fn media_text(message: &wa::Message, fallback_type: &str) -> Option<String> {
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

pub(crate) fn video_message(message: &wa::Message) -> Option<&wa::message::VideoMessage> {
    message
        .video_message
        .as_option()
        .or_else(|| message.ptv_message.as_option())
}

pub(crate) fn sticker_message(
    message: &wa::Message,
) -> Option<(&wa::message::StickerMessage, bool)> {
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

pub(crate) fn poll_media(message: &wa::Message) -> Option<MessageMedia> {
    let poll = poll_creation_message(message)?;
    let options: Vec<PollOption> = poll
        .options
        .iter()
        .filter_map(|option| option.option_name.as_deref().and_then(nonempty))
        .map(|name| PollOption {
            name,
            votes: 0,
            selected_by_me: false,
            voter_jids: Vec::new(),
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
    let maximum_selectable = u32::try_from(options.len()).unwrap_or(u32::MAX);
    // WhatsApp clients encode multiple-answer polls either with the explicit
    // limit or with zero, including v3 messages produced outside WA Web.
    // A missing count remains the backwards-compatible single-answer default.
    let selectable_count = match poll.selectable_options_count {
        Some(0) => maximum_selectable,
        None => 1,
        Some(count) => count.clamp(1, maximum_selectable),
    };
    Some(MessageMedia::Poll {
        question: poll
            .name
            .as_deref()
            .and_then(nonempty)
            .unwrap_or_else(|| "Poll".to_owned()),
        selectable_count,
        options,
        total_voters: 0,
        quiz: poll.poll_type == Some(wa::message::PollType::QUIZ),
        correct_option_index,
        end_timestamp,
    })
}

pub(crate) fn message_secret<'a>(
    outer: &'a wa::Message,
    base: &'a wa::Message,
) -> Option<&'a [u8]> {
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
    if !unchanged {
        if let Err(error) = assets::write_private_bytes(&path, bytes) {
            warn!(%error, "could not cache WhatsApp location thumbnail");
            return None;
        }
        // Only a real write may pay for a full cache scan.
        assets::prune_media_cache(directory, &path);
    }
    Some(path.to_string_lossy().into_owned())
}

pub(crate) fn message_media(
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::default_trait_access)] // Generated protobuf fixture types are inferred by MessageField.
mod tests {
    use super::*;
    use crate::test_support::test_shared;
    use crate::transport::fake::{Call, CallKind, FakeTransport, MediaKind};
    use buffa::MessageField;
    use chrono::TimeZone;
    use std::os::unix::fs::PermissionsExt;
    use whatsapp_rust::wacore::types::message::MessageSource;

    const CHAT: &str = "31600000001@s.whatsapp.net";
    const GROUP: &str = "120363000000000001@g.us";
    const NOW: i64 = 1_700_000_000;

    fn linked(directory: &tempfile::TempDir) -> (Arc<Shared>, u64, Arc<FakeTransport>) {
        let shared = Arc::new(test_shared(directory));
        assets::private_dir(&shared.media_dir).unwrap();
        let generation = shared.clock.begin_generation();
        (shared, generation, Arc::new(FakeTransport::new()))
    }

    fn wire(fake: &Arc<FakeTransport>) -> Arc<dyn Transport> {
        crate::transport::fake::transport(fake)
    }

    fn info_at(chat: &str, sender: &str, id: &str, timestamp: i64) -> MessageInfo {
        MessageInfo {
            source: MessageSource {
                chat: chat.parse().unwrap(),
                sender: sender.parse().unwrap(),
                is_group: chat.ends_with("@g.us"),
                ..MessageSource::default()
            },
            id: id.to_owned(),
            timestamp: Utc.timestamp_opt(timestamp, 0).unwrap(),
            ..MessageInfo::default()
        }
    }

    fn info(chat: &str, id: &str) -> MessageInfo {
        info_at(chat, chat, id, NOW)
    }

    fn stored_message(id: &str) -> Message {
        Message {
            id: id.to_owned(),
            chat_jid: CHAT.into(),
            sender_jid: CHAT.into(),
            sender_name: "Ada".into(),
            text: "parent".into(),
            timestamp: NOW,
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

    fn reaction(target: &str, emoji: &str, sender_timestamp_ms: Option<i64>) -> Arc<wa::Message> {
        Arc::new(wa::Message {
            reaction_message: MessageField::some(wa::message::ReactionMessage {
                key: MessageField::some(wa::MessageKey {
                    id: Some(target.to_owned()),
                    ..Default::default()
                }),
                text: Some(emoji.to_owned()),
                sender_timestamp_ms,
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    fn poll_option(name: &str) -> wa::message::poll_creation_message::Option {
        wa::message::poll_creation_message::Option {
            option_name: Some(name.to_owned()),
            ..Default::default()
        }
    }

    fn poll_creation(secret: Vec<u8>) -> Arc<wa::Message> {
        Arc::new(wa::Message {
            poll_creation_message_v3: MessageField::some(wa::message::PollCreationMessage {
                name: Some("Lunch?".into()),
                options: vec![poll_option("Soup"), poll_option("Salad")],
                selectable_options_count: Some(1),
                ..Default::default()
            }),
            message_context_info: MessageField::some(wa::MessageContextInfo {
                message_secret: Some(secret),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    /// One incoming vote's `encPayload`/`encIv` pair, absent when the update
    /// carried no `vote` at all.
    type Ciphertext = Option<(Option<Vec<u8>>, Option<Vec<u8>>)>;

    fn poll_vote(
        target: Option<&str>,
        ciphertext: Ciphertext,
        sender_timestamp_ms: Option<i64>,
    ) -> Arc<wa::Message> {
        Arc::new(wa::Message {
            poll_update_message: MessageField::some(wa::message::PollUpdateMessage {
                poll_creation_message_key: target.map_or_else(MessageField::none, |id| {
                    MessageField::some(wa::MessageKey {
                        id: Some(id.to_owned()),
                        ..Default::default()
                    })
                }),
                vote: ciphertext.map_or_else(MessageField::none, |(enc_payload, enc_iv)| {
                    MessageField::some(wa::message::PollEncValue {
                        enc_payload,
                        enc_iv,
                    })
                }),
                sender_timestamp_ms,
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    fn soup_hashes() -> Vec<Vec<u8>> {
        vec![whatsapp_rust::wacore::poll::compute_option_hash("Soup").to_vec()]
    }

    /// Seeds one stored poll whose secret and options the vote paths read back.
    async fn seed_poll(shared: &Arc<Shared>, generation: u64, fake: &Arc<FakeTransport>) {
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_creation(vec![7; 32]),
                    info(CHAT, "POLL-1"),
                    wire(fake),
                )
                .await
        );
        assert!(
            shared
                .database
                .poll_for_voting(CHAT, "POLL-1")
                .unwrap()
                .is_some()
        );
    }

    fn stored_votes(shared: &Arc<Shared>) -> Vec<PollOption> {
        match shared.database.messages(CHAT, 10).unwrap()[0].media.clone() {
            Some(MessageMedia::Poll { options, .. }) => options,
            other => panic!("expected stored poll media, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn broadcast_and_newsletter_chats_never_reach_the_local_model() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);

        for chat in ["status@broadcast", "120363000000000009@newsletter"] {
            assert!(
                shared
                    .receive_message(
                        generation,
                        Arc::new(wa::Message::text("hello")),
                        info(chat, "M-1"),
                        wire(&fake),
                    )
                    .await
            );
        }

        assert!(fake.calls().is_empty());
        assert!(shared.database.list_chats(10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_retired_generation_stops_before_the_message_is_modelled() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);

        assert!(
            !shared
                .receive_message(
                    generation.saturating_add(1),
                    Arc::new(wa::Message::text("hello")),
                    info(CHAT, "M-1"),
                    wire(&fake),
                )
                .await
        );

        assert!(shared.database.messages(CHAT, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn incoming_reactions_are_applied_deduplicated_and_tombstoned() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        shared
            .database
            .insert_message(&stored_message("PARENT-1"), "Ada", false, false)
            .unwrap();

        assert!(
            shared
                .receive_message(
                    generation,
                    reaction("PARENT-1", "👍", Some(1_700_000_500_000)),
                    info(CHAT, "R-1"),
                    wire(&fake),
                )
                .await
        );
        let reactions = shared.database.messages(CHAT, 10).unwrap()[0]
            .reactions
            .clone();
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].emoji, "👍");
        assert_eq!(reactions[0].count, 1);
        assert!(!reactions[0].from_me);

        // The same reaction replayed is idempotent rather than a failure.
        assert!(
            shared
                .receive_message(
                    generation,
                    reaction("PARENT-1", "👍", Some(1_700_000_500_000)),
                    info(CHAT, "R-1"),
                    wire(&fake),
                )
                .await
        );

        // An own reaction without a sender timestamp falls back to the envelope.
        let mut own = info_at(CHAT, CHAT, "R-2", 1_700_000_600);
        own.source.is_from_me = true;
        assert!(
            shared
                .receive_message(
                    generation,
                    reaction("PARENT-1", "❤", None),
                    own,
                    wire(&fake)
                )
                .await
        );
        assert!(
            shared.database.messages(CHAT, 10).unwrap()[0]
                .reactions
                .iter()
                .any(|entry| entry.emoji == "❤" && entry.from_me)
        );

        // A removal tombstones the reaction, and an older one stays blocked.
        assert!(
            shared
                .receive_message(
                    generation,
                    reaction("PARENT-1", "", Some(1_700_000_900_000)),
                    info(CHAT, "R-3"),
                    wire(&fake),
                )
                .await
        );
        assert!(
            shared
                .receive_message(
                    generation,
                    reaction("PARENT-1", "🎉", Some(1_700_000_800_000)),
                    info(CHAT, "R-4"),
                    wire(&fake),
                )
                .await
        );
        assert!(
            !shared.database.messages(CHAT, 10).unwrap()[0]
                .reactions
                .iter()
                .any(|entry| entry.emoji == "👍" || entry.emoji == "🎉")
        );
    }

    #[tokio::test]
    async fn an_unstorable_reaction_is_not_acknowledged() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        shared
            .database
            .execute_test_sql(
                "CREATE TRIGGER block_reaction BEFORE INSERT ON reactions
                 WHEN NEW.emoji = '💥'
                 BEGIN SELECT RAISE(ABORT, 'synthetic reaction failure'); END;",
            )
            .unwrap();

        assert!(
            !shared
                .receive_message(
                    generation,
                    reaction("PARENT-1", "💥", Some(1_700_000_500_000)),
                    info(CHAT, "R-1"),
                    wire(&fake),
                )
                .await
        );
    }

    #[tokio::test]
    async fn incoming_poll_votes_are_validated_before_they_are_applied() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        seed_poll(&shared, generation, &fake).await;

        // A vote without a parent key is acknowledged and dropped.
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(None, None, None),
                    info(CHAT, "V-0"),
                    wire(&fake)
                )
                .await
        );
        // An unknown parent poll is retried later instead of acknowledged.
        assert!(
            !shared
                .receive_message(
                    generation,
                    poll_vote(Some("MISSING"), None, None),
                    info(CHAT, "V-1"),
                    wire(&fake),
                )
                .await
        );
        // A vote whose encrypted payload never arrived is dropped.
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(Some("POLL-1"), None, None),
                    info(CHAT, "V-2"),
                    wire(&fake),
                )
                .await
        );
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(Some("POLL-1"), Some((None, Some(b"iv".to_vec()))), None),
                    info(CHAT, "V-3"),
                    wire(&fake),
                )
                .await
        );

        // A vote WhatsApp cannot decrypt is dropped rather than retried.
        fake.fail(CallKind::DecryptPollVote, "synthetic decrypt failure");
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(
                        Some("POLL-1"),
                        Some((Some(b"payload".to_vec()), Some(b"iv".to_vec()))),
                        None,
                    ),
                    info(CHAT, "V-4"),
                    wire(&fake),
                )
                .await
        );
        fake.succeed(CallKind::DecryptPollVote);

        // A decrypted vote for an option the stored poll does not know is dropped.
        *fake.poll_vote_hashes.lock().unwrap() = vec![vec![0; 32]];
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(
                        Some("POLL-1"),
                        Some((Some(b"payload".to_vec()), Some(b"iv".to_vec()))),
                        None,
                    ),
                    info(CHAT, "V-5"),
                    wire(&fake),
                )
                .await
        );
        assert!(stored_votes(&shared).iter().all(|option| option.votes == 0));

        // A stored poll whose creator JID cannot be parsed is dropped.
        shared
            .database
            .execute_test_sql("UPDATE poll_secrets SET creator_jid = 'not a jid'")
            .unwrap();
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(
                        Some("POLL-1"),
                        Some((Some(b"payload".to_vec()), Some(b"iv".to_vec()))),
                        None,
                    ),
                    info(CHAT, "V-6"),
                    wire(&fake),
                )
                .await
        );

        // An unreadable poll store is retried rather than acknowledged.
        shared
            .database
            .execute_test_sql("DROP TABLE poll_secrets")
            .unwrap();
        assert!(
            !shared
                .receive_message(
                    generation,
                    poll_vote(
                        Some("POLL-1"),
                        Some((Some(b"payload".to_vec()), Some(b"iv".to_vec()))),
                        None,
                    ),
                    info(CHAT, "V-7"),
                    wire(&fake),
                )
                .await
        );
    }

    #[tokio::test]
    async fn a_decrypted_poll_vote_updates_the_stored_tally() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        seed_poll(&shared, generation, &fake).await;
        *fake.poll_vote_hashes.lock().unwrap() = soup_hashes();
        let ciphertext = Some((Some(b"payload".to_vec()), Some(b"iv".to_vec())));

        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(Some("POLL-1"), ciphertext.clone(), Some(1_700_000_500_000)),
                    info(CHAT, "V-1"),
                    wire(&fake),
                )
                .await
        );
        let options = stored_votes(&shared);
        assert_eq!(options[0].name, "Soup");
        assert_eq!(options[0].votes, 1);
        assert_eq!(options[0].voter_jids, vec![CHAT.to_owned()]);
        assert_eq!(
            fake.calls_of(CallKind::DecryptPollVote)
                .into_iter()
                .map(|call| match call {
                    Call::DecryptPollVote {
                        message_secret,
                        poll_message_id,
                        creator_jid,
                        ..
                    } => (message_secret, poll_message_id, creator_jid),
                    other => panic!("unexpected call {other:?}"),
                })
                .collect::<Vec<_>>(),
            vec![(vec![7; 32], "POLL-1".to_owned(), CHAT.to_owned())]
        );

        // Replaying the same vote is idempotent.
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(Some("POLL-1"), ciphertext.clone(), Some(1_700_000_500_000)),
                    info(CHAT, "V-1"),
                    wire(&fake),
                )
                .await
        );
        assert_eq!(stored_votes(&shared)[0].votes, 1);

        // An own vote without a sender timestamp is attributed to this device.
        let mut own = info_at(CHAT, CHAT, "V-2", 1_700_000_600);
        own.source.is_from_me = true;
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_vote(Some("POLL-1"), ciphertext.clone(), None),
                    own.clone(),
                    wire(&fake),
                )
                .await
        );
        assert!(stored_votes(&shared)[0].selected_by_me);

        // A vote the database refuses is retried instead of acknowledged.
        shared
            .database
            .execute_test_sql(
                "CREATE TRIGGER block_vote BEFORE INSERT ON poll_votes
                 WHEN NEW.voter_jid = 'me'
                 BEGIN SELECT RAISE(ABORT, 'synthetic vote failure'); END;",
            )
            .unwrap();
        let mut later = own;
        later.timestamp = Utc.timestamp_opt(1_700_000_700, 0).unwrap();
        assert!(
            !shared
                .receive_message(
                    generation,
                    poll_vote(Some("POLL-1"), ciphertext, None),
                    later,
                    wire(&fake),
                )
                .await
        );
    }

    #[tokio::test]
    async fn a_generation_retired_while_decrypting_a_vote_discards_it() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        seed_poll(&shared, generation, &fake).await;
        *fake.poll_vote_hashes.lock().unwrap() = soup_hashes();
        let clock = Arc::clone(&shared.clock);
        fake.on_call(move |kind| {
            if kind == CallKind::DecryptPollVote {
                clock.retire_generation(generation);
            }
        });

        assert!(
            !shared
                .receive_message(
                    generation,
                    poll_vote(
                        Some("POLL-1"),
                        Some((Some(b"payload".to_vec()), Some(b"iv".to_vec()))),
                        Some(1_700_000_500_000),
                    ),
                    info(CHAT, "V-1"),
                    wire(&fake),
                )
                .await
        );

        assert!(stored_votes(&shared).iter().all(|option| option.votes == 0));
    }

    #[tokio::test]
    async fn control_messages_without_renderable_text_are_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message {
                        protocol_message: MessageField::some(Default::default()),
                        ..Default::default()
                    }),
                    info(CHAT, "CTRL-1"),
                    wire(&fake),
                )
                .await
        );

        assert!(shared.database.messages(CHAT, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unfocused_message_is_stored_unread_and_announced() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let mut events = shared.events.subscribe();
        let mut incoming = info(CHAT, "M-1");
        incoming.push_name = "Ada".into();

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("hello there")),
                    incoming,
                    wire(&fake),
                )
                .await
        );

        let published = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert!(published.iter().any(|event| matches!(
            event,
            ServerEvent::Message { message } if message.text == "hello there"
                && message.sender_name == "Ada"
        )));
        assert!(
            published
                .iter()
                .any(|event| matches!(event, ServerEvent::Unread { total: 1 }))
        );
        let chats = shared.database.list_chats(10).unwrap();
        assert_eq!(chats[0].name, "Ada");
        assert_eq!(chats[0].unread, 1);
        assert_eq!(
            shared.database.contact_name(CHAT).unwrap().as_deref(),
            Some("Ada")
        );
    }

    #[tokio::test]
    async fn a_focused_message_queues_a_read_receipt_instead_of_unread() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let connection = shared.open_connection();
        shared
            .set_connection_active_chat(connection, Some(CHAT.to_owned()))
            .unwrap();

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("focused")),
                    info(CHAT, "M-1"),
                    wire(&fake),
                )
                .await
        );

        assert_eq!(shared.database.list_chats(10).unwrap()[0].unread, 0);
        assert_eq!(
            shared.database.next_read_batch().unwrap().unwrap().receipts[0].message_id,
            "M-1"
        );
    }

    #[tokio::test]
    async fn muted_offline_and_own_messages_are_stored_without_a_notification() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        shared.database.apply_mute(CHAT, true, 0).unwrap();

        let mut offline = info_at(CHAT, CHAT, "M-1", NOW);
        offline.is_offline = true;
        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("offline replay")),
                    offline,
                    wire(&fake),
                )
                .await
        );

        let mut muted = info_at(CHAT, CHAT, "M-2", NOW + 1);
        muted.push_name = "Ada".into();
        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("muted")),
                    muted,
                    wire(&fake),
                )
                .await
        );

        let mut own = info_at(CHAT, CHAT, "M-3", NOW + 2);
        own.source.is_from_me = true;
        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("from this device")),
                    own,
                    wire(&fake),
                )
                .await
        );

        let messages = shared.database.messages(CHAT, 10).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].receipt, 1);
        assert_eq!(shared.database.list_chats(10).unwrap()[0].unread, 2);
    }

    #[tokio::test]
    async fn a_replayed_message_refreshes_only_its_media() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let image = Arc::new(wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                mimetype: Some("image/jpeg".into()),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::clone(&image),
                    info(CHAT, "IMG-1"),
                    wire(&fake),
                )
                .await
        );
        assert!(matches!(
            shared.database.messages(CHAT, 10).unwrap()[0].media,
            Some(MessageMedia::Image {
                downloaded: false,
                ..
            })
        ));

        // The cached file arriving later is the only change a replay applies.
        std::fs::write(
            assets::message_image_path(&shared.media_dir, CHAT, "IMG-1"),
            b"image",
        )
        .unwrap();
        assert!(
            shared
                .receive_message(generation, image, info(CHAT, "IMG-1"), wire(&fake))
                .await
        );

        let messages = shared.database.messages(CHAT, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            messages[0].media,
            Some(MessageMedia::Image {
                downloaded: true,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn download_metadata_is_persisted_for_every_media_payload() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let message = Arc::new(wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                mimetype: Some("image/jpeg".into()),
                ..Default::default()
            }),
            video_message: MessageField::some(wa::message::VideoMessage {
                mimetype: Some("video/mp4".into()),
                ..Default::default()
            }),
            audio_message: MessageField::some(wa::message::AudioMessage {
                mimetype: Some("audio/ogg".into()),
                ..Default::default()
            }),
            sticker_message: MessageField::some(wa::message::StickerMessage {
                mimetype: Some("image/webp".into()),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert!(
            shared
                .receive_message(generation, message, info(CHAT, "MEDIA-1"), wire(&fake))
                .await
        );

        assert!(
            !shared
                .database
                .media_download(CHAT, "MEDIA-1")
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            shared.database.messages(CHAT, 10).unwrap()[0].text,
            "[Image]"
        );
    }

    #[tokio::test]
    async fn lottie_stickers_are_recorded_with_their_animation_flag() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        let message = Arc::new(wa::Message {
            lottie_sticker_message: MessageField::some(wa::message::FutureProofMessage {
                message: MessageField::some(wa::Message {
                    sticker_message: MessageField::some(wa::message::StickerMessage::default()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        });

        assert!(
            shared
                .receive_message(generation, message, info(CHAT, "STICKER-1"), wire(&fake))
                .await
        );

        let payload = shared
            .database
            .media_download(CHAT, "STICKER-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            wa::message::StickerMessage::decode_from_slice(&payload)
                .unwrap()
                .is_lottie,
            Some(true)
        );
    }

    #[tokio::test]
    async fn group_subjects_come_from_metadata_and_then_from_the_stored_chat() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, _) = linked(&directory);
        let fake = Arc::new(FakeTransport::new().with_group_metadata(
            GROUP,
            whatsapp_rust::GroupMetadata {
                subject: "Garden".into(),
                ..whatsapp_rust::GroupMetadata::default()
            },
        ));
        let member = "31600000002@s.whatsapp.net";

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("first")),
                    info_at(GROUP, member, "G-1", NOW),
                    wire(&fake),
                )
                .await
        );
        assert_eq!(
            shared.database.chat_name(GROUP).unwrap().as_deref(),
            Some("Garden")
        );

        // The stored subject is reused instead of querying WhatsApp again.
        fake.clear_calls();
        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("second")),
                    info_at(GROUP, member, "G-2", NOW + 1),
                    wire(&fake),
                )
                .await
        );
        assert!(fake.calls_of(CallKind::GroupMetadata).is_empty());
    }

    #[tokio::test]
    async fn an_unavailable_group_subject_falls_back_to_the_chat_identifier() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, _) = linked(&directory);
        let fake = Arc::new(
            FakeTransport::new().failing(CallKind::GroupMetadata, "synthetic metadata failure"),
        );

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("first")),
                    info_at(GROUP, "31600000002@s.whatsapp.net", "G-1", NOW),
                    wire(&fake),
                )
                .await
        );

        assert_eq!(shared.database.chat_name(GROUP).unwrap(), None);
        assert_eq!(
            fake.calls_of(CallKind::GroupMetadata),
            vec![Call::GroupMetadata(GROUP.to_owned())]
        );
    }

    #[tokio::test]
    async fn an_unstorable_group_subject_still_keeps_the_message() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, _) = linked(&directory);
        let fake = Arc::new(FakeTransport::new().with_group_metadata(
            GROUP,
            whatsapp_rust::GroupMetadata {
                subject: "Garden".into(),
                ..whatsapp_rust::GroupMetadata::default()
            },
        ));
        shared
            .database
            .execute_test_sql(
                "CREATE TRIGGER block_group_subject BEFORE UPDATE ON chats
                 WHEN NEW.name_source = 30
                 BEGIN SELECT RAISE(ABORT, 'synthetic subject failure'); END;",
            )
            .unwrap();

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("first")),
                    info_at(GROUP, "31600000002@s.whatsapp.net", "G-1", NOW),
                    wire(&fake),
                )
                .await
        );

        assert_eq!(shared.database.messages(GROUP, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn documents_are_cached_after_the_message_is_stored() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, _) = linked(&directory);
        let fake = Arc::new(FakeTransport::new().with_download_bytes(b"%PDF-1.7 synthetic"));
        let document = |file_length| {
            Arc::new(wa::Message {
                document_message: MessageField::some(wa::message::DocumentMessage {
                    mimetype: Some("application/pdf".into()),
                    file_name: Some("quote.pdf".into()),
                    file_length,
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        let path = assets::message_document_path(&shared.media_dir, CHAT, "DOC-1", "quote.pdf");

        assert!(
            shared
                .receive_message(
                    generation,
                    document(Some(18)),
                    info(CHAT, "DOC-1"),
                    wire(&fake),
                )
                .await
        );
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"%PDF-1.7 synthetic");
        assert_eq!(
            fake.calls_of(CallKind::Download),
            vec![Call::Download(MediaKind::Document)]
        );

        // A cached document is not downloaded twice.
        fake.clear_calls();
        std::fs::write(
            assets::message_document_path(&shared.media_dir, CHAT, "DOC-2", "quote.pdf"),
            b"already cached",
        )
        .unwrap();
        assert!(
            shared
                .receive_message(
                    generation,
                    document(Some(18)),
                    info(CHAT, "DOC-2"),
                    wire(&fake),
                )
                .await
        );
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(fake.calls_of(CallKind::Download).is_empty());

        // A document declaring no size is reported instead of cached.
        assert!(
            shared
                .receive_message(generation, document(None), info(CHAT, "DOC-3"), wire(&fake),)
                .await
        );
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(shared.database.messages(CHAT, 10).unwrap().len(), 3);
    }

    #[tokio::test]
    async fn persistence_failures_degrade_without_acknowledging_the_message() {
        let directory = tempfile::tempdir().unwrap();
        let (shared, generation, fake) = linked(&directory);
        shared
            .database
            .execute_test_sql(
                "CREATE TRIGGER block_contact BEFORE INSERT ON contacts
                 WHEN NEW.name = 'Blocked'
                 BEGIN SELECT RAISE(ABORT, 'synthetic contact failure'); END;",
            )
            .unwrap();
        let mut blocked = info(CHAT, "M-1");
        blocked.push_name = "Blocked".into();

        assert!(
            shared
                .receive_message(
                    generation,
                    Arc::new(wa::Message::text("hello")),
                    blocked,
                    wire(&fake),
                )
                .await
        );
        assert_eq!(
            shared.database.messages(CHAT, 10).unwrap()[0].sender_name,
            "Blocked"
        );

        // A poll secret WhatsApp did not size correctly is reported and skipped.
        assert!(
            shared
                .receive_message(
                    generation,
                    poll_creation(vec![1, 2, 3]),
                    info(CHAT, "POLL-1"),
                    wire(&fake),
                )
                .await
        );
        assert!(
            shared
                .database
                .poll_for_voting(CHAT, "POLL-1")
                .unwrap()
                .is_none()
        );

        // Without the message store nothing can be persisted or acknowledged.
        shared
            .database
            .execute_test_sql("DROP TABLE messages")
            .unwrap();
        let media = Arc::new(wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage::default()),
            video_message: MessageField::some(wa::message::VideoMessage::default()),
            audio_message: MessageField::some(wa::message::AudioMessage::default()),
            sticker_message: MessageField::some(wa::message::StickerMessage::default()),
            ..Default::default()
        });
        assert!(
            !shared
                .receive_message(generation, media, info(CHAT, "MEDIA-1"), wire(&fake))
                .await
        );
    }

    /// The reducer runs while the daemon's run loop may retire the client
    /// generation, so the checkpoint just before the durable write has to be
    /// observed with the clock moving mid-call. A thread-local subscriber
    /// retires it from the one diagnostic the modelling step emits, which keeps
    /// the observation deterministic instead of racing a second thread.
    struct RetireOnDiagnostic {
        clock: Arc<crate::revisions::RevisionClock>,
        generation: u64,
    }

    impl tracing::Subscriber for RetireOnDiagnostic {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, _: &tracing::Event<'_>) {
            self.clock.retire_generation(self.generation);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[tokio::test]
    async fn a_generation_retired_while_modelling_media_discards_the_message() {
        let directory = tempfile::tempdir().unwrap();
        // A thumbnail cache the daemon cannot write is the modelling step's one
        // diagnostic, and the subscriber retires the generation from it.
        let mut shared = test_shared(&directory);
        shared.media_dir = directory.path().join("missing-media");
        let shared = Arc::new(shared);
        let generation = shared.clock.begin_generation();
        let fake = Arc::new(FakeTransport::new());

        let guard = tracing::subscriber::set_default(RetireOnDiagnostic {
            clock: Arc::clone(&shared.clock),
            generation,
        });
        let stored = shared
            .receive_message(
                generation,
                Arc::new(wa::Message {
                    image_message: MessageField::some(wa::message::ImageMessage {
                        jpeg_thumbnail: Some(b"\xff\xd8\xffpreview".to_vec()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                info(CHAT, "IMG-1"),
                wire(&fake),
            )
            .await;
        drop(guard);

        assert!(!stored);
        assert!(shared.database.messages(CHAT, 10).unwrap().is_empty());
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
        assert!(matches!(
            message_media(
                &message,
                tempfile::tempdir().unwrap().path(),
                "chat",
                "poll",
                1,
                0,
            ),
            Some(MessageMedia::Poll { .. })
        ));
    }

    #[test]
    fn poll_selectable_count_respects_multiple_answer_envelopes() {
        let poll = wa::message::PollCreationMessage {
            name: Some("Choose".into()),
            options: ["One", "Two", "Three"]
                .into_iter()
                .map(|name| wa::message::poll_creation_message::Option {
                    option_name: Some(name.into()),
                    ..Default::default()
                })
                .collect(),
            selectable_options_count: Some(0),
            ..Default::default()
        };
        let multiple = wa::Message {
            poll_creation_message: MessageField::some(poll.clone()),
            ..Default::default()
        };
        let multiple_v3 = wa::Message {
            poll_creation_message_v3: MessageField::some(poll.clone()),
            ..Default::default()
        };
        let default_single = wa::Message {
            poll_creation_message_v3: MessageField::some(wa::message::PollCreationMessage {
                selectable_options_count: None,
                ..poll.clone()
            }),
            ..Default::default()
        };
        let capped = wa::Message {
            poll_creation_message: MessageField::some(wa::message::PollCreationMessage {
                selectable_options_count: Some(99),
                ..poll
            }),
            ..Default::default()
        };

        let selectable = |message: &wa::Message| match poll_media(message) {
            Some(MessageMedia::Poll {
                selectable_count, ..
            }) => selectable_count,
            _ => panic!("expected poll media"),
        };
        assert_eq!(selectable(&multiple), 3);
        assert_eq!(selectable(&multiple_v3), 3);
        assert_eq!(selectable(&default_single), 1);
        assert_eq!(selectable(&capped), 3);
    }

    #[test]
    fn message_secret_prefers_the_base_envelope_and_falls_back_to_outer() {
        let outer = wa::Message {
            message_context_info: MessageField::some(wa::MessageContextInfo {
                message_secret: Some(vec![1, 2, 3]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let base = wa::Message {
            message_context_info: MessageField::some(wa::MessageContextInfo {
                message_secret: Some(vec![4, 5, 6]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(message_secret(&outer, &base), Some([4, 5, 6].as_slice()));
        assert_eq!(
            message_secret(&outer, &wa::Message::default()),
            Some([1, 2, 3].as_slice())
        );
        assert_eq!(
            message_secret(&wa::Message::default(), &wa::Message::default()),
            None
        );
    }

    #[test]
    fn protocol_control_messages_have_no_user_visible_fallback() {
        let control = wa::Message {
            protocol_message: MessageField::some(wa::message::ProtocolMessage::default()),
            ..Default::default()
        };
        assert_eq!(media_text(&control, ""), None);
        assert_eq!(media_placeholder("unknown"), None);
        assert!(find_reaction_message_at_depth(&wa::Message::default(), 16).is_none());
    }

    #[test]
    fn media_fallback_matrix_and_invalid_poll_are_explicit() {
        let location = |name: Option<&str>, address: Option<&str>, live| wa::Message {
            location_message: MessageField::some(wa::message::LocationMessage {
                name: name.map(str::to_owned),
                address: address.map(str::to_owned),
                is_live: Some(live),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            media_text(&location(Some("Place"), None, false), ""),
            Some("Place".into())
        );
        assert_eq!(
            media_text(&location(None, Some("Street"), false), ""),
            Some("Street".into())
        );
        assert_eq!(
            media_text(&location(None, None, false), ""),
            Some("[Location]".into())
        );
        assert_eq!(
            media_text(&location(None, None, true), ""),
            Some("[Live location]".into())
        );
        assert_eq!(
            media_text(
                &wa::Message {
                    live_location_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                ""
            ),
            Some("[Live location]".into())
        );

        let cases = [
            (
                wa::Message {
                    image_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Image]",
            ),
            (
                wa::Message {
                    video_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Video]",
            ),
            (
                wa::Message {
                    document_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Document]",
            ),
            (
                wa::Message {
                    contact_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Contact]",
            ),
            (
                wa::Message {
                    event_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Event]",
            ),
            (
                wa::Message {
                    group_invite_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Group invite]",
            ),
            (
                wa::Message {
                    product_message: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Product]",
            ),
            (
                wa::Message {
                    call_log_messsage: MessageField::some(Default::default()),
                    ..Default::default()
                },
                "[Call]",
            ),
        ];
        for (message, expected) in cases {
            assert_eq!(media_text(&message, ""), Some(expected.into()));
        }
        for kind in [
            "image",
            "video",
            "ptv",
            "audio",
            "ptt",
            "document",
            "sticker",
            "contact",
            "location",
            "live_location",
            "poll",
        ] {
            assert!(media_placeholder(kind).is_some());
        }

        let invalid_poll = wa::Message {
            poll_creation_message: MessageField::some(wa::message::PollCreationMessage {
                options: vec![wa::message::poll_creation_message::Option {
                    option_name: Some("Only".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(poll_media(&invalid_poll), None);
        assert_eq!(media_text(&invalid_poll, "poll"), Some("[Poll]".into()));
        assert_eq!(coordinate_e7(Some(f64::NAN)), 0);
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
                    sticker_message: MessageField::some(wa::message::StickerMessage {
                        mimetype: None,
                        ..sticker
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let MessageMedia::Sticker {
            downloaded,
            animated,
            lottie,
            mime_type,
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
        assert_eq!(mime_type, "application/json");
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
    fn existing_media_files_and_cache_failures_are_reflected_in_models() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media");
        assets::private_dir(&media_dir).unwrap();
        let chat = "1@s.whatsapp.net";

        std::fs::write(
            assets::message_image_path(&media_dir, chat, "image"),
            b"image",
        )
        .unwrap();
        let image = wa::Message {
            image_message: MessageField::some(Default::default()),
            ..Default::default()
        };
        assert!(matches!(
            message_media(&image, &media_dir, chat, "image", 1, 0),
            Some(MessageMedia::Image { downloaded: true, ref mime_type, .. })
                if mime_type == "image/jpeg"
        ));

        std::fs::write(
            assets::message_video_path(&media_dir, chat, "video", None),
            b"video",
        )
        .unwrap();
        let video = wa::Message {
            ptv_message: MessageField::some(Default::default()),
            ..Default::default()
        };
        assert!(matches!(
            message_media(&video, &media_dir, chat, "video", 1, 0),
            Some(MessageMedia::Video { downloaded: true, ref mime_type, .. })
                if mime_type == "video/mp4"
        ));

        std::fs::write(
            assets::message_audio_path(&media_dir, chat, "audio", None),
            b"audio",
        )
        .unwrap();
        let audio = wa::Message {
            audio_message: MessageField::some(Default::default()),
            ..Default::default()
        };
        assert!(matches!(
            message_media(&audio, &media_dir, chat, "audio", 1, 0),
            Some(MessageMedia::Audio { downloaded: true, ref mime_type, .. })
                if mime_type == "audio/ogg; codecs=opus"
        ));

        std::fs::write(
            assets::message_sticker_path(&media_dir, chat, "sticker"),
            b"sticker",
        )
        .unwrap();
        let sticker = wa::Message {
            sticker_message: MessageField::some(Default::default()),
            ..Default::default()
        };
        assert!(matches!(
            message_media(&sticker, &media_dir, chat, "sticker", 1, 0),
            Some(MessageMedia::Sticker { downloaded: true, ref mime_type, .. })
                if mime_type == "image/webp"
        ));

        let document = wa::Message {
            document_message: MessageField::some(wa::message::DocumentMessage {
                title: Some("Title fallback".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(matches!(
            message_media(&document, &media_dir, chat, "document", 1, 0),
            Some(MessageMedia::Document { ref file_name, ref mime_type, .. })
                if file_name == "Title fallback" && mime_type == "application/octet-stream"
        ));

        let location = wa::Message {
            location_message: MessageField::some(wa::message::LocationMessage {
                degrees_latitude: Some(f64::INFINITY),
                degrees_longitude: Some(-181.0),
                is_live: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(matches!(
            message_media(&location, &media_dir, chat, "location", 2, 0),
            Some(MessageMedia::Location {
                latitude_e7: 0,
                longitude_e7: -1_800_000_000,
                live: false,
                ..
            })
        ));

        let missing_dir = directory.path().join("missing");
        let with_thumbnail = wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                jpeg_thumbnail: Some(b"\xff\xd8\xffpreview".to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(message_media(&with_thumbnail, &missing_dir, chat, "error", 1, 0).is_some());
        let sticker_with_thumbnail = wa::Message {
            sticker_message: MessageField::some(wa::message::StickerMessage {
                png_thumbnail: Some(b"\x89PNG\r\n\x1a\npreview".to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            message_media(
                &sticker_with_thumbnail,
                &missing_dir,
                chat,
                "sticker-error",
                1,
                0,
            )
            .is_some()
        );
        let video_with_thumbnail = wa::Message {
            video_message: MessageField::some(wa::message::VideoMessage {
                jpeg_thumbnail: Some(b"\xff\xd8\xffpreview".to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            message_media(
                &video_with_thumbnail,
                &missing_dir,
                chat,
                "video-error",
                1,
                0,
            )
            .is_some()
        );
        assert_eq!(
            cache_location_thumbnail(
                &missing_dir,
                chat,
                "location-error",
                Some(&b"\xff\xd8\xffpreview".to_vec()),
            ),
            None
        );
    }

    #[test]
    fn an_unchanged_location_thumbnail_is_reused_without_rewriting_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media");
        assets::private_dir(&media_dir).unwrap();
        let thumbnail = b"\xff\xd8\xffstatic".to_vec();
        let first =
            cache_location_thumbnail(&media_dir, "1@s.whatsapp.net", "here", Some(&thumbnail))
                .unwrap();
        let written = std::fs::metadata(&first).unwrap().modified().unwrap();
        assert_eq!(
            cache_location_thumbnail(&media_dir, "1@s.whatsapp.net", "here", Some(&thumbnail)),
            Some(first.clone())
        );
        assert_eq!(
            std::fs::metadata(&first).unwrap().modified().unwrap(),
            written
        );
        assert_eq!(std::fs::read(&first).unwrap(), thumbnail);
        // A payload without a JPEG signature is never cached.
        assert_eq!(
            cache_location_thumbnail(
                &media_dir,
                "1@s.whatsapp.net",
                "here",
                Some(&b"not a jpeg".to_vec()),
            ),
            None
        );
    }
}
