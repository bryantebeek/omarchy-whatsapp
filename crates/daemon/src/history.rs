// History-sync modelling: the bounded lazy ingest pass, the pure decoders it
// uses, and the deferred media downloads it schedules.

use crate::assets;
use crate::identity::normalize_jid;
use crate::messages::{
    find_reaction_message, media_text, message_media, message_secret, poll_media, sticker_message,
    video_message,
};
use crate::state::{Shared, broadcast_messages};
use crate::transport::Transport;
use crate::util::nonempty;
use anyhow::Result;
use buffa::Message as _;
use futures::StreamExt;
use omarchy_whatsapp_protocol::{Message, MessageMedia};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};
use whatsapp_rust::prelude::*;

#[derive(Clone)]
pub(crate) enum PendingMedia {
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
    pub(crate) fn ingest_history(
        &self,
        lazy: &whatsapp_rust::types::events::LazyHistorySync,
        own_pn: Option<&str>,
        aliases: &HashMap<String, String>,
    ) -> Result<(Vec<PendingMedia>, HashSet<String>)> {
        let mut pending_media = Vec::new();
        let mut changed_message_chats = HashSet::new();
        let mut pending_documents = 0;
        let mut stream = lazy.stream();
        while let Some(conversation) = stream.next_conversation()? {
            let chat_jid = canonical_history_jid(&conversation.id, aliases);
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
                    if let Some(reaction) = history_reaction(&chat_jid, wire, aliases) {
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
                    let result =
                        history_message(&chat_jid, &chat_name, wire, &self.media_dir, aliases);
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
            let chat = omarchy_whatsapp_protocol::Chat {
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
            };
            // One durable commit per conversation instead of one per message:
            // a replayed sync otherwise costs an fsync for every message it
            // stores.
            if self
                .database
                .insert_history_conversation(&chat, &messages)?
            {
                changed_message_chats.insert(chat_jid.clone());
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
                    history_poll_creator(&chat_jid, wire, own_pn, aliases),
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
                        canonical_history_jid(participant, aliases)
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
                    match self.database.apply_poll_vote(
                        &chat_jid,
                        message_id,
                        &voter_jid,
                        &selected_options,
                        from_me,
                        timestamp,
                    ) {
                        Ok(true) => {
                            changed_message_chats.insert(chat_jid.clone());
                        }
                        Ok(false) => {}
                        Err(error) => {
                            warn!(%error, %chat_jid, %message_id,
                                "could not persist history poll vote");
                        }
                    }
                }
            }
        }
        let skipped = stream.skipped_conversations();
        if skipped > 0 {
            warn!(skipped, "history sync contained undecodable conversations");
        }
        let remainder = stream.remainder()?;
        for push_name in remainder.pushnames {
            if let (Some(jid), Some(name)) = (push_name.id, push_name.pushname) {
                self.database
                    .update_contact_name(&normalize_jid(&jid), &name)?;
            }
        }
        Ok((pending_media, changed_message_chats))
    }
}

fn canonical_history_jid(value: &str, aliases: &HashMap<String, String>) -> String {
    let normalized = normalize_jid(value);
    aliases.get(&normalized).cloned().unwrap_or(normalized)
}

fn collect_history_lid_jid(jids: &mut HashSet<String>, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    let Ok(jid) = value.parse::<Jid>() else {
        return;
    };
    if jid.is_lid() {
        jids.insert(jid.to_non_ad_string());
    }
}

// Lazy history parsing is synchronous and deliberately bounded-memory. Scan
// the compressed payload once for identity dependencies so their async SDK
// lookups finish before a second bounded pass writes any conversation state.
pub(crate) fn history_lid_jids(
    lazy: &whatsapp_rust::types::events::LazyHistorySync,
) -> Result<HashSet<String>> {
    let mut jids = HashSet::new();
    let mut stream = lazy.stream();
    while let Some(conversation) = stream.next_conversation()? {
        collect_history_lid_jid(&mut jids, Some(&conversation.id));
        for wire in conversation
            .messages
            .iter()
            .filter_map(|history| history.message.as_option())
        {
            collect_history_lid_jid(&mut jids, wire.participant.as_deref());
            if let Some(key) = wire.key.as_option() {
                collect_history_lid_jid(&mut jids, key.remote_jid.as_deref());
                collect_history_lid_jid(&mut jids, key.participant.as_deref());
            }
            for update in &wire.poll_updates {
                if let Some(key) = update.poll_update_message_key.as_option() {
                    collect_history_lid_jid(&mut jids, key.remote_jid.as_deref());
                    collect_history_lid_jid(&mut jids, key.participant.as_deref());
                }
            }
        }
    }
    Ok(jids)
}

pub(crate) async fn download_pending_media(
    shared: Arc<Shared>,
    transport: Arc<dyn Transport>,
    media: Vec<PendingMedia>,
) {
    let downloaded = futures::stream::iter(media.into_iter().map(|pending| {
        let transport = Arc::clone(&transport);
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
                    assets::download_message_document(transport, document, path).await
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
    if downloaded > 0 {
        for chat_jid in shared.connection_state().active_chats {
            broadcast_messages(&shared, &chat_jid);
        }
    }
    info!(downloaded, "cached WhatsApp history media");
}

struct HistoryReaction {
    message_id: String,
    reactor_jid: String,
    emoji: String,
    from_me: bool,
    timestamp: i64,
}

fn history_reaction(
    chat_jid: &str,
    wire: &wa::WebMessageInfo,
    aliases: &HashMap<String, String>,
) -> Option<HistoryReaction> {
    let envelope_key = wire.key.as_option()?;
    let reaction = find_reaction_message(wire.message.as_option()?)?;
    let target = reaction.key.as_option()?;
    let from_me = envelope_key.from_me.unwrap_or(false);
    let reactor_jid = if from_me {
        "me".to_owned()
    } else {
        canonical_history_jid(
            wire.participant
                .as_deref()
                .or(envelope_key.participant.as_deref())
                .unwrap_or(chat_jid),
            aliases,
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

pub(crate) fn option_names_for_hashes(
    options: &[String],
    hashes: &[Vec<u8>],
) -> Option<Vec<String>> {
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
    aliases: &HashMap<String, String>,
) -> Option<String> {
    let key = wire.key.as_option()?;
    if key.from_me.unwrap_or(false) {
        key.participant
            .as_deref()
            .or(wire.participant.as_deref())
            .map(|jid| canonical_history_jid(jid, aliases))
            .or_else(|| own_pn.map(str::to_owned))
    } else if chat_jid.ends_with("@g.us") {
        wire.participant
            .as_deref()
            .or(key.participant.as_deref())
            .map(|jid| canonical_history_jid(jid, aliases))
    } else {
        Some(chat_jid.to_owned())
    }
}

fn history_message(
    chat_jid: &str,
    chat_name: &str,
    wire: &wa::WebMessageInfo,
    media_dir: &Path,
    aliases: &HashMap<String, String>,
) -> Option<Message> {
    let key = wire.key.as_option()?;
    let id = key.id.clone()?;
    let body = wire.message.as_option()?;
    let from_me = key.from_me.unwrap_or(false);
    let sender_jid = if from_me {
        "me".to_owned()
    } else {
        canonical_history_jid(
            wire.participant
                .as_deref()
                .or(key.participant.as_deref())
                .unwrap_or(chat_jid),
            aliases,
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::default_trait_access)] // Generated protobuf fixture types are inferred by MessageField.
mod tests {
    use super::*;
    use crate::test_support::test_shared;
    use crate::transport::fake::{Call, CallKind, FakeTransport, MediaKind, transport};
    use buffa::MessageField;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    /// A `conversations` field (2) whose payload claims five bytes and carries
    /// one, so the lenient stream skips and counts it.
    const CORRUPT_CONVERSATION: &[u8] = &[0x12, 0x03, 0x0A, 0x05, b'x'];

    fn compress(history: &wa::HistorySync, tail: &[u8]) -> Vec<u8> {
        let mut raw = history.encode_to_vec();
        raw.extend_from_slice(tail);
        raw
    }

    fn lazy(raw: &[u8]) -> whatsapp_rust::types::events::LazyHistorySync {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).unwrap();
        whatsapp_rust::types::events::LazyHistorySync::new(
            encoder.finish().unwrap().into(),
            raw.len(),
            wa::history_sync::HistorySyncType::INITIAL_BOOTSTRAP as i32,
            None,
            Some(100),
        )
    }

    fn history_message_entry(
        id: &str,
        participant: Option<&str>,
        body: wa::Message,
        timestamp: u64,
    ) -> wa::HistorySyncMsg {
        wa::HistorySyncMsg {
            message: MessageField::some(wa::WebMessageInfo {
                key: MessageField::some(wa::MessageKey {
                    remote_jid: Some("123-456@g.us".into()),
                    from_me: Some(false),
                    id: Some(id.into()),
                    participant: participant.map(str::to_owned),
                }),
                message: MessageField::some(body),
                message_timestamp: Some(timestamp),
                push_name: Some("Bob".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn media_body(media: wa::Message) -> wa::Message {
        media
    }

    fn poll_options(names: &[&str]) -> Vec<wa::message::poll_creation_message::Option> {
        names
            .iter()
            .map(|name| wa::message::poll_creation_message::Option {
                option_name: Some((*name).into()),
                ..Default::default()
            })
            .collect()
    }

    fn option_hash(name: &str) -> Vec<u8> {
        whatsapp_rust::wacore::poll::compute_option_hash(name).to_vec()
    }

    fn poll_update(
        from_me: bool,
        participant: Option<&str>,
        selected: &[&str],
        sender_timestamp_ms: Option<i64>,
        server_timestamp_ms: Option<i64>,
    ) -> wa::PollUpdate {
        wa::PollUpdate {
            poll_update_message_key: MessageField::some(wa::MessageKey {
                remote_jid: Some("123-456@g.us".into()),
                from_me: Some(from_me),
                id: Some("VOTE".into()),
                participant: participant.map(str::to_owned),
            }),
            vote: MessageField::some(wa::message::PollVoteMessage {
                selected_options: selected.iter().copied().map(option_hash).collect(),
            }),
            sender_timestamp_ms,
            server_timestamp_ms,
            ..Default::default()
        }
    }

    fn synthetic_history() -> wa::HistorySync {
        let participant_lid = "100000000000002@lid";
        let group_poll = history_message_entry(
            "MSG-POLL",
            Some(participant_lid),
            wa::Message {
                poll_creation_message_v3: MessageField::some(wa::message::PollCreationMessage {
                    name: Some("Lunch?".into()),
                    options: poll_options(&["Soup", "Salad"]),
                    selectable_options_count: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            },
            1_700_000_400,
        );
        let group_poll = wa::HistorySyncMsg {
            message: MessageField::some(wa::WebMessageInfo {
                message_secret: Some(vec![7u8; 32]),
                poll_updates: vec![
                    // The device's own vote, timed by the sender clock.
                    poll_update(true, None, &["Soup"], Some(1_700_000_450_000), None),
                    // A participant's vote, timed by the server clock only.
                    poll_update(
                        false,
                        Some(participant_lid),
                        &["Salad"],
                        None,
                        Some(1_700_000_460_000),
                    ),
                    // A hash the poll never offered.
                    wa::PollUpdate {
                        vote: MessageField::some(wa::message::PollVoteMessage {
                            selected_options: vec![vec![0u8; 32]],
                        }),
                        ..poll_update(false, Some(participant_lid), &[], None, None)
                    },
                    // A group vote WhatsApp did not attribute to anyone.
                    poll_update(false, None, &["Soup"], None, None),
                    // A vote selecting more options than the poll allows.
                    poll_update(
                        false,
                        Some("31600000003@s.whatsapp.net"),
                        &["Soup", "Salad"],
                        Some(1_700_000_470_000),
                        None,
                    ),
                    // A vote whose envelope carries neither key nor ballot.
                    wa::PollUpdate::default(),
                ],
                ..group_poll.message.unwrap()
            }),
            ..group_poll
        };

        wa::HistorySync {
            sync_type: wa::history_sync::HistorySyncType::INITIAL_BOOTSTRAP,
            pushnames: vec![
                wa::Pushname {
                    id: Some("31600000005:2@s.whatsapp.net".into()),
                    pushname: Some("Eve".into()),
                },
                wa::Pushname::default(),
            ],
            conversations: vec![
                wa::Conversation {
                    id: "status@broadcast".into(),
                    ..Default::default()
                },
                wa::Conversation {
                    id: "1@newsletter".into(),
                    ..Default::default()
                },
                wa::Conversation {
                    id: "123-456@g.us".into(),
                    display_name: Some("Garden".into()),
                    last_msg_timestamp: Some(1_700_000_500),
                    messages: vec![
                        history_message_entry(
                            "MSG-TEXT",
                            Some(participant_lid),
                            wa::Message::text("hello"),
                            1_700_000_100,
                        ),
                        history_message_entry(
                            "REACT-1",
                            Some(participant_lid),
                            wa::Message {
                                reaction_message: MessageField::some(
                                    wa::message::ReactionMessage {
                                        key: MessageField::some(wa::MessageKey {
                                            id: Some("MSG-TEXT".into()),
                                            ..Default::default()
                                        }),
                                        text: Some("👍".into()),
                                        sender_timestamp_ms: Some(1_700_000_200_000),
                                        ..Default::default()
                                    },
                                ),
                                ..Default::default()
                            },
                            1_700_000_200,
                        ),
                        history_message_entry(
                            "MSG-IMG",
                            Some(participant_lid),
                            media_body(wa::Message {
                                image_message: MessageField::some(wa::message::ImageMessage {
                                    file_length: Some(16),
                                    mimetype: Some("image/jpeg".into()),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            1_700_000_210,
                        ),
                        history_message_entry(
                            "MSG-STK",
                            Some(participant_lid),
                            media_body(wa::Message {
                                lottie_sticker_message: MessageField::some(
                                    wa::message::FutureProofMessage {
                                        message: MessageField::some(wa::Message {
                                            sticker_message: MessageField::some(
                                                wa::message::StickerMessage {
                                                    file_length: Some(8),
                                                    ..Default::default()
                                                },
                                            ),
                                            ..Default::default()
                                        }),
                                    },
                                ),
                                ..Default::default()
                            }),
                            1_700_000_220,
                        ),
                        history_message_entry(
                            "MSG-VID",
                            Some(participant_lid),
                            media_body(wa::Message {
                                video_message: MessageField::some(wa::message::VideoMessage {
                                    file_length: Some(32),
                                    mimetype: Some("video/mp4".into()),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            1_700_000_230,
                        ),
                        history_message_entry(
                            "MSG-AUD",
                            Some(participant_lid),
                            media_body(wa::Message {
                                audio_message: MessageField::some(wa::message::AudioMessage {
                                    file_length: Some(12),
                                    ptt: Some(true),
                                    mimetype: Some("audio/ogg".into()),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            1_700_000_240,
                        ),
                        history_message_entry(
                            "MSG-DOC",
                            Some(participant_lid),
                            media_body(wa::Message {
                                document_message: MessageField::some(
                                    wa::message::DocumentMessage {
                                        file_length: Some(9),
                                        file_name: Some("report.pdf".into()),
                                        ..Default::default()
                                    },
                                ),
                                ..Default::default()
                            }),
                            1_700_000_250,
                        ),
                        group_poll,
                        // A history entry without a decodable body is dropped.
                        wa::HistorySyncMsg::default(),
                        // An envelope WhatsApp sent without a key.
                        wa::HistorySyncMsg {
                            message: MessageField::some(wa::WebMessageInfo {
                                message: MessageField::some(wa::Message::text("keyless")),
                                message_timestamp: Some(1_700_000_260),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        // An envelope whose key carries no message ID.
                        wa::HistorySyncMsg {
                            message: MessageField::some(wa::WebMessageInfo {
                                key: MessageField::some(wa::MessageKey {
                                    remote_jid: Some("123-456@g.us".into()),
                                    from_me: Some(false),
                                    id: None,
                                    participant: None,
                                }),
                                message: MessageField::some(wa::Message::text("anonymous")),
                                message_timestamp: Some(1_700_000_270),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                wa::Conversation {
                    id: "100000012345678@lid".into(),
                    name: Some("Ada".into()),
                    unread_count: Some(1),
                    messages: vec![wa::HistorySyncMsg {
                        message: MessageField::some(wa::WebMessageInfo {
                            key: MessageField::some(wa::MessageKey {
                                remote_jid: Some("100000012345678@lid".into()),
                                from_me: Some(false),
                                id: Some("MSG-DIRECT-POLL".into()),
                                participant: None,
                            }),
                            message: MessageField::some(wa::Message {
                                poll_creation_message_v3: MessageField::some(
                                    wa::message::PollCreationMessage {
                                        name: Some("Tea?".into()),
                                        options: poll_options(&["Yes", "No"]),
                                        selectable_options_count: Some(1),
                                        ..Default::default()
                                    },
                                ),
                                ..Default::default()
                            }),
                            // A secret WhatsApp truncated: the daemon logs and
                            // keeps the poll rather than aborting the chunk.
                            message_secret: Some(vec![1, 2, 3]),
                            message_timestamp: Some(1_700_000_600),
                            poll_updates: vec![wa::PollUpdate {
                                poll_update_message_key: MessageField::some(wa::MessageKey {
                                    remote_jid: Some("100000012345678@lid".into()),
                                    from_me: Some(false),
                                    id: Some("VOTE".into()),
                                    participant: None,
                                }),
                                vote: MessageField::some(wa::message::PollVoteMessage {
                                    selected_options: vec![option_hash("Yes")],
                                }),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                // Named only by the address book, and with no messages at all.
                wa::Conversation {
                    id: "31600000010@s.whatsapp.net".into(),
                    ..Default::default()
                },
                // A blank name is not a name, and no address-book entry exists.
                wa::Conversation {
                    id: "31600000011@s.whatsapp.net".into(),
                    name: Some("   ".into()),
                    conversation_timestamp: Some(1_700_000_700),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn aliases() -> HashMap<String, String> {
        HashMap::from([
            (
                "100000000000002@lid".to_owned(),
                "31600000002@s.whatsapp.net".to_owned(),
            ),
            (
                "100000012345678@lid".to_owned(),
                "31612345678@s.whatsapp.net".to_owned(),
            ),
        ])
    }

    #[tokio::test]
    async fn a_history_chunk_absorbs_media_reactions_polls_and_push_names() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        assets::private_dir(&shared.media_dir).unwrap();
        shared
            .database
            .update_address_book_name("31600000010@s.whatsapp.net", "Frank")
            .unwrap();
        let raw = compress(&synthetic_history(), CORRUPT_CONVERSATION);
        let lazy = lazy(&raw);

        assert_eq!(
            history_lid_jids(&lazy).unwrap(),
            HashSet::from([
                "100000000000002@lid".to_owned(),
                "100000012345678@lid".to_owned(),
            ])
        );

        let (pending, changed) = shared
            .ingest_history(&lazy, Some("31600000000@s.whatsapp.net"), &aliases())
            .unwrap();

        // Broadcast and newsletter conversations never become chats.
        let mut chats = shared
            .database
            .list_chats(20)
            .unwrap()
            .into_iter()
            .map(|chat| (chat.jid, chat.name, chat.last_timestamp))
            .collect::<Vec<_>>();
        chats.sort();
        assert_eq!(
            chats,
            vec![
                (
                    "123-456@g.us".to_owned(),
                    "Garden".to_owned(),
                    1_700_000_500
                ),
                (
                    "31600000010@s.whatsapp.net".to_owned(),
                    "Frank".to_owned(),
                    0
                ),
                (
                    "31600000011@s.whatsapp.net".to_owned(),
                    String::new(),
                    1_700_000_700
                ),
                (
                    "31612345678@s.whatsapp.net".to_owned(),
                    "Ada".to_owned(),
                    1_700_000_600
                ),
            ]
        );
        assert_eq!(
            changed,
            HashSet::from([
                "123-456@g.us".to_owned(),
                "31612345678@s.whatsapp.net".to_owned(),
            ])
        );

        let group = shared.database.messages("123-456@g.us", 20).unwrap();
        assert_eq!(
            group
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "MSG-TEXT", "MSG-IMG", "MSG-STK", "MSG-VID", "MSG-AUD", "MSG-DOC", "MSG-POLL",
            ]
        );
        let text = &group[0];
        assert_eq!(text.sender_jid, "31600000002@s.whatsapp.net");
        assert_eq!(text.sender_name, "Bob");
        assert_eq!(
            text.reactions
                .iter()
                .map(|reaction| reaction.emoji.as_str())
                .collect::<Vec<_>>(),
            vec!["👍"]
        );

        // Only the votes WhatsApp attributed to a real option are tallied.
        let poll = shared
            .database
            .message_by_id("123-456@g.us", "MSG-POLL")
            .unwrap()
            .unwrap();
        let Some(MessageMedia::Poll {
            options,
            total_voters,
            ..
        }) = poll.media
        else {
            panic!("expected poll media");
        };
        assert_eq!(total_voters, 2);
        assert_eq!(
            options
                .iter()
                .map(|option| (option.name.as_str(), option.votes))
                .collect::<Vec<_>>(),
            vec![("Soup", 1), ("Salad", 1)]
        );
        assert!(options[0].selected_by_me);
        assert_eq!(
            options[1].voter_jids,
            vec!["31600000002@s.whatsapp.net".to_owned()]
        );
        assert_eq!(
            shared
                .database
                .contact_name("31600000005@s.whatsapp.net")
                .unwrap()
                .as_deref(),
            Some("Eve")
        );

        assert_eq!(pending.len(), 5);
        let kinds = pending
            .iter()
            .map(|media| match media {
                PendingMedia::Image { .. } => "image",
                PendingMedia::Sticker { .. } => "sticker",
                PendingMedia::Video { .. } => "video",
                PendingMedia::Audio { .. } => "audio",
                PendingMedia::Document { .. } => "document",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec!["image", "sticker", "video", "audio", "document"]
        );
        assert!(matches!(
            &pending[1],
            PendingMedia::Sticker { sticker, .. } if sticker.is_lottie == Some(true)
        ));

        // Replaying the same chunk changes nothing, which keeps the shell from
        // reloading a chat that did not move.
        let (_, replayed) = shared
            .ingest_history(&lazy, Some("31600000000@s.whatsapp.net"), &aliases())
            .unwrap();
        assert!(replayed.is_empty());
    }

    #[tokio::test]
    async fn pending_history_media_is_cached_and_failures_are_survivable() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        assets::private_dir(&shared.media_dir).unwrap();
        let raw = compress(&synthetic_history(), CORRUPT_CONVERSATION);
        let (pending, _) = shared
            .ingest_history(&lazy(&raw), None, &aliases())
            .unwrap();
        let connection = shared.open_connection();
        shared
            .set_connection_active_chat(connection, Some("123-456@g.us".into()))
            .unwrap();
        let fake = Arc::new(FakeTransport::new().with_download_bytes(b"synthetic document"));
        let mut events = shared.events.subscribe();

        download_pending_media(Arc::clone(&shared), transport(&fake), pending).await;

        for message_id in ["MSG-IMG", "MSG-STK", "MSG-VID", "MSG-AUD"] {
            assert!(
                shared
                    .database
                    .media_download("123-456@g.us", message_id)
                    .unwrap()
                    .is_some(),
                "{message_id} keeps its encrypted payload for a later download"
            );
        }
        assert_eq!(
            fake.calls_of(CallKind::Download),
            vec![Call::Download(MediaKind::Document)]
        );
        assert!(
            assets::message_document_path(
                &shared.media_dir,
                "123-456@g.us",
                "MSG-DOC",
                "report.pdf"
            )
            .exists()
        );
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(|frame| matches!(
                frame.event,
                omarchy_whatsapp_protocol::ServerEvent::Invalidated {
                    resource: omarchy_whatsapp_protocol::Resource::Messages,
                    ..
                }
            ))
        );

        // A document WhatsApp never sized cannot be cached; the pass logs it.
        fake.clear_calls();
        download_pending_media(
            Arc::clone(&shared),
            transport(&fake),
            vec![PendingMedia::Document {
                document: wa::message::DocumentMessage::default(),
                path: shared.media_dir.join("unsized.document"),
            }],
        )
        .await;
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn history_ingest_reports_storage_failures_to_its_caller() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let raw = compress(&synthetic_history(), CORRUPT_CONVERSATION);
        shared
            .database
            .execute_test_sql(
                "CREATE TRIGGER block_reactions BEFORE INSERT ON reactions
                 BEGIN SELECT RAISE(ABORT, 'reactions are blocked'); END;
                 CREATE TRIGGER block_poll_votes BEFORE INSERT ON poll_votes
                 BEGIN SELECT RAISE(ABORT, 'poll votes are blocked'); END;
                 CREATE TRIGGER block_poll_secrets BEFORE INSERT ON poll_secrets
                 BEGIN SELECT RAISE(ABORT, 'poll secrets are blocked'); END;",
            )
            .unwrap();

        // Each per-row failure is logged and the chunk still lands.
        shared
            .ingest_history(&lazy(&raw), None, &aliases())
            .unwrap();
        assert_eq!(
            shared.database.messages("123-456@g.us", 20).unwrap().len(),
            7
        );

        shared
            .database
            .execute_test_sql("DROP TABLE chats")
            .unwrap();
        assert!(
            shared
                .ingest_history(&lazy(&raw), None, &aliases())
                .is_err()
        );
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

        let aliases = HashMap::new();
        let reaction = history_reaction("123-456@g.us", &wire, &aliases).unwrap();
        assert_eq!(reaction.message_id, "parent-message");
        assert_eq!(reaction.reactor_jid, "2@s.whatsapp.net");
        assert_eq!(reaction.emoji, "👍");
        assert!(!reaction.from_me);
        assert_eq!(reaction.timestamp, 12);

        let own_wire = wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                from_me: Some(true),
                ..Default::default()
            }),
            message: MessageField::some(wa::Message {
                reaction_message: MessageField::some(wa::message::ReactionMessage {
                    key: MessageField::some(wa::MessageKey {
                        id: Some("own-parent".into()),
                        ..Default::default()
                    }),
                    text: Some("❤".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            message_timestamp: Some(u64::MAX),
            ..Default::default()
        };
        let own = history_reaction("chat", &own_wire, &aliases).unwrap();
        assert_eq!(own.reactor_jid, "me");
        assert_eq!(own.timestamp, i64::MAX);
    }

    #[test]
    fn history_identity_poll_hash_and_message_fallbacks_are_explicit() {
        let aliases = HashMap::new();
        let options = vec!["Soup".to_owned(), "Salad".to_owned()];
        let soup = whatsapp_rust::wacore::poll::compute_option_hash("Soup").to_vec();
        assert_eq!(
            option_names_for_hashes(&options, std::slice::from_ref(&soup)),
            Some(vec!["Soup".into()])
        );
        assert_eq!(option_names_for_hashes(&options, &[vec![0; 32]]), None);

        let key = |from_me, participant: Option<&str>| wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                from_me: Some(from_me),
                id: Some("message".into()),
                participant: participant.map(str::to_owned),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            history_poll_creator(
                "chat@g.us",
                &key(true, Some("1:2@s.whatsapp.net")),
                None,
                &aliases,
            )
            .as_deref(),
            Some("1@s.whatsapp.net")
        );
        assert_eq!(
            history_poll_creator(
                "chat@g.us",
                &key(true, None),
                Some("me@s.whatsapp.net"),
                &aliases,
            )
            .as_deref(),
            Some("me@s.whatsapp.net")
        );
        assert_eq!(
            history_poll_creator(
                "chat@g.us",
                &key(false, Some("2:3@s.whatsapp.net")),
                None,
                &aliases,
            )
            .as_deref(),
            Some("2@s.whatsapp.net")
        );
        assert_eq!(
            history_poll_creator("2@s.whatsapp.net", &key(false, None), None, &aliases).as_deref(),
            Some("2@s.whatsapp.net")
        );
        assert_eq!(
            history_poll_creator("chat@g.us", &wa::WebMessageInfo::default(), None, &aliases,),
            None
        );

        let directory = tempfile::tempdir().unwrap();
        assert!(
            history_message(
                "chat",
                "Chat",
                &wa::WebMessageInfo::default(),
                directory.path(),
                &aliases,
            )
            .is_none()
        );
        let missing_id = wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey::default()),
            ..Default::default()
        };
        assert!(history_message("chat", "Chat", &missing_id, directory.path(), &aliases).is_none());

        let outgoing = wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                from_me: Some(true),
                id: Some("outgoing".into()),
                ..Default::default()
            }),
            message: MessageField::some(wa::Message::text("hello")),
            message_timestamp: Some(u64::MAX),
            ..Default::default()
        };
        let outgoing =
            history_message("chat", "Chat", &outgoing, directory.path(), &aliases).unwrap();
        assert_eq!(outgoing.sender_jid, "me");
        assert_eq!(outgoing.sender_name, "You");
        assert_eq!(outgoing.timestamp, i64::MAX);

        let incoming = wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                from_me: Some(false),
                id: Some("incoming".into()),
                ..Default::default()
            }),
            message: MessageField::some(wa::Message::text("hello")),
            ..Default::default()
        };
        let incoming =
            history_message("chat", "Chat name", &incoming, directory.path(), &aliases).unwrap();
        assert_eq!(incoming.sender_jid, "chat");
        assert_eq!(incoming.sender_name, "Chat name");

        let participant_fallback = wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                from_me: Some(false),
                id: Some("participant".into()),
                participant: Some("3:4@s.whatsapp.net".into()),
                ..Default::default()
            }),
            message: MessageField::some(wa::Message::text("hello")),
            ..Default::default()
        };
        let participant = history_message(
            "chat@g.us",
            "Chat",
            &participant_fallback,
            directory.path(),
            &aliases,
        )
        .unwrap();
        assert_eq!(participant.sender_jid, "3@s.whatsapp.net");
        assert_eq!(participant.sender_name, "3@s.whatsapp.net");

        let final_location = wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                from_me: Some(false),
                id: Some("location".into()),
                ..Default::default()
            }),
            message: MessageField::some(wa::Message::text("location")),
            message_timestamp: Some(100),
            duration: Some(60),
            final_live_location: MessageField::some(wa::message::LiveLocationMessage {
                degrees_latitude: Some(1.0),
                degrees_longitude: Some(2.0),
                time_offset: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(matches!(
            history_message("chat", "Chat", &final_location, directory.path(), &aliases)
                .unwrap()
                .media,
            Some(MessageMedia::Location {
                live: false,
                updated_at: 105,
                duration_seconds: 60,
                ..
            })
        ));
    }

    #[test]
    fn history_identity_aliases_are_canonical_before_modeling() {
        let lid = "100000012345678@lid";
        let phone = "31612345678@s.whatsapp.net";
        let aliases = HashMap::from([(lid.to_owned(), phone.to_owned())]);

        assert_eq!(canonical_history_jid(lid, &aliases), phone);
        assert_eq!(
            canonical_history_jid("100000012345678:7@lid", &aliases),
            phone
        );
        assert_eq!(
            canonical_history_jid("31600000000:4@s.whatsapp.net", &aliases),
            "31600000000@s.whatsapp.net"
        );
        assert_eq!(canonical_history_jid("malformed", &aliases), "malformed");

        let mut collected = HashSet::new();
        collect_history_lid_jid(&mut collected, None);
        collect_history_lid_jid(&mut collected, Some("malformed"));
        collect_history_lid_jid(&mut collected, Some("31600000000@s.whatsapp.net"));
        collect_history_lid_jid(&mut collected, Some("100000012345678:7@lid"));
        assert_eq!(collected, HashSet::from([lid.to_owned()]));
    }

    #[test]
    fn history_sync_populates_initial_chat_list() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let lid_jid = "100000012345678@lid";
        let phone_jid = "31612345678@s.whatsapp.net";
        let history = wa::HistorySync {
            sync_type: wa::history_sync::HistorySyncType::INITIAL_BOOTSTRAP,
            conversations: vec![wa::Conversation {
                id: lid_jid.into(),
                name: Some("Ada".into()),
                unread_count: Some(1),
                conversation_timestamp: Some(1_700_000_000),
                messages: vec![wa::HistorySyncMsg {
                    message: MessageField::some(wa::WebMessageInfo {
                        key: MessageField::some(wa::MessageKey {
                            remote_jid: Some(lid_jid.into()),
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

        assert_eq!(
            history_lid_jids(&lazy).unwrap(),
            HashSet::from([lid_jid.into()])
        );
        let aliases = HashMap::from([(lid_jid.into(), phone_jid.into())]);
        shared.ingest_history(&lazy, None, &aliases).unwrap();

        let chats = shared.database.list_chats(10).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].jid, phone_jid);
        assert_eq!(chats[0].name, "Ada");
        assert_eq!(chats[0].unread, 1);
        assert_eq!(chats[0].last_message, "hello from history");
        let messages = shared.database.messages(phone_jid, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender_jid, phone_jid);
        assert_eq!(messages[0].text, "hello from history");
        assert_eq!(messages[0].sender_name, "Ada");
    }
}
