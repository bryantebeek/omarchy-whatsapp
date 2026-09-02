// IPC command dispatch: request validation, the background media/avatar job
// queues it hands work to, and the outbound WhatsApp calls each command makes.

use crate::identity::{
    canonical_contact_jid, group_participant_identity, list_chats_with_phone_numbers,
    own_poll_creator_jid, resolve_group_participants,
};
use crate::presence::reconcile_connection_intent;
use crate::state::{
    Shared, broadcast_chats, broadcast_messages, broadcast_text_outbox, broadcast_voice_outbox,
    voice_outbox_event,
};
use crate::sync::refresh_avatar;
use crate::{assets, database, jobs, text_outbox, voice_outbox};
use anyhow::{Context, Result, anyhow, bail};
use buffa::Message as _;
use chrono::Utc;
use omarchy_whatsapp_protocol::{
    ChatState, ChatStateResyncStatus, Command, ConnectionStatus, Message, MessageMedia, PollOption,
    ServerEvent,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::warn;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::download::MediaType;
use whatsapp_rust::wacore_binary::JidExt;
use whatsapp_rust::{SendOptions, UploadOptions, media};

pub(crate) async fn canonical_requested_jid(shared: &Shared, raw: &str) -> String {
    let Ok(jid) = raw.parse::<Jid>() else {
        return raw.to_owned();
    };
    let client = shared.client.read().await.clone();
    match client {
        Some(client) => canonical_contact_jid(shared, &client, &jid).await,
        None => jid.to_non_ad_string(),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[derive(Clone, Copy)]
enum MediaDownloadKind {
    Image,
    Sticker,
    Video,
    Audio,
}

impl MediaDownloadKind {
    fn from_label(label: &str) -> Option<Self> {
        match label {
            "image" => Some(Self::Image),
            "sticker" => Some(Self::Sticker),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Sticker => "sticker",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }
}

/// Validates a media download request and queues the transfer as a background
/// job so the command acks without holding a connection permit across network
/// I/O. Every client learns the outcome through a `media_downloaded` or
/// `media_download_failed` broadcast.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn start_media_download(
    shared: &Arc<Shared>,
    chat_jid: String,
    message_id: String,
) -> Result<ServerEvent> {
    if message_id.is_empty() || message_id.len() > 512 {
        bail!("invalid media message ID");
    }
    let client = shared
        .client
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
    let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
    let stored_kind = shared
        .database
        .message_media_kind(&chat_jid, &message_id)?
        .ok_or_else(|| anyhow!("message does not contain downloadable media"))?;
    let kind = MediaDownloadKind::from_label(&stored_kind)
        .ok_or_else(|| anyhow!("this media type does not require a download"))?;
    let label = kind.label();
    let key = format!("{chat_jid}\0{message_id}");
    {
        let mut downloads = shared.media_downloads.lock().await;
        if downloads.contains(&key) {
            // The pending job for this message broadcasts the outcome.
            return Ok(ServerEvent::Ack);
        }
        if downloads.len() >= jobs::MAX_PENDING_MEDIA_DOWNLOADS {
            bail!("too many downloads are already queued");
        }
        downloads.insert(key.clone());
    }
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        let result = async {
            let _permit = shared
                .media_download_permits
                .acquire()
                .await
                .context("media download queue is closed")?;
            tokio::time::timeout(
                jobs::MEDIA_DOWNLOAD_TIMEOUT,
                perform_media_download(&shared, client, &chat_jid, &message_id, kind),
            )
            .await
            .map_err(|_| anyhow!("{label} download timed out"))?
        }
        .await;
        shared.media_downloads.lock().await.remove(&key);
        match result {
            Ok(media) => shared.publish(ServerEvent::MediaDownloaded {
                media,
                chat_jid,
                message_id,
            }),
            Err(error) => {
                warn!(%error, %chat_jid, %message_id, media = label, "WhatsApp media download failed");
                shared.publish(ServerEvent::MediaDownloadFailed {
                    chat_jid,
                    message_id,
                    message: error.to_string(),
                });
            }
        }
    });
    Ok(ServerEvent::Ack)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn perform_media_download(
    shared: &Arc<Shared>,
    client: Arc<Client>,
    chat_jid: &str,
    message_id: &str,
    kind: MediaDownloadKind,
) -> Result<MessageMedia> {
    let payload =
        media_download_payload(shared, &client, chat_jid, message_id, kind.label()).await?;
    match kind {
        MediaDownloadKind::Image => {
            let image = wa::message::ImageMessage::decode_from_slice(&payload)
                .context("reading image download metadata")?;
            let path = assets::message_image_path(&shared.media_dir, chat_jid, message_id);
            let thumbnail_path = assets::cache_message_image_thumbnail(
                &shared.media_dir,
                chat_jid,
                message_id,
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
                .update_message_media(chat_jid, message_id, &media)?;
            assets::download_message_image(client, image, path).await?;
            if let MessageMedia::Image { downloaded, .. } = &mut media {
                *downloaded = true;
            }
            Ok(media)
        }
        MediaDownloadKind::Sticker => {
            let sticker = wa::message::StickerMessage::decode_from_slice(&payload)
                .context("reading sticker download metadata")?;
            if sticker.is_lottie.unwrap_or(false) {
                bail!("Lottie sticker animation is not supported safely");
            }
            let path = assets::message_sticker_path(&shared.media_dir, chat_jid, message_id);
            let thumbnail_path = assets::cache_message_sticker_thumbnail(
                &shared.media_dir,
                chat_jid,
                message_id,
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
                .update_message_media(chat_jid, message_id, &media)?;
            assets::download_message_sticker(client, sticker, path).await?;
            if let MessageMedia::Sticker { downloaded, .. } = &mut media {
                *downloaded = true;
            }
            Ok(media)
        }
        MediaDownloadKind::Video => {
            let video = wa::message::VideoMessage::decode_from_slice(&payload)
                .context("reading video download metadata")?;
            let path = assets::message_video_path(
                &shared.media_dir,
                chat_jid,
                message_id,
                video.mimetype.as_deref(),
            );
            let thumbnail_path = assets::cache_message_video_thumbnail(
                &shared.media_dir,
                chat_jid,
                message_id,
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
                .update_message_media(chat_jid, message_id, &media)?;
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
            Ok(media)
        }
        MediaDownloadKind::Audio => {
            let audio = wa::message::AudioMessage::decode_from_slice(&payload)
                .context("reading audio download metadata")?;
            let path = assets::message_audio_path(
                &shared.media_dir,
                chat_jid,
                message_id,
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
                .update_message_media(chat_jid, message_id, &media)?;
            assets::download_message_audio(client, audio, path).await?;
            if let MessageMedia::Audio { downloaded, .. } = &mut media {
                *downloaded = true;
            }
            Ok(media)
        }
    }
}

// Runs a queued avatar request; successes surface through the `avatars`
// broadcast that `refresh_avatar` publishes. The caller resolves the canonical
// identity so the in-flight set already deduplicates a contact's LID and
// phone-number forms.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn fetch_requested_avatar(shared: &Arc<Shared>, client: Arc<Client>, canonical: Jid) {
    let Ok(_permit) = shared.avatar_fetch_permits.acquire().await else {
        return;
    };
    let canonical_jid = canonical.to_non_ad_string();
    if tokio::time::timeout(
        jobs::AVATAR_FETCH_TIMEOUT,
        refresh_avatar(Arc::clone(shared), client, canonical, false),
    )
    .await
    .is_err()
    {
        warn!(%canonical_jid, "WhatsApp avatar fetch timed out");
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

async fn finish_recovery_attempt(shared: &Shared, recovery_key: &str, succeeded: bool) -> bool {
    // The set is a successful-attempt marker as well as an in-flight guard.
    // Only a total transient failure should re-arm the next UI refresh.
    if succeeded {
        return false;
    }
    shared
        .media_recovery_requested
        .write()
        .await
        .remove(recovery_key)
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn schedule_message_recovery(
    shared: &Arc<Shared>,
    client: Arc<Client>,
    cursor: database::HistoryCursor,
    recovery_key: String,
    label: &'static str,
) {
    let recovery_shared = Arc::clone(shared);
    let generation = shared.clock.generation();
    tokio::spawn(async move {
        let chat = cursor.chat_jid.parse::<Jid>();
        let exact_result = request_exact_message(&client, &cursor).await;
        let history_result = match chat {
            Ok(chat) => {
                client
                    .fetch_message_history(
                        &chat,
                        &cursor.message_id,
                        cursor.from_me,
                        cursor.timestamp_ms,
                        50,
                    )
                    .await
            }
            Err(error) => Err(error.into()),
        };
        if let (Err(exact_error), Err(history_error)) = (&exact_result, &history_result) {
            warn!(%exact_error, %history_error, message_id = %cursor.message_id,
                recovery = label, "could not request message recovery");
        }
        let succeeded = exact_result.is_ok() || history_result.is_ok();
        if !recovery_shared.clock.is_current(generation) {
            finish_recovery_attempt(&recovery_shared, &recovery_key, false).await;
            return;
        }
        finish_recovery_attempt(&recovery_shared, &recovery_key, succeeded).await;
    });
}

const MAX_POLL_QUESTION_CHARS: usize = 255;
const MAX_POLL_OPTION_CHARS: usize = 100;
const MAX_POLL_OPTIONS: usize = 12;

/// A poll request that satisfied every daemon-side rule, so the outbound call
/// and the locally stored card cannot disagree about what was asked.
struct ValidatedPoll {
    question: String,
    options: Vec<String>,
    selectable_count: u32,
    correct_option_index: Option<u32>,
}

/// Applies the poll limits before any network call. The option cap otherwise
/// lives only in the shell, and `omarchy-whatsappctl` bypasses it. An
/// out-of-range selectable count is rejected rather than clamped, so a mistyped
/// flag cannot silently create a different poll; a quiz always keeps exactly
/// one selectable answer.
fn validate_poll_request(
    question: &str,
    options: Vec<String>,
    selectable_count: u32,
    correct_option_index: Option<u32>,
) -> Result<ValidatedPoll> {
    let question = question.trim().to_owned();
    if question.is_empty() {
        bail!("poll question cannot be empty");
    }
    if question.chars().count() > MAX_POLL_QUESTION_CHARS {
        bail!("poll question is longer than {MAX_POLL_QUESTION_CHARS} characters");
    }
    let mut trimmed: Vec<String> = Vec::with_capacity(options.len());
    for option in options {
        let option = option.trim().to_owned();
        if option.is_empty() {
            bail!("poll options cannot be empty");
        }
        if option.chars().count() > MAX_POLL_OPTION_CHARS {
            bail!("a poll option is longer than {MAX_POLL_OPTION_CHARS} characters");
        }
        if trimmed.contains(&option) {
            bail!("poll options must be distinct");
        }
        trimmed.push(option);
    }
    if !(2..=MAX_POLL_OPTIONS).contains(&trimmed.len()) {
        bail!("a poll needs between 2 and {MAX_POLL_OPTIONS} options");
    }
    let selectable_count = if let Some(index) = correct_option_index {
        if usize::try_from(index).unwrap_or(usize::MAX) >= trimmed.len() {
            bail!("the correct quiz option is outside the poll");
        }
        1
    } else {
        let requested = usize::try_from(selectable_count).unwrap_or(usize::MAX);
        if !(1..=trimmed.len()).contains(&requested) {
            bail!(
                "a poll must allow between 1 and {} selected options",
                trimmed.len()
            );
        }
        selectable_count
    };
    Ok(ValidatedPoll {
        question,
        options: trimmed,
        selectable_count,
        correct_option_index,
    })
}

// Upload-and-send adapter for one recording. The durable job transitions, the
// outbox bounds, and the resulting snapshots are measured in `voice_outbox`.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn send_voice_message(
    shared: &Arc<Shared>,
    chat_jid: String,
    recording_id: String,
) -> Result<ServerEvent> {
    let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
    let outbox_dir = shared.voice_outbox_dir.clone();
    let prepare_recording_id = recording_id.clone();
    let prepare_chat_jid = requested.to_non_ad_string();
    // Preparation runs the outbox cleanup and writes the job, so it is the only
    // step that can touch other recordings and the only one that needs the
    // global gate. The `voice:<recording_id>` conflict key already serializes
    // sending and discarding this recording across connections, and the upload
    // below must not block a panel that is only listing the outbox.
    let mut prepared = {
        let _voice_outbox_guard = shared.voice_outbox_gate.lock().await;
        tokio::task::spawn_blocking(move || {
            voice_outbox::prepare(
                &outbox_dir,
                &prepare_recording_id,
                &prepare_chat_jid,
                Utc::now().timestamp(),
            )
        })
        .await
        .context("voice outbox preparation task failed")??
    };
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
        let canonical_jid = jid.to_non_ad_string();
        let assigned_jid = canonical_jid.clone();
        let assigned_delivery_id = delivery_id.clone();
        prepared.job = persist_voice_job(shared, prepared.job.clone(), move |outbox, job| {
            voice_outbox::assign_delivery(
                outbox,
                job,
                &assigned_jid,
                &assigned_delivery_id,
                Utc::now().timestamp(),
            )
        })
        .await?;
        broadcast_voice_outbox(shared);
        if let Some(message) = shared
            .database
            .message_by_id(&canonical_jid, &delivery_id)?
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
            &canonical_jid,
            &delivery_id,
            Some("audio/ogg; codecs=opus"),
        );
        let recording_path = voice_outbox::recording_path(&shared.voice_outbox_dir, &recording_id)?;
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
            chat_jid: canonical_jid,
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
        // The requesting connection already learns about the send from this
        // command's `sent` response, so other clients only need a refresh.
        broadcast_messages(shared, &message.chat_jid);
        broadcast_chats(shared);
        Ok(message)
    }
    .await;
    match result {
        Ok(message) => {
            let job = prepared.job.clone();
            match persist_voice_job(shared, job, |outbox, job| {
                voice_outbox::finish_sent(outbox, job, Utc::now().timestamp())
            })
            .await
            {
                Ok(job) => prepared.job = job,
                Err(error) => warn!(%error, "could not finalize a sent voice outbox entry"),
            }
            broadcast_voice_outbox(shared);
            Ok(ServerEvent::Sent { message })
        }
        Err(error) => {
            let job = prepared.job.clone();
            let detail = error.to_string();
            match persist_voice_job(shared, job, move |outbox, job| {
                voice_outbox::mark_failed(outbox, job, &detail, Utc::now().timestamp())
            })
            .await
            {
                Ok(job) => prepared.job = job,
                Err(persist_error) => {
                    warn!(%persist_error, "could not retain a failed voice outbox entry");
                }
            }
            broadcast_voice_outbox(shared);
            Err(error)
        }
    }
}

// Voice outbox writes fsync a job file and rename it, so they run on the
// blocking pool instead of stalling an async worker.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn persist_voice_job<F>(
    shared: &Arc<Shared>,
    mut job: voice_outbox::VoiceJob,
    write: F,
) -> Result<voice_outbox::VoiceJob>
where
    F: FnOnce(&Path, &mut voice_outbox::VoiceJob) -> Result<()> + Send + 'static,
{
    let outbox_dir = shared.voice_outbox_dir.clone();
    tokio::task::spawn_blocking(move || write(&outbox_dir, &mut job).map(|()| job))
        .await
        .context("voice outbox write task failed")?
}

// IPC command-to-SDK dispatch is the outbound transport adapter. Command
// identity, deadlines, serialization, durable state machines, and response
// convergence are covered in their dedicated modules and IPC tests.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn handle_command(
    command: Command,
    shared: &Arc<Shared>,
    connection_id: u64,
) -> Result<ServerEvent> {
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
                .get_metadata(&jid)
                .await
                .context("loading WhatsApp group participants")?;
            Ok(ServerEvent::GroupParticipants {
                chat_jid: jid.to_non_ad_string(),
                participants: resolve_group_participants(
                    shared,
                    &client,
                    info.participants
                        .into_iter()
                        .map(group_participant_identity)
                        .collect(),
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
                    {
                        schedule_message_recovery(
                            shared,
                            client,
                            cursor,
                            chat_jid.clone(),
                            "media",
                        );
                    }
                    let poll_recovery_key = format!("poll:{chat_jid}");
                    if let Some(cursor) =
                        shared.database.poll_metadata_recovery_cursor(&chat_jid)?
                        && let Some(client) = shared.client.read().await.clone()
                        && shared
                            .media_recovery_requested
                            .write()
                            .await
                            .insert(poll_recovery_key.clone())
                    {
                        schedule_message_recovery(
                            shared,
                            client,
                            cursor,
                            poll_recovery_key,
                            "poll metadata",
                        );
                    }
                    shared.database.messages(&chat_jid, limit)?
                },
                chat_jid,
                first_unread_message_id,
            })
        }
        Command::SendMessage {
            chat_jid,
            text,
            delivery_id,
        } => {
            let _outbox_guard = shared.text_outbox_gate.lock().await;
            let text = text_outbox::validate(&delivery_id, &text)?;
            let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
            let message_id = text_outbox::stable_message_id(&delivery_id);
            shared.database.enqueue_text_message(
                &delivery_id,
                &requested.to_non_ad_string(),
                &text,
                &message_id,
                Utc::now().timestamp(),
            )?;
            broadcast_text_outbox(shared);
            shared.text_outbox_notify.notify_one();
            Ok(ServerEvent::TextAccepted { delivery_id })
        }
        Command::SendVoiceMessage {
            chat_jid,
            recording_id,
        } => send_voice_message(shared, chat_jid, recording_id).await,
        Command::DiscardVoiceRecording { recording_id } => {
            // Discarding removes this recording's files while another command
            // may be running the outbox cleanup, so it keeps the global gate.
            let _voice_outbox_guard = shared.voice_outbox_gate.lock().await;
            voice_outbox::discard(&shared.voice_outbox_dir, &recording_id)?;
            broadcast_voice_outbox(shared);
            Ok(ServerEvent::Ack)
        }
        // Listing an outbox is a snapshot of atomically written job files or a
        // single query. Taking a delivery gate here would make the panel's
        // first request wait for an in-flight upload and time out.
        Command::ListVoiceOutbox => voice_outbox_event(shared),
        Command::ListTextOutbox => Ok(ServerEvent::TextOutbox {
            entries: shared.database.text_outbox()?,
        }),
        Command::RetryTextMessage { delivery_id } => {
            let _outbox_guard = shared.text_outbox_gate.lock().await;
            if !shared.database.retry_text_message(&delivery_id)? {
                bail!("text message is not waiting for retry");
            }
            broadcast_text_outbox(shared);
            shared.text_outbox_notify.notify_one();
            Ok(ServerEvent::TextAccepted { delivery_id })
        }
        Command::DiscardTextMessage { delivery_id } => {
            let _outbox_guard = shared.text_outbox_gate.lock().await;
            if !shared.database.discard_text_message(&delivery_id)? {
                bail!("text message is sending or is not in the outbox");
            }
            broadcast_text_outbox(shared);
            Ok(ServerEvent::Ack)
        }
        Command::CreatePoll {
            chat_jid,
            question,
            options,
            selectable_count,
            correct_option_index,
        } => {
            let ValidatedPoll {
                question,
                options,
                selectable_count,
                correct_option_index,
            } = validate_poll_request(&question, options, selectable_count, correct_option_index)?;
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
                            voter_jids: Vec::new(),
                        })
                        .collect(),
                    selectable_count,
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
            // The requesting connection already learns about the poll from
            // this command's `sent` response, so other clients only need a
            // refresh instead of a second copy of the same message.
            broadcast_messages(shared, &message.chat_jid);
            broadcast_chats(shared);
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
        Command::DownloadMedia {
            chat_jid,
            message_id,
        } => start_media_download(shared, chat_jid, message_id).await,
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
            let _outbox_guard = shared.read_outbox_gate.lock().await;
            let chat_jid = canonical_requested_jid(shared, &chat_jid).await;
            let receipts = shared.database.unread_receipts(&chat_jid)?;
            shared
                .database
                .queue_read_receipts(&chat_jid, &receipts, Utc::now().timestamp())?;
            shared.database.mark_read(&chat_jid)?;
            shared.read_outbox_notify.notify_one();
            let total = shared.database.unread_total()?;
            broadcast_chats(shared);
            shared.publish(ServerEvent::Unread { total });
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
            shared
                .database
                .apply_pin_at(&chat_jid, pinned, Utc::now().timestamp())?;
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
            // Resolve the alias first: the shell renders the same contact under
            // its LID and its phone number, and deduplicating the raw string
            // would fetch that one avatar twice.
            let canonical = canonical_contact_jid(shared, &client, &requested).await;
            let target: Jid = canonical.parse().context("invalid canonical avatar JID")?;
            {
                // Avatar requests are advisory: a queued or dropped one is
                // re-requested the next time the contact is rendered, and the
                // `avatars` broadcast announces every result.
                let mut fetches = shared.avatar_fetches.lock().await;
                if fetches.contains(&canonical) || fetches.len() >= jobs::MAX_PENDING_AVATAR_FETCHES
                {
                    return Ok(ServerEvent::Ack);
                }
                fetches.insert(canonical.clone());
            }
            let shared = Arc::clone(shared);
            tokio::spawn(async move {
                fetch_requested_avatar(&shared, client, target).await;
                shared.avatar_fetches.lock().await.remove(&canonical);
            });
            Ok(ServerEvent::Ack)
        }
        Command::ListAvatars => Ok(shared.avatar_snapshot()),
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
            let (before, after) = shared.set_connection_active_chat(connection_id, next)?;
            reconcile_connection_intent(shared, &before, &after).await;
            Ok(ServerEvent::Ack)
        }
        Command::SetPresence { available } => {
            let (before, after) = shared.set_connection_available(connection_id, available)?;
            reconcile_connection_intent(shared, &before, &after).await;
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
        Command::ResyncChatState => {
            if !matches!(*shared.status.read().await, ConnectionStatus::Connected) {
                bail!("WhatsApp must be connected before chat state can be resynchronized");
            }
            if shared
                .chat_state_resync_requested
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                bail!("a WhatsApp chat-state resync is already in progress");
            }
            if let Err(error) = std::fs::remove_file(&shared.event_sync_marker)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                shared
                    .chat_state_resync_requested
                    .store(false, Ordering::SeqCst);
                shared
                    .set_chat_state_resync(
                        ChatStateResyncStatus::Failed,
                        Some("Could not schedule the WhatsApp chat-state replay".to_owned()),
                    )
                    .await;
                return Err(error).context("arming WhatsApp chat-state resync");
            }
            shared
                .set_chat_state_resync(
                    ChatStateResyncStatus::Requested,
                    Some("Chat-state resync requested".to_owned()),
                )
                .await;
            shared.chat_state_resync_notify.notify_one();
            Ok(ServerEvent::Ack)
        }
        Command::Logout => {
            let client = shared
                .client
                .read()
                .await
                .clone()
                .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
            let trigger = shared
                .take_logout_trigger()
                .ok_or_else(|| anyhow!("a WhatsApp logout is already in progress"))?;
            shared.logout_requested.store(true, Ordering::SeqCst);
            client.logout().await;
            // Stopping the run loop here keeps the armed flag from outliving
            // this request: the account wipe happens now or not at all.
            let _ = trigger.send(());
            Ok(ServerEvent::Ack)
        }
        Command::Ping => Ok(ServerEvent::Pong),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::state::write_private_marker;
    use crate::test_support::test_shared;
    use std::collections::HashSet;

    #[tokio::test]
    async fn offline_presence_and_active_chat_intent_are_retained() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let connection_id = shared.open_connection();

        assert_eq!(
            handle_command(
                Command::SetPresence { available: true },
                &shared,
                connection_id,
            )
            .await
            .unwrap(),
            ServerEvent::Ack
        );
        assert!(shared.connection_state().available);

        assert_eq!(
            handle_command(
                Command::SetActiveChat {
                    chat_jid: Some("1@s.whatsapp.net".into()),
                },
                &shared,
                connection_id,
            )
            .await
            .unwrap(),
            ServerEvent::Ack
        );
        assert_eq!(
            shared.connection_state().active_chats,
            HashSet::from(["1@s.whatsapp.net".into()])
        );
        assert!(
            handle_command(
                Command::SetActiveChat {
                    chat_jid: Some("not a jid".into()),
                },
                &shared,
                connection_id,
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn chat_state_resync_requires_connection_and_arms_one_controlled_restart() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        assert!(
            handle_command(Command::ResyncChatState, &shared, 0)
                .await
                .is_err()
        );

        *shared.status.write().await = ConnectionStatus::Connected;
        write_private_marker(&shared.event_sync_marker).unwrap();
        let mut events = shared.events.subscribe();
        assert_eq!(
            handle_command(Command::ResyncChatState, &shared, 0)
                .await
                .unwrap(),
            ServerEvent::Ack
        );
        assert!(!shared.event_sync_marker.exists());
        assert!(shared.chat_state_resync_requested.load(Ordering::SeqCst));
        assert_eq!(
            events.recv().await.unwrap().event,
            ServerEvent::ChatStateResync {
                status: ChatStateResyncStatus::Requested,
                message: Some("Chat-state resync requested".into()),
            }
        );
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            shared.chat_state_resync_notify.notified(),
        )
        .await
        .unwrap();
        assert!(
            handle_command(Command::ResyncChatState, &shared, 0)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn command_errors_keep_request_identity_when_stamped() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let _ = shared.clock.begin_generation();
        let error = handle_command(
            Command::SendMessage {
                chat_jid: "chat@s.whatsapp.net".into(),
                text: "  ".into(),
                delivery_id: "synthetic".into(),
            },
            &shared,
            0,
        )
        .await
        .unwrap_err();
        let response = shared.response(
            Some(4),
            ServerEvent::Error {
                message: error.to_string(),
            },
        );
        assert_eq!(response.id, Some(4));
        assert_eq!(
            response.event,
            ServerEvent::Error {
                message: "message cannot be empty".into(),
            }
        );
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
        assert!(finish_recovery_attempt(&shared, "chat@s.whatsapp.net", false).await);
        assert!(shared.media_recovery_requested.read().await.is_empty());

        shared
            .media_recovery_requested
            .write()
            .await
            .insert("chat@s.whatsapp.net".into());
        assert!(!finish_recovery_attempt(&shared, "chat@s.whatsapp.net", true).await);
        assert!(
            shared
                .media_recovery_requested
                .read()
                .await
                .contains("chat@s.whatsapp.net")
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

        let event = handle_command(Command::ListAvatars, &shared, 0)
            .await
            .unwrap();

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
    fn media_download_kind_is_derived_from_stored_media() {
        for (kind, label) in [
            (MediaDownloadKind::Image, "image"),
            (MediaDownloadKind::Sticker, "sticker"),
            (MediaDownloadKind::Video, "video"),
            (MediaDownloadKind::Audio, "audio"),
        ] {
            assert_eq!(kind.label(), label);
            assert_eq!(MediaDownloadKind::from_label(label).unwrap().label(), label);
        }
        assert!(MediaDownloadKind::from_label("document").is_none());
    }

    #[test]
    fn poll_requests_are_validated_before_any_network_call() {
        let options = || vec!["Soup".to_owned(), " Salad ".to_owned()];
        let poll = validate_poll_request(" Lunch? ", options(), 2, None).unwrap();
        assert_eq!(poll.question, "Lunch?");
        assert_eq!(poll.options, ["Soup", "Salad"]);
        assert_eq!(poll.selectable_count, 2);
        assert_eq!(poll.correct_option_index, None);

        // A quiz has one right answer, so its selectable count is normalized
        // regardless of what the caller asked for.
        let quiz = validate_poll_request("Capital?", options(), 2, Some(1)).unwrap();
        assert_eq!(quiz.selectable_count, 1);
        assert_eq!(quiz.correct_option_index, Some(1));

        let longest_question = "q".repeat(MAX_POLL_QUESTION_CHARS);
        assert!(validate_poll_request(&longest_question, options(), 1, None).is_ok());
        let longest_option = "o".repeat(MAX_POLL_OPTION_CHARS);
        let widest: Vec<String> = (0..MAX_POLL_OPTIONS)
            .map(|index| format!("option {index}"))
            .collect();
        assert!(validate_poll_request("q", widest, 1, None).is_ok());

        assert!(validate_poll_request("   ", options(), 1, None).is_err());
        assert!(
            validate_poll_request(&format!("{longest_question}q"), options(), 1, None).is_err()
        );
        assert!(validate_poll_request("q", vec!["only".into()], 1, None).is_err());
        let too_many: Vec<String> = (0..=MAX_POLL_OPTIONS)
            .map(|index| format!("option {index}"))
            .collect();
        assert!(validate_poll_request("q", too_many, 1, None).is_err());
        assert!(validate_poll_request("q", vec!["a".into(), "  ".into()], 1, None).is_err());
        let over_long_option = vec!["a".to_owned(), format!("{longest_option}o")];
        assert!(validate_poll_request("q", over_long_option, 1, None).is_err());
        assert!(validate_poll_request("q", vec!["a".into(), " a ".into()], 1, None).is_err());
        assert!(validate_poll_request("q", options(), 0, None).is_err());
        assert!(validate_poll_request("q", options(), 3, None).is_err());
        assert!(validate_poll_request("q", options(), 1, Some(2)).is_err());
    }
}
