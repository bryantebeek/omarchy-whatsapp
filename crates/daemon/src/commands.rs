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
use crate::transport::Transport;
use crate::{assets, database, jobs, paste, text_outbox, voice_outbox};
use anyhow::{Context, Result, anyhow, bail, ensure};
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
use whatsapp_rust::wacore_binary::JidExt;
use whatsapp_rust::{SendOptions, media};

pub(crate) async fn canonical_requested_jid(shared: &Shared, raw: &str) -> String {
    let Ok(jid) = raw.parse::<Jid>() else {
        return raw.to_owned();
    };
    let client = shared.client.read().await.clone();
    match client {
        Some(client) => canonical_contact_jid(shared, client.as_ref(), &jid).await,
        None => jid.to_non_ad_string(),
    }
}

async fn media_download_payload(
    shared: &Arc<Shared>,
    transport: &Arc<dyn Transport>,
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
    request_exact_message(transport.as_ref(), &cursor)
        .await
        .with_context(|| format!("requesting exact {media_label} message"))?;
    transport
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
async fn start_media_download(
    shared: &Arc<Shared>,
    chat_jid: String,
    message_id: String,
) -> Result<ServerEvent> {
    if message_id.is_empty() || message_id.len() > 512 {
        bail!("invalid media message ID");
    }
    let transport = shared
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
                perform_media_download(&shared, transport, &chat_jid, &message_id, kind),
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

// A downloaded video is usable without its poster frame, so a failed or
// panicking preview worker is only logged. Naming the outcomes keeps that
// decision testable without spawning `ffmpeg`.
fn log_video_preview_result(result: std::result::Result<Result<bool>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            warn!(%error, "could not generate downloaded video preview");
        }
        Err(error) => warn!(%error, "video preview worker panicked"),
    }
}

async fn perform_media_download(
    shared: &Arc<Shared>,
    transport: Arc<dyn Transport>,
    chat_jid: &str,
    message_id: &str,
    kind: MediaDownloadKind,
) -> Result<MessageMedia> {
    let payload =
        media_download_payload(shared, &transport, chat_jid, message_id, kind.label()).await?;
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
            assets::download_message_image(transport, image, path).await?;
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
            assets::download_message_sticker(transport, sticker, path).await?;
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
            assets::download_message_video(transport, video, path.clone()).await?;
            let preview_result = tokio::task::spawn_blocking(move || {
                assets::refresh_message_video_thumbnail(&path, &thumbnail_path)
            })
            .await;
            log_video_preview_result(preview_result);
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
            assets::download_message_audio(transport, audio, path).await?;
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
async fn fetch_requested_avatar(
    shared: &Arc<Shared>,
    transport: Arc<dyn Transport>,
    canonical: Jid,
) {
    let Ok(_permit) = shared.avatar_fetch_permits.acquire().await else {
        return;
    };
    let canonical_jid = canonical.to_non_ad_string();
    if tokio::time::timeout(
        jobs::AVATAR_FETCH_TIMEOUT,
        refresh_avatar(Arc::clone(shared), transport, canonical, false),
    )
    .await
    .is_err()
    {
        warn!(%canonical_jid, "WhatsApp avatar fetch timed out");
    }
}

async fn request_exact_message(
    transport: &dyn Transport,
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
        id: cursor.message_id.clone().into(),
        timestamp,
        ..Default::default()
    });
    transport.request_placeholder_resend(&info).await
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

fn schedule_message_recovery(
    shared: &Arc<Shared>,
    transport: Arc<dyn Transport>,
    cursor: database::HistoryCursor,
    recovery_key: String,
    label: &'static str,
) {
    let recovery_shared = Arc::clone(shared);
    let generation = shared.clock.generation();
    tokio::spawn(async move {
        let chat = cursor.chat_jid.parse::<Jid>();
        let exact_result = request_exact_message(transport.as_ref(), &cursor).await;
        let history_result = match chat {
            Ok(chat) => {
                transport
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

// Drives one recording through the durable outbox: prepare, assign a delivery
// identity, upload, send, cache, and record the outcome. The job transitions
// and the outbox retention bounds themselves live in `voice_outbox`.
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
        let transport = shared
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
        let canonical = canonical_contact_jid(shared, transport.as_ref(), &requested).await;
        let jid: Jid = canonical.parse().context("invalid canonical chat JID")?;
        let delivery_id = prepared
            .job
            .message_id
            .clone()
            .unwrap_or_else(|| transport.generate_message_id());
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
        let duration_seconds =
            u32::try_from(prepared.job.duration_ms.div_ceil(1_000)).unwrap_or(u32::MAX);
        let outbound = transport
            .upload_audio_message(
                std::mem::take(&mut prepared.bytes),
                media::AudioOptions {
                    mimetype: Some("audio/ogg; codecs=opus".into()),
                    duration_seconds: Some(duration_seconds),
                    ptt: Some(true),
                    ..Default::default()
                },
            )
            .await
            .context("uploading voice message")?;
        let sent = transport
            .send_message(
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
            quote: None,
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

const MAX_IMAGE_CAPTION_CHARS: usize = 1024;

fn validate_image_caption(caption: &str) -> Result<String> {
    let caption = caption.trim().to_owned();
    ensure!(
        caption.chars().count() <= MAX_IMAGE_CAPTION_CHARS,
        "caption is too large"
    );
    Ok(caption)
}

async fn paste_image(shared: &Arc<Shared>) -> Result<ServerEvent> {
    let clipboard = shared.clipboard.clone();
    let media_dir = shared.media_dir.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        paste::paste_image_from_clipboard(&*clipboard, &media_dir)
    })
    .await
    .context("clipboard paste task failed")??;
    match outcome {
        paste::PasteOutcome::Empty => Ok(ServerEvent::ImagePasteEmpty),
        paste::PasteOutcome::Pasted {
            path,
            width,
            height,
            mime_type,
        } => Ok(ServerEvent::ImagePasted {
            path: path.to_string_lossy().into_owned(),
            width,
            height,
            mime_type,
        }),
    }
}

async fn resolve_reply(
    shared: &Shared,
    chat: &Jid,
    target: Option<omarchy_whatsapp_protocol::ReplyTarget>,
) -> Result<Option<omarchy_whatsapp_protocol::MessageQuote>> {
    let Some(target) = target else {
        return Ok(None);
    };
    ensure!(
        !target.message_id.is_empty() && target.message_id.len() <= 256,
        "invalid reply message ID"
    );
    let message = shared
        .database
        .message_by_identity(
            &chat.to_non_ad_string(),
            &target.message_id,
            &target.sender_jid,
        )?
        .context("reply target is no longer available in this conversation")?;
    let transport = shared.client.read().await.clone();
    let mut sender_jid = if message.from_me {
        let transport = transport.as_ref().context("WhatsApp is not connected")?;
        own_poll_creator_jid(transport.as_ref(), chat)
            .await?
            .to_non_ad_string()
    } else {
        message
            .sender_jid
            .parse::<Jid>()
            .context("invalid reply participant")?
            .to_non_ad_string()
    };
    if let Some(transport) = transport {
        sender_jid = crate::identity::quote_participant(transport.as_ref(), chat, sender_jid).await;
    }
    Ok(Some(omarchy_whatsapp_protocol::MessageQuote {
        message_id: message.id,
        sender_jid,
        sender_name: message.sender_name,
        text: message.text,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn send_image(
    shared: &Arc<Shared>,
    chat_jid: String,
    path: String,
    caption: String,
    delivery_id: String,
    mentions: Vec<String>,
    reply_to: Option<omarchy_whatsapp_protocol::ReplyTarget>,
) -> Result<ServerEvent> {
    let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
    let mentions = text_outbox::validate_mentions(&requested, &caption, mentions)?;
    text_outbox::validate_delivery_id(&delivery_id)?;
    let caption = validate_image_caption(&caption)?;
    let staged = paste::resolve_staged_image(&shared.media_dir, &path)?;
    let bytes = std::fs::read(&staged).context("staged image is not available")?;
    let mime_type = paste::sniff_image_mime(&bytes)
        .context("staged image is not a supported image")?
        .to_owned();
    paste::validate_pasted_bytes(&bytes, &mime_type)?;
    let (width, height) = paste::pasted_image_dimensions(&bytes, &mime_type)?;
    let transport = shared
        .client
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow!("WhatsApp is not connected"))?;
    let canonical = canonical_contact_jid(shared, transport.as_ref(), &requested).await;
    let jid: Jid = canonical.parse().context("invalid canonical chat JID")?;
    let canonical_jid = jid.to_non_ad_string();
    let quote = resolve_reply(shared, &jid, reply_to).await?;
    let message_id = text_outbox::stable_message_id(&delivery_id);
    if let Some(message) = shared.database.message_by_id(&canonical_jid, &message_id)? {
        return Ok(ServerEvent::Sent { message });
    }
    let mut outbound = transport
        .upload_image_message(
            bytes,
            media::ImageOptions {
                caption: (!caption.is_empty()).then(|| caption.clone()),
                mimetype: Some(mime_type.clone()),
                ..Default::default()
            },
        )
        .await
        .context("uploading image")?;
    if let Some(image) = outbound.image_message.as_option_mut() {
        image.width = Some(width);
        image.height = Some(height);
        image.context_info = buffa::MessageField::some(wa::ContextInfo {
            mentioned_jid: mentions,
            ..quote
                .as_ref()
                .map_or_else(wa::ContextInfo::default, text_outbox::quote_context)
        });
    }
    let sent = transport
        .send_message(
            &jid,
            outbound,
            SendOptions::default().with_message_id(&message_id),
        )
        .await?;
    if sent.message_id != message_id {
        bail!("WhatsApp returned a different image message ID");
    }
    let cached_path = assets::message_image_path(&shared.media_dir, &canonical_jid, &message_id);
    let copy_source = staged.clone();
    let copy_destination = cached_path.clone();
    let downloaded = match tokio::task::spawn_blocking(move || {
        assets::copy_private_file(&copy_source, &copy_destination)
    })
    .await
    .context("image cache copy task failed")?
    {
        Ok(()) => true,
        Err(error) => {
            warn!(%error, "could not retain sent image in the private cache");
            false
        }
    };
    assets::prune_media_cache(
        &shared.media_dir,
        if downloaded { &cached_path } else { &staged },
    );
    let cached = cached_path.to_string_lossy().into_owned();
    let message = Message {
        id: message_id,
        chat_jid: canonical_jid,
        sender_jid: "me".into(),
        sender_name: "You".into(),
        text: if caption.is_empty() {
            "[Image]".into()
        } else {
            caption
        },
        timestamp: Utc::now().timestamp(),
        from_me: true,
        receipt: 1,
        delivered_at: None,
        read_at: None,
        delivered_to: Vec::new(),
        read_by: Vec::new(),
        media: Some(MessageMedia::Image {
            // Sent images have no separate thumbnail; pointing at the cached
            // copy keeps history recovery from ever flipping them back to
            // undownloaded.
            path: cached.clone(),
            thumbnail_path: cached,
            downloaded,
            mime_type,
            width,
            height,
        }),
        reactions: Vec::new(),
        quote,
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
    Ok(ServerEvent::Sent { message })
}

// IPC command-to-`WhatsApp` dispatch. Command identity, deadlines,
// serialization, and response convergence belong to `ipc`; every arm below owns
// only its validation, its outbound calls, and the local state it publishes.
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
                .group_metadata(&jid)
                .await
                .context("loading WhatsApp group participants")?;
            Ok(ServerEvent::GroupParticipants {
                chat_jid: jid.to_non_ad_string(),
                participants: resolve_group_participants(
                    shared,
                    client.as_ref(),
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
            mentions,
            reply_to,
        } => {
            let text = text_outbox::validate(&delivery_id, &text)?;
            let requested: Jid = chat_jid.parse().context("invalid chat JID")?;
            let mentions = text_outbox::validate_mentions(&requested, &text, mentions)?;
            let quote = resolve_reply(shared, &requested, reply_to).await?;
            let message_id = text_outbox::stable_message_id(&delivery_id);
            let _outbox_guard = shared.text_outbox_gate.lock().await;
            shared.database.enqueue_text_message(
                &delivery_id,
                &requested.to_non_ad_string(),
                &text,
                &message_id,
                Utc::now().timestamp(),
                &mentions,
                quote.as_ref(),
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
        Command::PasteImage => paste_image(shared).await,
        Command::SendImage {
            chat_jid,
            path,
            caption,
            delivery_id,
            mentions,
            reply_to,
        } => {
            send_image(
                shared,
                chat_jid,
                path,
                caption,
                delivery_id,
                mentions,
                reply_to,
            )
            .await
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
            let canonical = canonical_contact_jid(shared, client.as_ref(), &requested).await;
            let jid: Jid = canonical.parse().context("invalid canonical chat JID")?;
            let creator_jid = own_poll_creator_jid(client.as_ref(), &jid).await?;
            let (result, message_secret) = if let Some(correct_index) = correct_option_index {
                client
                    .create_quiz(
                        jid.clone(),
                        &question,
                        &options,
                        usize::try_from(correct_index).context("invalid correct option index")?,
                    )
                    .await?
            } else {
                client
                    .create_poll(jid.clone(), &question, &options, selectable_count)
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
                quote: None,
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
                .vote_poll(
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
            let chat_jid = canonical_contact_jid(shared, client.as_ref(), &requested).await;
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
            let chat_jid = canonical_contact_jid(shared, client.as_ref(), &requested).await;
            let chat: Jid = chat_jid.parse().context("invalid canonical chat JID")?;
            if pinned {
                client.pin_chat(&chat).await?;
            } else {
                client.unpin_chat(&chat).await?;
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
            let canonical = canonical_contact_jid(shared, client.as_ref(), &requested).await;
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
                        Some(client) => {
                            canonical_contact_jid(shared, client.as_ref(), &requested).await
                        }
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
            let canonical = canonical_contact_jid(shared, client.as_ref(), &requested).await;
            let jid: Jid = canonical
                .parse()
                .context("invalid canonical chat-state JID")?;
            match state {
                ChatState::Typing => client.send_composing(&jid).await?,
                ChatState::Recording => client.send_recording(&jid).await?,
                ChatState::Paused => client.send_paused(&jid).await?,
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
    use crate::transport::fake::{
        Call, CallKind, FakeTransport, MediaKind, transport as fake_transport,
    };
    use omarchy_whatsapp_protocol::{Resource, TextOutboxStatus, VoiceOutboxStatus};
    use std::collections::HashSet;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use whatsapp_rust::GroupMetadata;

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
                mentions: Vec::new(),
                reply_to: None,
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

    // --- shared fixtures -------------------------------------------------

    fn shared_with_dirs(directory: &tempfile::TempDir) -> Arc<Shared> {
        let shared = Arc::new(test_shared(directory));
        assets::private_dir(&shared.avatar_dir).unwrap();
        assets::private_dir(&shared.media_dir).unwrap();
        assets::private_dir(&shared.voice_outbox_dir).unwrap();
        shared
    }

    async fn attach(shared: &Arc<Shared>, fake: &Arc<FakeTransport>) {
        *shared.client.write().await = Some(fake_transport(fake));
    }

    async fn run(shared: &Arc<Shared>, command: Command) -> Result<ServerEvent> {
        handle_command(command, shared, 0).await
    }

    fn stored_message(chat_jid: &str, id: &str, media: Option<MessageMedia>) -> Message {
        Message {
            id: id.to_owned(),
            chat_jid: chat_jid.to_owned(),
            sender_jid: chat_jid.to_owned(),
            sender_name: "Ada".into(),
            text: "synthetic".into(),
            timestamp: 1_700_000_000,
            from_me: false,
            receipt: 0,
            delivered_at: None,
            read_at: None,
            delivered_to: Vec::new(),
            read_by: Vec::new(),
            media,
            reactions: Vec::new(),
            quote: None,
        }
    }

    /// Lets already-spawned background jobs finish; the fake transport never
    /// blocks, so a bounded number of yields is enough.
    async fn settle() {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    // Synthetic Ogg Opus, mirroring the generator in `voice_outbox`'s tests so
    // that no captured recording enters these fixtures.
    fn ogg_page(sequence: u32, granule: u64, body: &[u8]) -> Vec<u8> {
        let mut page = Vec::with_capacity(28 + body.len());
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(if sequence == 0 { 2 } else { 0 });
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&7u32.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(1);
        page.push(u8::try_from(body.len()).unwrap());
        page.extend_from_slice(body);
        page
    }

    fn recording(duration_ms: u64) -> Vec<u8> {
        let pre_skip = 312u16;
        let mut opus_head = b"OpusHead".to_vec();
        opus_head.extend_from_slice(&[1, 1]);
        opus_head.extend_from_slice(&pre_skip.to_le_bytes());
        opus_head.extend_from_slice(&48_000u32.to_le_bytes());
        opus_head.extend_from_slice(&0u16.to_le_bytes());
        opus_head.push(0);
        let mut bytes = ogg_page(0, 0, &opus_head);
        bytes.extend(ogg_page(
            1,
            duration_ms * 48 + u64::from(pre_skip),
            b"synthetic opus packet",
        ));
        bytes
    }

    fn write_recording(shared: &Arc<Shared>, recording_id: &str, duration_ms: u64) {
        let path = voice_outbox::recording_path(&shared.voice_outbox_dir, recording_id).unwrap();
        std::fs::write(path, recording(duration_ms)).unwrap();
    }

    fn seed_media(shared: &Arc<Shared>, chat: &str, id: &str, media: MessageMedia, payload: &[u8]) {
        let message = stored_message(chat, id, Some(media));
        shared
            .database
            .insert_message(&message, "Ada", false, false)
            .unwrap();
        shared
            .database
            .store_media_download(chat, id, payload)
            .unwrap();
    }

    async fn download_outcome(shared: &Arc<Shared>, chat: &str, id: &str) -> ServerEvent {
        let mut events = shared.events.subscribe();
        let command = Command::DownloadMedia {
            chat_jid: chat.to_owned(),
            message_id: id.to_owned(),
        };
        assert_eq!(run(shared, command).await.unwrap(), ServerEvent::Ack);
        loop {
            let frame = events.recv().await.unwrap();
            if matches!(
                frame.event,
                ServerEvent::MediaDownloaded { .. } | ServerEvent::MediaDownloadFailed { .. }
            ) {
                return frame.event;
            }
        }
    }

    // --- dispatch ---------------------------------------------------------

    #[tokio::test]
    async fn commands_that_talk_to_whatsapp_are_rejected_while_unlinked() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let chat = "1@s.whatsapp.net";
        for command in [
            Command::GetGroupParticipants {
                chat_jid: "123-456@g.us".into(),
            },
            Command::CreatePoll {
                chat_jid: chat.into(),
                question: "Lunch?".into(),
                options: vec!["Soup".into(), "Salad".into()],
                selectable_count: 1,
                correct_option_index: None,
            },
            Command::VotePoll {
                chat_jid: chat.into(),
                message_id: "poll".into(),
                selected_options: Vec::new(),
            },
            Command::DownloadMedia {
                chat_jid: chat.into(),
                message_id: "image".into(),
            },
            Command::React {
                chat_jid: chat.into(),
                message_id: "image".into(),
                sender_jid: chat.into(),
                target_from_me: false,
                emoji: "👍".into(),
            },
            Command::SetChatPinned {
                chat_jid: chat.into(),
                pinned: true,
            },
            Command::RequestAvatar { jid: chat.into() },
            Command::SetChatState {
                chat_jid: chat.into(),
                state: ChatState::Typing,
            },
            Command::Logout,
        ] {
            assert_eq!(
                run(&shared, command).await.unwrap_err().to_string(),
                "WhatsApp is not connected"
            );
        }
    }

    #[tokio::test]
    async fn local_queries_answer_without_a_linked_device() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);

        assert!(matches!(
            run(&shared, Command::GetState).await.unwrap(),
            ServerEvent::State { .. }
        ));
        assert_eq!(
            run(&shared, Command::Ping).await.unwrap(),
            ServerEvent::Pong
        );
        assert_eq!(
            run(&shared, Command::ListChats { limit: 10 })
                .await
                .unwrap(),
            ServerEvent::Chats { chats: Vec::new() }
        );
        assert_eq!(
            run(&shared, Command::ListVoiceOutbox).await.unwrap(),
            ServerEvent::VoiceOutbox {
                entries: Vec::new()
            }
        );
        assert_eq!(
            run(&shared, Command::ListTextOutbox).await.unwrap(),
            ServerEvent::TextOutbox {
                entries: Vec::new()
            }
        );
    }

    #[tokio::test]
    async fn participant_lists_are_only_available_for_groups() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let group = "123-456@g.us";
        let fake = Arc::new(FakeTransport::new().with_group_metadata(
            group,
            GroupMetadata {
                subject: "Garden".into(),
                ..GroupMetadata::default()
            },
        ));
        attach(&shared, &fake).await;

        assert!(
            run(
                &shared,
                Command::GetGroupParticipants {
                    chat_jid: "not a jid".into(),
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            run(
                &shared,
                Command::GetGroupParticipants {
                    chat_jid: "1@s.whatsapp.net".into(),
                }
            )
            .await
            .unwrap_err()
            .to_string(),
            "participant lists are only available for group chats"
        );
        assert_eq!(
            run(
                &shared,
                Command::GetGroupParticipants {
                    chat_jid: group.into(),
                }
            )
            .await
            .unwrap(),
            ServerEvent::GroupParticipants {
                chat_jid: group.into(),
                participants: Vec::new(),
            }
        );
        assert_eq!(
            fake.calls_of(CallKind::GroupMetadata),
            vec![Call::GroupMetadata(group.into())]
        );

        fake.fail(CallKind::GroupMetadata, "offline");
        assert_eq!(
            run(
                &shared,
                Command::GetGroupParticipants {
                    chat_jid: group.into(),
                }
            )
            .await
            .unwrap_err()
            .to_string(),
            "loading WhatsApp group participants"
        );
    }

    #[tokio::test]
    async fn getting_messages_schedules_media_and_poll_recovery_once() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new().with_history_request_id("request-1"));
        attach(&shared, &fake).await;
        let _ = shared.clock.begin_generation();
        let chat = "1@s.whatsapp.net";

        let mut image = stored_message(chat, "image-1", None);
        image.text = "[Image]".into();
        shared
            .database
            .insert_message(&image, "Ada", false, false)
            .unwrap();
        let mut poll = stored_message(
            chat,
            "poll-1",
            Some(MessageMedia::Poll {
                question: "Lunch?".into(),
                options: vec![PollOption {
                    name: "Soup".into(),
                    votes: 0,
                    selected_by_me: false,
                    voter_jids: Vec::new(),
                }],
                selectable_count: 1,
                total_voters: 0,
                quiz: false,
                correct_option_index: None,
                end_timestamp: 0,
            }),
        );
        poll.timestamp += 1;
        shared
            .database
            .insert_message(&poll, "Ada", false, false)
            .unwrap();

        let event = run(
            &shared,
            Command::GetMessages {
                chat_jid: "1:2@s.whatsapp.net".into(),
                limit: 10,
            },
        )
        .await
        .unwrap();
        let ServerEvent::Messages {
            chat_jid, messages, ..
        } = event
        else {
            panic!("expected a message list");
        };
        assert_eq!(chat_jid, chat);
        assert_eq!(messages.len(), 2);
        settle().await;

        assert_eq!(fake.calls_of(CallKind::FetchMessageHistory).len(), 2);
        assert_eq!(fake.calls_of(CallKind::RequestPlaceholderResend).len(), 2);
        let requested = shared.media_recovery_requested.read().await.clone();
        assert_eq!(
            requested,
            HashSet::from([chat.to_owned(), format!("poll:{chat}")])
        );

        // A successful attempt is remembered, so a refresh does not re-request.
        fake.clear_calls();
        run(
            &shared,
            Command::GetMessages {
                chat_jid: chat.into(),
                limit: 10,
            },
        )
        .await
        .unwrap();
        settle().await;
        assert!(fake.calls_of(CallKind::FetchMessageHistory).is_empty());
    }

    #[tokio::test]
    async fn recovery_is_rearmed_after_a_total_failure_and_after_a_client_restart() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let _ = shared.clock.begin_generation();
        let cursor = database::HistoryCursor {
            chat_jid: "1@s.whatsapp.net".into(),
            message_id: "image-1".into(),
            sender_jid: "1@s.whatsapp.net".into(),
            from_me: false,
            timestamp_ms: 1_700_000_000_000,
        };

        // Neither request reached WhatsApp, so the next refresh may retry.
        let broken = Arc::new(
            FakeTransport::new()
                .failing(CallKind::RequestPlaceholderResend, "no primary device")
                .failing(CallKind::FetchMessageHistory, "offline"),
        );
        shared
            .media_recovery_requested
            .write()
            .await
            .insert("failed".into());
        schedule_message_recovery(
            &shared,
            fake_transport(&broken),
            cursor.clone(),
            "failed".into(),
            "media",
        );
        settle().await;
        assert!(
            !shared
                .media_recovery_requested
                .read()
                .await
                .contains("failed")
        );

        // An unparsable chat cannot even build the history request.
        let unusable = database::HistoryCursor {
            chat_jid: "not a jid".into(),
            ..cursor.clone()
        };
        shared
            .media_recovery_requested
            .write()
            .await
            .insert("unusable".into());
        schedule_message_recovery(
            &shared,
            fake_transport(&Arc::new(FakeTransport::new())),
            unusable,
            "unusable".into(),
            "media",
        );
        settle().await;
        assert!(
            !shared
                .media_recovery_requested
                .read()
                .await
                .contains("unusable")
        );

        // A result that belongs to a retired client is dropped rather than
        // recorded as a completed attempt.
        let stale = Arc::new(FakeTransport::new());
        shared
            .media_recovery_requested
            .write()
            .await
            .insert("stale".into());
        schedule_message_recovery(
            &shared,
            fake_transport(&stale),
            cursor,
            "stale".into(),
            "poll metadata",
        );
        let _ = shared.clock.begin_generation();
        settle().await;
        assert!(
            !shared
                .media_recovery_requested
                .read()
                .await
                .contains("stale")
        );
        assert_eq!(stale.calls_of(CallKind::FetchMessageHistory).len(), 1);
    }

    #[tokio::test]
    async fn a_recovery_request_names_the_exact_message_and_its_author() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        drop(shared);
        let fake = Arc::new(FakeTransport::new());
        let transport = fake_transport(&fake);
        let mine = database::HistoryCursor {
            chat_jid: "1@s.whatsapp.net".into(),
            message_id: "mine".into(),
            sender_jid: "unused".into(),
            from_me: true,
            timestamp_ms: 1_700_000_000_000,
        };
        let theirs = database::HistoryCursor {
            message_id: "theirs".into(),
            sender_jid: "1@s.whatsapp.net".into(),
            from_me: false,
            ..mine.clone()
        };

        request_exact_message(transport.as_ref(), &mine)
            .await
            .unwrap();
        request_exact_message(transport.as_ref(), &theirs)
            .await
            .unwrap();
        let broken = database::HistoryCursor {
            sender_jid: "not a jid".into(),
            ..theirs
        };
        assert!(
            request_exact_message(transport.as_ref(), &broken)
                .await
                .is_err()
        );

        assert_eq!(
            fake.calls_of(CallKind::RequestPlaceholderResend),
            vec![
                Call::RequestPlaceholderResend {
                    chat: "1@s.whatsapp.net".into(),
                    message_id: "mine".into(),
                },
                Call::RequestPlaceholderResend {
                    chat: "1@s.whatsapp.net".into(),
                    message_id: "theirs".into(),
                },
            ]
        );
    }

    // --- text outbox ------------------------------------------------------

    #[tokio::test]
    async fn replies_resolve_exact_chat_and_participant_before_durable_acceptance() {
        use omarchy_whatsapp_protocol::ReplyTarget;
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let mut original = stored_message("123@g.us", "ORIGINAL", None);
        original.sender_jid = "200@lid".into();
        original.text = "Question".into();
        shared
            .database
            .insert_message(&original, "Group", true, false)
            .unwrap();
        let target = ReplyTarget {
            message_id: original.id.clone(),
            sender_jid: original.sender_jid.clone(),
        };
        for (chat, reply_to) in [
            (
                "123@g.us",
                ReplyTarget {
                    message_id: String::new(),
                    ..target.clone()
                },
            ),
            (
                "123@g.us",
                ReplyTarget {
                    message_id: "x".repeat(257),
                    ..target.clone()
                },
            ),
            (
                "123@g.us",
                ReplyTarget {
                    sender_jid: "300@lid".into(),
                    ..target.clone()
                },
            ),
            ("456@g.us", target.clone()),
        ] {
            assert!(
                run(
                    &shared,
                    Command::SendMessage {
                        chat_jid: chat.into(),
                        text: "Reply".into(),
                        delivery_id: "reply".into(),
                        mentions: vec![],
                        reply_to: Some(reply_to)
                    }
                )
                .await
                .is_err()
            );
        }
        run(
            &shared,
            Command::SendMessage {
                chat_jid: "123@g.us".into(),
                text: "Reply".into(),
                delivery_id: "reply".into(),
                mentions: vec![],
                reply_to: Some(target.clone()),
            },
        )
        .await
        .unwrap();
        let pending = shared.database.claim_text_message().unwrap().unwrap();
        let quote = pending.quote.unwrap();
        assert_eq!(quote.message_id, "ORIGINAL");
        assert_eq!(quote.sender_jid, "200@lid");
        assert_eq!(quote.sender_name, "Ada");
        assert_eq!(quote.text, "Question");

        let direct = stored_message("200@s.whatsapp.net", "DIRECT", None);
        shared
            .database
            .insert_message(&direct, "Ada", false, false)
            .unwrap();
        let target_direct = ReplyTarget {
            message_id: "DIRECT".into(),
            sender_jid: "200@s.whatsapp.net".into(),
        };
        assert_eq!(
            resolve_reply(
                &shared,
                &"200@s.whatsapp.net".parse().unwrap(),
                Some(target_direct)
            )
            .await
            .unwrap()
            .unwrap()
            .text,
            "synthetic"
        );
        original.id = "OWN".into();
        original.sender_jid = "me".into();
        original.from_me = true;
        shared
            .database
            .insert_message(&original, "Group", true, false)
            .unwrap();
        let own = ReplyTarget {
            message_id: "OWN".into(),
            sender_jid: "me".into(),
        };
        let group = "123@g.us".parse().unwrap();
        assert!(
            resolve_reply(&shared, &group, Some(own.clone()))
                .await
                .is_err()
        );
        let fake = Arc::new(
            FakeTransport::new()
                .with_pn("100@s.whatsapp.net")
                .with_lid("100@lid"),
        );
        attach(&shared, &fake).await;
        assert_eq!(
            resolve_reply(&shared, &group, Some(own))
                .await
                .unwrap()
                .unwrap()
                .sender_jid,
            "100@s.whatsapp.net"
        );
        for lid_mode in [false, true] {
            let fake = Arc::new(
                FakeTransport::new()
                    .with_pn("100@s.whatsapp.net")
                    .with_lid("100@lid")
                    .with_group_metadata(
                        "123@g.us",
                        GroupMetadata {
                            addressing_mode: if lid_mode {
                                whatsapp_rust::wacore::types::message::AddressingMode::Lid
                            } else {
                                whatsapp_rust::wacore::types::message::AddressingMode::Pn
                            },
                            participants: vec![whatsapp_rust::GroupParticipant {
                                jid: "200@lid".parse().unwrap(),
                                phone_number: Some("200@s.whatsapp.net".parse().unwrap()),
                                lid: Some("200@lid".parse().unwrap()),
                                username: None,
                                participant_type: whatsapp_rust::ParticipantType::Member,
                                details: None,
                            }],
                            ..Default::default()
                        },
                    ),
            );
            attach(&shared, &fake).await;
            assert_eq!(
                resolve_reply(&shared, &group, Some(target.clone()))
                    .await
                    .unwrap()
                    .unwrap()
                    .sender_jid,
                if lid_mode {
                    "200@lid"
                } else {
                    "200@s.whatsapp.net"
                }
            );
            let own = omarchy_whatsapp_protocol::ReplyTarget {
                message_id: "OWN".into(),
                sender_jid: "me".into(),
            };
            assert_eq!(
                resolve_reply(&shared, &group, Some(own))
                    .await
                    .unwrap()
                    .unwrap()
                    .sender_jid,
                if lid_mode {
                    "100@lid"
                } else {
                    "100@s.whatsapp.net"
                }
            );
        }
    }

    #[tokio::test]
    async fn text_mentions_are_validated_before_durable_acceptance() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        for (chat, text) in [("1@s.whatsapp.net", "Hi @200"), ("123@g.us", "Hi @201")] {
            assert!(
                run(
                    &shared,
                    Command::SendMessage {
                        chat_jid: chat.into(),
                        text: text.into(),
                        delivery_id: "mention".into(),
                        mentions: vec!["200@lid".into()],
                        reply_to: None,
                    }
                )
                .await
                .is_err()
            );
            assert!(shared.database.text_outbox().unwrap().is_empty());
        }
        run(
            &shared,
            Command::SendMessage {
                chat_jid: "123@g.us".into(),
                text: "Hi @200".into(),
                delivery_id: "mention".into(),
                mentions: vec!["200@lid".into()],
                reply_to: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            shared
                .database
                .claim_text_message()
                .unwrap()
                .unwrap()
                .mentions,
            ["200@lid"]
        );
    }

    #[tokio::test]
    async fn text_messages_are_validated_queued_retried_and_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let accepted = ServerEvent::TextAccepted {
            delivery_id: "d1".into(),
        };

        assert_eq!(
            run(
                &shared,
                Command::SendMessage {
                    chat_jid: "1@s.whatsapp.net".into(),
                    text: "hi".into(),
                    delivery_id: String::new(),
                    mentions: Vec::new(),
                    reply_to: None,
                }
            )
            .await
            .unwrap_err()
            .to_string(),
            "delivery ID cannot be empty"
        );
        assert!(
            run(
                &shared,
                Command::SendMessage {
                    chat_jid: "not a jid".into(),
                    text: "hi".into(),
                    delivery_id: "d1".into(),
                    mentions: Vec::new(),
                    reply_to: None,
                }
            )
            .await
            .is_err()
        );

        let send = || Command::SendMessage {
            chat_jid: "1:2@s.whatsapp.net".into(),
            text: "  hi  ".into(),
            delivery_id: "d1".into(),
            mentions: Vec::new(),
            reply_to: None,
        };
        assert_eq!(run(&shared, send()).await.unwrap(), accepted);
        assert_eq!(run(&shared, send()).await.unwrap(), accepted);
        assert!(
            run(
                &shared,
                Command::SendMessage {
                    chat_jid: "1@s.whatsapp.net".into(),
                    text: "different".into(),
                    delivery_id: "d1".into(),
                    mentions: Vec::new(),
                    reply_to: None,
                }
            )
            .await
            .is_err()
        );

        let ServerEvent::TextOutbox { entries } =
            run(&shared, Command::ListTextOutbox).await.unwrap()
        else {
            panic!("expected the text outbox");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].chat_jid, "1@s.whatsapp.net");
        assert_eq!(entries[0].text, "hi");
        assert_eq!(entries[0].status, TextOutboxStatus::Queued);

        let retry = || Command::RetryTextMessage {
            delivery_id: "d1".into(),
        };
        let discard = || Command::DiscardTextMessage {
            delivery_id: "d1".into(),
        };
        assert_eq!(
            run(&shared, retry()).await.unwrap_err().to_string(),
            "text message is not waiting for retry"
        );
        shared.database.claim_text_message().unwrap().unwrap();
        assert_eq!(
            run(&shared, discard()).await.unwrap_err().to_string(),
            "text message is sending or is not in the outbox"
        );
        shared.database.fail_text_message("d1", "offline").unwrap();
        assert_eq!(run(&shared, retry()).await.unwrap(), accepted);
        assert_eq!(run(&shared, discard()).await.unwrap(), ServerEvent::Ack);
        assert!(shared.database.text_outbox().unwrap().is_empty());
    }

    // --- polls ------------------------------------------------------------

    #[tokio::test]
    async fn creating_a_poll_stores_its_secret_and_sends_exactly_one_card() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(
            FakeTransport::new()
                .with_pn("31600000000@s.whatsapp.net")
                .with_poll_secret(&[7u8; 32])
                .with_message_ids(["POLL-1", "QUIZ-1"]),
        );
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let mut events = shared.events.subscribe();
        let poll = |question: &str, correct: Option<u32>| Command::CreatePoll {
            chat_jid: chat.to_owned(),
            question: question.to_owned(),
            options: vec!["Soup".into(), " Salad ".into()],
            selectable_count: 2,
            correct_option_index: correct,
        };

        assert!(run(&shared, poll("   ", None)).await.is_err());
        assert!(
            run(
                &shared,
                Command::CreatePoll {
                    chat_jid: "not a jid".into(),
                    question: "Lunch?".into(),
                    options: vec!["Soup".into(), "Salad".into()],
                    selectable_count: 1,
                    correct_option_index: None,
                }
            )
            .await
            .is_err()
        );

        let ServerEvent::Sent { message } = run(&shared, poll(" Lunch? ", None)).await.unwrap()
        else {
            panic!("expected the poll card");
        };
        assert_eq!(message.id, "POLL-1");
        assert_eq!(message.text, "[Poll] Lunch?");
        assert_eq!(message.sender_jid, "me");
        assert_eq!(
            fake.calls_of(CallKind::CreatePoll),
            vec![Call::CreatePoll {
                chat: chat.into(),
                question: "Lunch?".into(),
                options: vec!["Soup".into(), "Salad".into()],
                selectable_count: 2,
            }]
        );
        let stored = shared
            .database
            .poll_for_voting(chat, "POLL-1")
            .unwrap()
            .unwrap();
        assert_eq!(stored.creator_jid, "31600000000@s.whatsapp.net");
        assert_eq!(stored.message_secret, [7u8; 32]);

        let ServerEvent::Sent { message } = run(&shared, poll("Capital?", Some(1))).await.unwrap()
        else {
            panic!("expected the quiz card");
        };
        assert!(matches!(
            message.media,
            Some(MessageMedia::Poll {
                quiz: true,
                selectable_count: 1,
                correct_option_index: Some(1),
                ..
            })
        ));
        assert_eq!(fake.calls_of(CallKind::CreateQuiz).len(), 1);

        // Other clients only receive a refresh, never a second copy of the card.
        let published = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert!(
            !published
                .iter()
                .any(|event| matches!(event, ServerEvent::Sent { .. }))
        );
        assert!(published.iter().any(|event| matches!(
            event,
            ServerEvent::Invalidated {
                resource: Resource::Messages,
                ..
            }
        )));

        fake.fail(CallKind::CreatePoll, "offline");
        assert!(run(&shared, poll("Dinner?", None)).await.is_err());
    }

    #[tokio::test]
    async fn voting_is_validated_against_the_stored_poll_card() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(
            FakeTransport::new()
                .with_pn("31600000000@s.whatsapp.net")
                .with_poll_secret(&[9u8; 32])
                .with_message_ids(["POLL-9"]),
        );
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        run(
            &shared,
            Command::CreatePoll {
                chat_jid: chat.into(),
                question: "Lunch?".into(),
                options: vec!["Soup".into(), "Salad".into(), "Stew".into()],
                selectable_count: 2,
                correct_option_index: None,
            },
        )
        .await
        .unwrap();
        let vote = |message_id: &str, options: &[&str]| Command::VotePoll {
            chat_jid: chat.to_owned(),
            message_id: message_id.to_owned(),
            selected_options: options.iter().map(|name| (*name).to_owned()).collect(),
        };

        for invalid in [String::new(), "x".repeat(513)] {
            assert_eq!(
                run(&shared, vote(&invalid, &[]))
                    .await
                    .unwrap_err()
                    .to_string(),
                "invalid poll message ID"
            );
        }
        assert_eq!(
            run(&shared, vote("missing", &[]))
                .await
                .unwrap_err()
                .to_string(),
            "poll details are unavailable; its history may need to be recovered"
        );
        assert_eq!(
            run(&shared, vote("POLL-9", &["Pizza"]))
                .await
                .unwrap_err()
                .to_string(),
            "poll vote contains an unknown option"
        );
        assert_eq!(
            run(&shared, vote("POLL-9", &["Soup", "Salad", "Stew"]))
                .await
                .unwrap_err()
                .to_string(),
            "poll vote selects more options than the poll allows"
        );

        assert_eq!(
            run(&shared, vote("POLL-9", &["Soup", "Soup", "Salad"]))
                .await
                .unwrap(),
            ServerEvent::Ack
        );
        assert_eq!(
            fake.calls_of(CallKind::VotePoll),
            vec![Call::VotePoll {
                chat: chat.into(),
                poll_message_id: "POLL-9".into(),
                creator_jid: "31600000000@s.whatsapp.net".into(),
                message_secret: vec![9u8; 32],
                option_names: vec!["Soup".into(), "Salad".into()],
            }]
        );
        let stored = shared
            .database
            .message_by_id(chat, "POLL-9")
            .unwrap()
            .unwrap();
        assert!(matches!(
            stored.media,
            Some(MessageMedia::Poll {
                total_voters: 1,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_closed_poll_no_longer_accepts_votes() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let message = stored_message(
            chat,
            "ended",
            Some(MessageMedia::Poll {
                question: "Lunch?".into(),
                options: vec![PollOption {
                    name: "Soup".into(),
                    votes: 0,
                    selected_by_me: false,
                    voter_jids: Vec::new(),
                }],
                selectable_count: 1,
                total_voters: 0,
                quiz: false,
                correct_option_index: None,
                end_timestamp: 1,
            }),
        );
        shared
            .database
            .insert_message(&message, "Ada", false, false)
            .unwrap();
        shared
            .database
            .store_poll_secret(chat, "ended", "31600000000@s.whatsapp.net", &[3u8; 32])
            .unwrap();

        assert_eq!(
            run(
                &shared,
                Command::VotePoll {
                    chat_jid: chat.into(),
                    message_id: "ended".into(),
                    selected_options: vec!["Soup".into()],
                }
            )
            .await
            .unwrap_err()
            .to_string(),
            "this poll has ended"
        );
        assert!(fake.calls_of(CallKind::VotePoll).is_empty());
    }

    // --- reactions, receipts, pins ----------------------------------------

    #[tokio::test]
    async fn reacting_to_a_direct_message_needs_no_participant_key() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        shared
            .database
            .insert_message(&stored_message(chat, "m1", None), "Ada", false, false)
            .unwrap();
        let react = |chat_jid: &str, message_id: &str, emoji: &str| Command::React {
            chat_jid: chat_jid.to_owned(),
            message_id: message_id.to_owned(),
            sender_jid: chat.to_owned(),
            target_from_me: false,
            emoji: emoji.to_owned(),
        };

        assert_eq!(
            run(&shared, react(chat, "", "👍"))
                .await
                .unwrap_err()
                .to_string(),
            "reaction target is missing"
        );
        assert_eq!(
            run(&shared, react(chat, "m1", "\u{1}"))
                .await
                .unwrap_err()
                .to_string(),
            "reaction must be a short emoji"
        );
        assert!(run(&shared, react("not a jid", "m1", "👍")).await.is_err());

        assert_eq!(
            run(&shared, react("1:2@s.whatsapp.net", "m1", "👍"))
                .await
                .unwrap(),
            ServerEvent::Ack
        );

        let reactions = fake.calls_of(CallKind::SendReaction);
        let [
            Call::SendReaction {
                chat: reacted_chat,
                target_key,
                emoji,
            },
        ] = reactions.as_slice()
        else {
            panic!("expected exactly one reaction");
        };
        assert_eq!(reacted_chat, chat);
        assert_eq!(emoji, "👍");
        assert_eq!(target_key.participant, None);
        assert_eq!(target_key.from_me, Some(false));
        assert_eq!(target_key.id.as_deref(), Some("m1"));
        assert_eq!(target_key.remote_jid.as_deref(), Some(chat));
        let stored = shared.database.message_by_id(chat, "m1").unwrap().unwrap();
        assert_eq!(stored.reactions.len(), 1);
    }

    #[tokio::test]
    async fn a_group_reaction_addresses_the_original_author() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let group = "123-456@g.us";
        let fake = Arc::new(FakeTransport::new().with_lid("100000000000000@lid"));
        attach(&shared, &fake).await;
        let react = |from_me: bool, sender: &str| Command::React {
            chat_jid: group.to_owned(),
            message_id: "m1".into(),
            sender_jid: sender.to_owned(),
            target_from_me: from_me,
            emoji: "👍".into(),
        };

        assert!(run(&shared, react(false, "not a jid")).await.is_err());
        run(&shared, react(false, "31600000000:3@s.whatsapp.net"))
            .await
            .unwrap();
        run(&shared, react(true, "unused")).await.unwrap();

        let participants = fake
            .calls_of(CallKind::SendReaction)
            .into_iter()
            .map(|call| match call {
                Call::SendReaction { target_key, .. } => target_key.participant.clone(),
                _ => unreachable!("filtered by call kind"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            participants,
            vec![
                Some("31600000000@s.whatsapp.net".to_owned()),
                Some("100000000000000@lid".to_owned()),
            ]
        );

        // Without a LID the phone-number identity is used instead.
        let phone_only = Arc::new(FakeTransport::new().with_pn("31600000000@s.whatsapp.net"));
        attach(&shared, &phone_only).await;
        run(&shared, react(true, "unused")).await.unwrap();
        let phone_reactions = phone_only.calls_of(CallKind::SendReaction);
        let [Call::SendReaction { target_key, .. }] = phone_reactions.as_slice() else {
            panic!("expected exactly one reaction");
        };
        assert_eq!(
            target_key.participant.as_deref(),
            Some("31600000000@s.whatsapp.net")
        );

        // An unpaired device has no identity to react with at all.
        let anonymous = Arc::new(FakeTransport::new());
        attach(&shared, &anonymous).await;
        assert_eq!(
            run(&shared, react(true, "unused"))
                .await
                .unwrap_err()
                .to_string(),
            "WhatsApp identity is unavailable"
        );
    }

    #[tokio::test]
    async fn marking_a_chat_read_queues_receipts_and_republishes_the_total() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let chat = "1@s.whatsapp.net";
        shared
            .database
            .insert_message(&stored_message(chat, "m1", None), "Ada", false, true)
            .unwrap();
        assert_eq!(shared.database.unread_total().unwrap(), 1);
        let mut events = shared.events.subscribe();

        assert_eq!(
            run(
                &shared,
                Command::MarkRead {
                    chat_jid: chat.into(),
                }
            )
            .await
            .unwrap(),
            ServerEvent::Ack
        );

        assert_eq!(shared.database.unread_total().unwrap(), 0);
        let batch = shared.database.next_read_batch().unwrap().unwrap();
        assert_eq!(batch.chat_jid, chat);
        assert_eq!(batch.receipts.len(), 1);
        let published = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert!(published.contains(&ServerEvent::Unread { total: 0 }));
    }

    #[tokio::test]
    async fn pinning_a_chat_updates_whatsapp_and_the_local_chat_list() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let pin = |pinned: bool| Command::SetChatPinned {
            chat_jid: chat.to_owned(),
            pinned,
        };

        assert!(
            run(
                &shared,
                Command::SetChatPinned {
                    chat_jid: "not a jid".into(),
                    pinned: true,
                }
            )
            .await
            .is_err()
        );
        assert_eq!(run(&shared, pin(true)).await.unwrap(), ServerEvent::Ack);
        assert_eq!(run(&shared, pin(false)).await.unwrap(), ServerEvent::Ack);
        assert_eq!(
            fake.calls_of(CallKind::PinChat),
            vec![Call::PinChat(chat.into())]
        );
        assert_eq!(
            fake.calls_of(CallKind::UnpinChat),
            vec![Call::UnpinChat(chat.into())]
        );

        fake.fail(CallKind::PinChat, "offline");
        assert_eq!(
            run(&shared, pin(true)).await.unwrap_err().to_string(),
            "offline"
        );
    }

    // --- presence, chat state, session ------------------------------------

    #[tokio::test]
    async fn connected_presence_intent_subscribes_and_unsubscribes_the_active_chat() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new().with_push_name("Ada"));
        attach(&shared, &fake).await;
        let connection_id = shared.open_connection();

        for command in [
            Command::SetPresence { available: true },
            Command::SetActiveChat {
                chat_jid: Some("1@s.whatsapp.net".into()),
            },
            Command::SetActiveChat { chat_jid: None },
            Command::SetPresence { available: false },
        ] {
            assert_eq!(
                handle_command(command, &shared, connection_id)
                    .await
                    .unwrap(),
                ServerEvent::Ack
            );
        }

        assert_eq!(
            fake.calls_of(CallKind::SubscribePresence),
            vec![Call::SubscribePresence("1@s.whatsapp.net".into())]
        );
        assert_eq!(
            fake.calls_of(CallKind::UnsubscribePresence),
            vec![Call::UnsubscribePresence("1@s.whatsapp.net".into())]
        );
        assert_eq!(fake.calls_of(CallKind::SetAvailable).len(), 1);
        assert_eq!(fake.calls_of(CallKind::SetUnavailable).len(), 1);
    }

    #[tokio::test]
    async fn every_chat_state_reaches_whatsapp() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let typing = || Command::SetChatState {
            chat_jid: chat.to_owned(),
            state: ChatState::Typing,
        };

        for (state, kind) in [
            (ChatState::Typing, CallKind::SendComposing),
            (ChatState::Recording, CallKind::SendRecording),
            (ChatState::Paused, CallKind::SendPaused),
        ] {
            assert_eq!(
                run(
                    &shared,
                    Command::SetChatState {
                        chat_jid: chat.into(),
                        state,
                    }
                )
                .await
                .unwrap(),
                ServerEvent::Ack
            );
            assert_eq!(fake.calls_of(kind).len(), 1);
        }

        assert!(
            run(
                &shared,
                Command::SetChatState {
                    chat_jid: "not a jid".into(),
                    state: ChatState::Typing,
                }
            )
            .await
            .is_err()
        );
        fake.fail(CallKind::SendComposing, "offline");
        assert!(run(&shared, typing()).await.is_err());
    }

    #[tokio::test]
    async fn a_chat_state_resync_that_cannot_be_armed_reports_the_failure() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = test_shared(&directory);
        shared.event_sync_marker = directory.path().join("marker-directory");
        std::fs::create_dir(&shared.event_sync_marker).unwrap();
        let shared = Arc::new(shared);
        *shared.status.write().await = ConnectionStatus::Connected;
        let mut events = shared.events.subscribe();

        assert_eq!(
            run(&shared, Command::ResyncChatState)
                .await
                .unwrap_err()
                .to_string(),
            "arming WhatsApp chat-state resync"
        );

        assert!(!shared.chat_state_resync_requested.load(Ordering::SeqCst));
        assert_eq!(
            events.recv().await.unwrap().event,
            ServerEvent::ChatStateResync {
                status: ChatStateResyncStatus::Failed,
                message: Some("Could not schedule the WhatsApp chat-state replay".into()),
            }
        );
    }

    #[tokio::test]
    async fn logging_out_unlinks_once_and_stops_the_run_loop() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let mut signal = shared.arm_logout();

        assert_eq!(
            run(&shared, Command::Logout).await.unwrap(),
            ServerEvent::Ack
        );

        assert!(shared.logout_requested.load(Ordering::SeqCst));
        assert!(signal.try_recv().is_ok());
        assert_eq!(fake.call_kinds(), vec![CallKind::Logout]);
        assert_eq!(
            run(&shared, Command::Logout).await.unwrap_err().to_string(),
            "a WhatsApp logout is already in progress"
        );
    }

    // --- avatars ----------------------------------------------------------

    #[tokio::test]
    async fn avatar_requests_are_deduplicated_by_canonical_identity() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let jid = "1@s.whatsapp.net";

        assert!(
            run(
                &shared,
                Command::RequestAvatar {
                    jid: "not a jid".into(),
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            run(
                &shared,
                Command::RequestAvatar {
                    jid: "1:2@s.whatsapp.net".into(),
                }
            )
            .await
            .unwrap(),
            ServerEvent::Ack
        );
        settle().await;

        assert!(shared.avatar_fetches.lock().await.is_empty());
        assert!(assets::avatar_missing_path(&shared.avatar_dir, jid).exists());
        assert_eq!(
            fake.calls_of(CallKind::ProfilePicture),
            vec![Call::ProfilePicture(jid.into())]
        );

        // A fetch already in flight for the same identity is acked, not repeated.
        shared.avatar_fetches.lock().await.insert(jid.to_owned());
        fake.clear_calls();
        assert_eq!(
            run(&shared, Command::RequestAvatar { jid: jid.into() })
                .await
                .unwrap(),
            ServerEvent::Ack
        );
        settle().await;
        assert!(fake.calls_of(CallKind::ProfilePicture).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_avatar_fetch_gives_up_on_a_slow_answer_or_a_closed_queue() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(
            FakeTransport::new()
                .with_delay(CallKind::ProfilePicture, jobs::AVATAR_FETCH_TIMEOUT * 2),
        );
        let transport = fake_transport(&fake);

        fetch_requested_avatar(
            &shared,
            Arc::clone(&transport),
            "1@s.whatsapp.net".parse().unwrap(),
        )
        .await;
        assert!(!assets::avatar_missing_path(&shared.avatar_dir, "1@s.whatsapp.net").exists());

        shared.avatar_fetch_permits.close();
        fake.clear_calls();
        fetch_requested_avatar(&shared, transport, "2@s.whatsapp.net".parse().unwrap()).await;
        assert!(fake.calls().is_empty());
    }

    // --- media downloads --------------------------------------------------

    #[tokio::test]
    async fn downloading_an_image_writes_the_private_cache_and_broadcasts_it() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let bytes = b"\xff\xd8\xffsynthetic image".to_vec();
        let fake = Arc::new(FakeTransport::new().with_download_bytes(&bytes));
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let payload = wa::message::ImageMessage {
            mimetype: Some("image/jpeg".into()),
            file_length: Some(u64::try_from(bytes.len()).unwrap()),
            width: Some(2),
            height: Some(3),
            jpeg_thumbnail: Some(b"\xff\xd8\xffpreview".to_vec()),
            ..wa::message::ImageMessage::default()
        };
        seed_media(
            &shared,
            chat,
            "image-1",
            MessageMedia::Image {
                path: String::new(),
                thumbnail_path: String::new(),
                downloaded: false,
                mime_type: "image/jpeg".into(),
                width: 2,
                height: 3,
            },
            &payload.encode_to_vec(),
        );

        let event = download_outcome(&shared, chat, "image-1").await;

        let ServerEvent::MediaDownloaded {
            media, message_id, ..
        } = event
        else {
            panic!("expected a downloaded image, got {event:?}");
        };
        assert_eq!(message_id, "image-1");
        let MessageMedia::Image {
            path,
            thumbnail_path,
            downloaded,
            ..
        } = media
        else {
            panic!("expected image media");
        };
        assert!(downloaded);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::read(&thumbnail_path).unwrap(),
            b"\xff\xd8\xffpreview"
        );
        assert_eq!(
            fake.calls_of(CallKind::Download),
            vec![Call::Download(MediaKind::Image)]
        );
        assert!(shared.media_downloads.lock().await.is_empty());
    }

    #[tokio::test]
    async fn downloading_a_sticker_requires_webp_and_refuses_lottie() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let bytes = b"RIFF\x08\0\0\0WEBPVP8 ".to_vec();
        let fake = Arc::new(FakeTransport::new().with_download_bytes(&bytes));
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let sticker_media = MessageMedia::Sticker {
            path: String::new(),
            thumbnail_path: String::new(),
            downloaded: false,
            mime_type: "image/webp".into(),
            width: 512,
            height: 512,
            animated: false,
            lottie: false,
            accessibility_label: String::new(),
        };
        let payload = wa::message::StickerMessage {
            mimetype: Some("image/webp".into()),
            file_length: Some(u64::try_from(bytes.len()).unwrap()),
            width: Some(512),
            height: Some(512),
            png_thumbnail: Some(b"\x89PNG\r\n\x1a\npreview".to_vec()),
            ..wa::message::StickerMessage::default()
        };
        seed_media(
            &shared,
            chat,
            "sticker-1",
            sticker_media.clone(),
            &payload.encode_to_vec(),
        );
        let lottie = wa::message::StickerMessage {
            is_lottie: Some(true),
            ..payload
        };
        seed_media(
            &shared,
            chat,
            "sticker-2",
            sticker_media,
            &lottie.encode_to_vec(),
        );

        let event = download_outcome(&shared, chat, "sticker-1").await;
        let ServerEvent::MediaDownloaded { media, .. } = event else {
            panic!("expected a downloaded sticker, got {event:?}");
        };
        let MessageMedia::Sticker {
            path,
            thumbnail_path,
            downloaded,
            ..
        } = media
        else {
            panic!("expected sticker media");
        };
        assert!(downloaded);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(
            std::fs::read(thumbnail_path).unwrap(),
            b"\x89PNG\r\n\x1a\npreview"
        );

        assert_eq!(
            download_outcome(&shared, chat, "sticker-2").await,
            ServerEvent::MediaDownloadFailed {
                chat_jid: chat.into(),
                message_id: "sticker-2".into(),
                message: "Lottie sticker animation is not supported safely".into(),
            }
        );
        assert_eq!(
            fake.calls_of(CallKind::Download),
            vec![Call::Download(MediaKind::Sticker)]
        );
    }

    #[tokio::test]
    async fn downloading_a_video_keeps_its_embedded_preview_when_regeneration_fails() {
        // The synthetic bytes cannot decode, so the forced post-download
        // refresh must fall back to the embedded thumbnail it tried to beat.
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let bytes = b"\0\0\0\x18ftypisomsynthetic".to_vec();
        let fake = Arc::new(FakeTransport::new().with_download_bytes(&bytes));
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let payload = wa::message::VideoMessage {
            mimetype: Some("video/mp4".into()),
            file_length: Some(u64::try_from(bytes.len()).unwrap()),
            width: Some(4),
            height: Some(2),
            seconds: Some(9),
            jpeg_thumbnail: Some(b"\xff\xd8\xffpreview".to_vec()),
            ..wa::message::VideoMessage::default()
        };
        seed_media(
            &shared,
            chat,
            "video-1",
            MessageMedia::Video {
                path: String::new(),
                thumbnail_path: String::new(),
                downloaded: false,
                mime_type: "video/mp4".into(),
                width: 4,
                height: 2,
                duration_seconds: 9,
                gif_playback: false,
            },
            &payload.encode_to_vec(),
        );

        let event = download_outcome(&shared, chat, "video-1").await;

        let ServerEvent::MediaDownloaded { media, .. } = event else {
            panic!("expected a downloaded video, got {event:?}");
        };
        let MessageMedia::Video {
            path,
            thumbnail_path,
            downloaded,
            duration_seconds,
            ..
        } = media
        else {
            panic!("expected video media");
        };
        assert!(downloaded);
        assert_eq!(duration_seconds, 9);
        assert!(path.ends_with(".video.mp4"));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(
            std::fs::read(thumbnail_path).unwrap(),
            b"\xff\xd8\xffpreview"
        );
        assert_eq!(
            fake.calls_of(CallKind::Download),
            vec![Call::Download(MediaKind::Video)]
        );
    }

    #[tokio::test]
    async fn downloading_a_voice_note_writes_ogg_audio() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let bytes = b"OggSsynthetic voice".to_vec();
        let fake = Arc::new(FakeTransport::new().with_download_bytes(&bytes));
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let payload = wa::message::AudioMessage {
            mimetype: Some("audio/ogg; codecs=opus".into()),
            file_length: Some(u64::try_from(bytes.len()).unwrap()),
            seconds: Some(4),
            ptt: Some(true),
            ..wa::message::AudioMessage::default()
        };
        seed_media(
            &shared,
            chat,
            "audio-1",
            MessageMedia::Audio {
                path: String::new(),
                downloaded: false,
                mime_type: "audio/ogg; codecs=opus".into(),
                duration_seconds: 4,
                voice_message: true,
            },
            &payload.encode_to_vec(),
        );

        let event = download_outcome(&shared, chat, "audio-1").await;

        let ServerEvent::MediaDownloaded { media, .. } = event else {
            panic!("expected downloaded audio, got {event:?}");
        };
        let MessageMedia::Audio {
            path,
            downloaded,
            voice_message,
            ..
        } = media
        else {
            panic!("expected audio media");
        };
        assert!(downloaded);
        assert!(voice_message);
        assert!(path.ends_with(".audio.ogg"));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(
            fake.calls_of(CallKind::Download),
            vec![Call::Download(MediaKind::Audio)]
        );
    }

    #[tokio::test]
    async fn media_download_requests_are_validated_and_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let download = |message_id: &str| Command::DownloadMedia {
            chat_jid: chat.to_owned(),
            message_id: message_id.to_owned(),
        };

        for invalid in [String::new(), "x".repeat(513)] {
            assert_eq!(
                run(&shared, download(&invalid))
                    .await
                    .unwrap_err()
                    .to_string(),
                "invalid media message ID"
            );
        }
        assert_eq!(
            run(&shared, download("unknown"))
                .await
                .unwrap_err()
                .to_string(),
            "message does not contain downloadable media"
        );

        shared
            .database
            .insert_message(
                &stored_message(
                    chat,
                    "document-1",
                    Some(MessageMedia::Document {
                        path: String::new(),
                        file_name: "quote.pdf".into(),
                        mime_type: "application/pdf".into(),
                        file_size: 4,
                        page_count: 1,
                    }),
                ),
                "Ada",
                false,
                false,
            )
            .unwrap();
        assert_eq!(
            run(&shared, download("document-1"))
                .await
                .unwrap_err()
                .to_string(),
            "this media type does not require a download"
        );

        seed_media(
            &shared,
            chat,
            "image-1",
            MessageMedia::Image {
                path: String::new(),
                thumbnail_path: String::new(),
                downloaded: false,
                mime_type: "image/jpeg".into(),
                width: 1,
                height: 1,
            },
            b"payload",
        );
        // A transfer already queued for this message reports its own outcome.
        let key = format!("{chat}\0image-1");
        shared.media_downloads.lock().await.insert(key.clone());
        assert_eq!(
            run(&shared, download("image-1")).await.unwrap(),
            ServerEvent::Ack
        );

        let mut downloads = shared.media_downloads.lock().await;
        for index in 0..jobs::MAX_PENDING_MEDIA_DOWNLOADS {
            downloads.insert(format!("queued-{index}"));
        }
        downloads.remove(&key);
        drop(downloads);
        assert_eq!(
            run(&shared, download("image-1"))
                .await
                .unwrap_err()
                .to_string(),
            "too many downloads are already queued"
        );
        assert!(fake.calls_of(CallKind::Download).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_that_cannot_be_decoded_or_finished_is_reported_as_failed() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let bytes = b"\xff\xd8\xffsynthetic image".to_vec();
        let fake = Arc::new(
            FakeTransport::new()
                .with_download_bytes(&bytes)
                .with_delay(CallKind::Download, jobs::MEDIA_DOWNLOAD_TIMEOUT * 2),
        );
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let image_media = MessageMedia::Image {
            path: String::new(),
            thumbnail_path: String::new(),
            downloaded: false,
            mime_type: "image/jpeg".into(),
            width: 1,
            height: 1,
        };
        seed_media(
            &shared,
            chat,
            "corrupt",
            image_media.clone(),
            &[0xff, 0xff, 0xff, 0xff],
        );
        let payload = wa::message::ImageMessage {
            mimetype: Some("image/jpeg".into()),
            file_length: Some(u64::try_from(bytes.len()).unwrap()),
            ..wa::message::ImageMessage::default()
        };
        seed_media(&shared, chat, "slow", image_media, &payload.encode_to_vec());

        let ServerEvent::MediaDownloadFailed { message, .. } =
            download_outcome(&shared, chat, "corrupt").await
        else {
            panic!("expected a decoding failure");
        };
        assert_eq!(message, "reading image download metadata");

        let ServerEvent::MediaDownloadFailed { message, .. } =
            download_outcome(&shared, chat, "slow").await
        else {
            panic!("expected a timeout failure");
        };
        assert_eq!(message, "image download timed out");
        assert!(shared.media_downloads.lock().await.is_empty());
    }

    #[tokio::test]
    async fn a_download_without_a_private_cache_fails_before_any_transfer() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new().with_download_bytes(b"\xff\xd8\xffunreachable"));
        attach(&shared, &fake).await;
        let chat = "1@s.whatsapp.net";
        let jpeg = b"\xff\xd8\xffpreview".to_vec();

        let image = wa::message::ImageMessage {
            file_length: Some(16),
            jpeg_thumbnail: Some(jpeg.clone()),
            ..wa::message::ImageMessage::default()
        };
        seed_media(
            &shared,
            chat,
            "image-1",
            MessageMedia::Image {
                path: String::new(),
                thumbnail_path: String::new(),
                downloaded: false,
                mime_type: "image/jpeg".into(),
                width: 1,
                height: 1,
            },
            &image.encode_to_vec(),
        );
        let sticker = wa::message::StickerMessage {
            file_length: Some(16),
            png_thumbnail: Some(b"\x89PNG\r\n\x1a\npreview".to_vec()),
            ..wa::message::StickerMessage::default()
        };
        seed_media(
            &shared,
            chat,
            "sticker-1",
            MessageMedia::Sticker {
                path: String::new(),
                thumbnail_path: String::new(),
                downloaded: false,
                mime_type: "image/webp".into(),
                width: 1,
                height: 1,
                animated: false,
                lottie: false,
                accessibility_label: String::new(),
            },
            &sticker.encode_to_vec(),
        );
        let video = wa::message::VideoMessage {
            mimetype: Some("video/mp4".into()),
            file_length: Some(16),
            jpeg_thumbnail: Some(jpeg),
            ..wa::message::VideoMessage::default()
        };
        seed_media(
            &shared,
            chat,
            "video-1",
            MessageMedia::Video {
                path: String::new(),
                thumbnail_path: String::new(),
                downloaded: false,
                mime_type: "video/mp4".into(),
                width: 1,
                height: 1,
                duration_seconds: 1,
                gif_playback: false,
            },
            &video.encode_to_vec(),
        );
        std::fs::remove_dir_all(&shared.media_dir).unwrap();

        for message_id in ["image-1", "sticker-1", "video-1"] {
            let outcome = download_outcome(&shared, chat, message_id).await;
            assert!(
                matches!(outcome, ServerEvent::MediaDownloadFailed { .. }),
                "{message_id} reported {outcome:?}"
            );
        }
        assert!(fake.calls_of(CallKind::Download).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn download_metadata_is_recovered_from_history_or_reported_missing() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new().with_history_request_id("request-1"));
        let transport = fake_transport(&fake);
        let chat = "1@s.whatsapp.net";

        assert_eq!(
            media_download_payload(&shared, &transport, chat, "gone", "image")
                .await
                .unwrap_err()
                .to_string(),
            "image message is no longer in local history"
        );

        for id in ["stored", "arriving", "missing"] {
            shared
                .database
                .insert_message(&stored_message(chat, id, None), "Ada", false, false)
                .unwrap();
        }
        shared
            .database
            .store_media_download(chat, "stored", b"payload")
            .unwrap();
        assert_eq!(
            media_download_payload(&shared, &transport, chat, "stored", "image")
                .await
                .unwrap(),
            b"payload".to_vec()
        );
        assert!(fake.calls().is_empty());

        let arriving = Arc::clone(&shared);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            arriving
                .database
                .store_media_download("1@s.whatsapp.net", "arriving", b"recovered")
                .unwrap();
        });
        assert_eq!(
            media_download_payload(&shared, &transport, chat, "arriving", "image")
                .await
                .unwrap(),
            b"recovered".to_vec()
        );
        assert_eq!(
            fake.calls_of(CallKind::FetchMessageHistory),
            vec![Call::FetchMessageHistory {
                chat: chat.into(),
                oldest_message_id: "arriving".into(),
                oldest_message_from_me: false,
                oldest_message_timestamp_ms: 1_700_000_000_000,
                count: 3,
            }]
        );

        assert_eq!(
            media_download_payload(&shared, &transport, chat, "missing", "video")
                .await
                .unwrap_err()
                .to_string(),
            "WhatsApp did not return download details for this video"
        );

        fake.fail(CallKind::FetchMessageHistory, "offline");
        assert_eq!(
            media_download_payload(&shared, &transport, chat, "missing", "image")
                .await
                .unwrap_err()
                .to_string(),
            "requesting image download metadata"
        );
        fake.fail(CallKind::RequestPlaceholderResend, "no primary device");
        assert_eq!(
            media_download_payload(&shared, &transport, chat, "missing", "image")
                .await
                .unwrap_err()
                .to_string(),
            "requesting exact image message"
        );
    }

    #[tokio::test]
    async fn a_failed_or_panicking_video_preview_only_warns() {
        log_video_preview_result(Ok(Ok(true)));
        log_video_preview_result(Ok(Err(anyhow!("ffmpeg is unavailable"))));
        let panicked = tokio::spawn(async {
            panic!("video preview worker");
        })
        .await
        .unwrap_err();
        log_video_preview_result(Err(panicked));
    }

    // --- voice messages ---------------------------------------------------

    fn pasted_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes
    }

    fn stage_paste(shared: &Arc<Shared>, name: &str, bytes: &[u8]) -> String {
        let path = shared.media_dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn pasting_an_image_from_clipboard_stages_it_for_sending() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        paste::fake::test_clipboard(&shared.clipboard).script_image("image/png", pasted_png(2, 3));
        std::fs::create_dir_all(&shared.media_dir).unwrap();

        let event = run(&shared, Command::PasteImage).await.unwrap();
        let ServerEvent::ImagePasted {
            path,
            width,
            height,
            mime_type,
        } = event
        else {
            panic!("expected a staged paste, got {event:?}");
        };
        assert_eq!((width, height), (2, 3));
        assert_eq!(mime_type, "image/png");
        assert_eq!(
            Path::new(&path).extension().and_then(|name| name.to_str()),
            Some("png")
        );
        assert_eq!(std::fs::read(path).unwrap(), pasted_png(2, 3));
    }

    #[tokio::test]
    async fn pasting_without_an_image_reports_empty() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        assert!(matches!(
            run(&shared, Command::PasteImage).await.unwrap(),
            ServerEvent::ImagePasteEmpty
        ));
        paste::fake::test_clipboard(&shared.clipboard).fail_list("no selection");
        assert!(matches!(
            run(&shared, Command::PasteImage).await.unwrap(),
            ServerEvent::ImagePasteEmpty
        ));
    }

    #[tokio::test]
    async fn pasting_an_invalid_image_fails_the_command() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        paste::fake::test_clipboard(&shared.clipboard)
            .script_image("image/png", b"not an image".to_vec());
        assert!(run(&shared, Command::PasteImage).await.is_err());
    }

    #[tokio::test]
    async fn image_caption_mentions_reach_the_wire() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let mut original = stored_message("123@g.us", "ORIGINAL", None);
        original.sender_jid = "200@lid".into();
        shared
            .database
            .insert_message(&original, "Group", true, false)
            .unwrap();
        let staged = stage_paste(&shared, "mention.png", &pasted_png(2, 3));
        assert!(
            run(
                &shared,
                Command::SendImage {
                    chat_jid: "123@g.us".into(),
                    path: staged.clone(),
                    caption: String::new(),
                    delivery_id: "missing-reply".into(),
                    mentions: vec![],
                    reply_to: Some(omarchy_whatsapp_protocol::ReplyTarget {
                        message_id: "MISSING".into(),
                        sender_jid: "200@lid".into()
                    }),
                }
            )
            .await
            .is_err()
        );
        run(
            &shared,
            Command::SendImage {
                chat_jid: "123@g.us".into(),
                path: staged,
                caption: "For @200".into(),
                delivery_id: "image-mention".into(),
                mentions: vec!["200@lid".into()],
                reply_to: Some(omarchy_whatsapp_protocol::ReplyTarget {
                    message_id: "ORIGINAL".into(),
                    sender_jid: "200@lid".into(),
                }),
            },
        )
        .await
        .unwrap();
        let calls = fake.calls_of(CallKind::SendMessage);
        let Call::SendMessage { message, .. } = &calls[0] else {
            panic!("expected send")
        };
        assert_eq!(
            message.image_message.context_info.stanza_id.as_deref(),
            Some("ORIGINAL")
        );
        assert_eq!(
            message.image_message.context_info.participant.as_deref(),
            Some("200@lid")
        );
        assert!(
            shared
                .database
                .messages("123@g.us", 10)
                .unwrap()
                .iter()
                .any(|m| m.quote.is_some())
        );
        assert_eq!(
            message.image_message.context_info.mentioned_jid,
            ["200@lid"]
        );
    }

    #[tokio::test]
    async fn sending_a_pasted_image_uploads_caches_and_persists_it() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let staged = stage_paste(&shared, "paste-1.png", &pasted_png(2, 3));

        let event = run(
            &shared,
            Command::SendImage {
                chat_jid: "1@s.whatsapp.net".into(),
                path: staged,
                caption: "hi".into(),
                delivery_id: "img-1".into(),
                mentions: Vec::new(),
                reply_to: None,
            },
        )
        .await
        .unwrap();
        let ServerEvent::Sent { message } = event else {
            panic!("expected the sent image message");
        };
        assert_eq!(message.id, text_outbox::stable_message_id("img-1"));
        assert_eq!(message.chat_jid, "1@s.whatsapp.net");
        assert_eq!(message.text, "hi");
        let Some(MessageMedia::Image {
            path,
            thumbnail_path,
            downloaded,
            width,
            height,
            ..
        }) = message.media
        else {
            panic!("expected image media");
        };
        assert!(downloaded);
        assert_eq!((width, height), (2, 3));
        assert_eq!(thumbnail_path, path);
        assert_eq!(std::fs::read(&path).unwrap(), pasted_png(2, 3));
        assert_eq!(
            fake.calls_of(CallKind::UploadImageMessage),
            vec![Call::UploadImageMessage {
                byte_count: pasted_png(2, 3).len(),
                mimetype: Some("image/png".into()),
                caption: Some("hi".into()),
            }]
        );
        assert_eq!(fake.calls_of(CallKind::SendMessage).len(), 1);
        assert!(
            shared
                .database
                .message_by_id("1@s.whatsapp.net", &message.id)
                .unwrap()
                .is_some()
        );

        let staged = stage_paste(&shared, "paste-2.png", &pasted_png(4, 5));
        let event = run(
            &shared,
            Command::SendImage {
                chat_jid: "1@s.whatsapp.net".into(),
                path: staged,
                caption: String::new(),
                delivery_id: "img-2".into(),
                mentions: Vec::new(),
                reply_to: None,
            },
        )
        .await
        .unwrap();
        let ServerEvent::Sent { message } = event else {
            panic!("expected the sent image message");
        };
        assert_eq!(message.text, "[Image]");
    }

    #[tokio::test]
    async fn sending_a_pasted_image_twice_delivers_once() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let staged = stage_paste(&shared, "paste-1.png", &pasted_png(2, 3));
        let send = || Command::SendImage {
            chat_jid: "1@s.whatsapp.net".into(),
            path: staged.clone(),
            caption: String::new(),
            delivery_id: "img-1".into(),
            mentions: Vec::new(),
            reply_to: None,
        };
        run(&shared, send()).await.unwrap();
        run(&shared, send()).await.unwrap();
        assert_eq!(fake.calls_of(CallKind::UploadImageMessage).len(), 1);
        assert_eq!(fake.calls_of(CallKind::SendMessage).len(), 1);
    }

    #[tokio::test]
    async fn image_sends_reject_invalid_requests() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let staged = stage_paste(&shared, "paste-1.png", &pasted_png(2, 3));
        let send =
            |chat_jid: &str, path: String, caption: String, delivery_id: &str| Command::SendImage {
                chat_jid: chat_jid.into(),
                path,
                caption,
                delivery_id: delivery_id.into(),
                mentions: Vec::new(),
                reply_to: None,
            };
        assert!(
            run(
                &shared,
                send("not a jid", staged.clone(), String::new(), "img-1")
            )
            .await
            .is_err()
        );
        assert!(
            run(
                &shared,
                send("1@s.whatsapp.net", staged.clone(), String::new(), "!!")
            )
            .await
            .is_err()
        );
        assert_eq!(
            run(
                &shared,
                send(
                    "1@s.whatsapp.net",
                    staged.clone(),
                    "a".repeat(1025),
                    "img-1"
                )
            )
            .await
            .unwrap_err()
            .to_string(),
            "caption is too large"
        );
        assert!(
            run(
                &shared,
                send(
                    "1@s.whatsapp.net",
                    "/etc/hostname".into(),
                    String::new(),
                    "img-1"
                )
            )
            .await
            .is_err()
        );
        assert!(
            run(
                &shared,
                send(
                    "1@s.whatsapp.net",
                    shared
                        .media_dir
                        .join("missing.png")
                        .to_string_lossy()
                        .into_owned(),
                    String::new(),
                    "img-1"
                )
            )
            .await
            .is_err()
        );
        let garbage = stage_paste(&shared, "paste-garbage.png", b"not an image");
        assert!(
            run(
                &shared,
                send("1@s.whatsapp.net", garbage, String::new(), "img-1")
            )
            .await
            .is_err()
        );
        assert_eq!(fake.calls_of(CallKind::UploadImageMessage).len(), 0);
        assert_eq!(fake.calls_of(CallKind::SendMessage).len(), 0);
    }

    #[tokio::test]
    async fn image_send_failures_surface_transport_errors() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let staged = stage_paste(&shared, "paste-1.png", &pasted_png(2, 3));
        let send = || Command::SendImage {
            chat_jid: "1@s.whatsapp.net".into(),
            path: staged.clone(),
            caption: String::new(),
            delivery_id: "img-1".into(),
            mentions: Vec::new(),
            reply_to: None,
        };
        assert_eq!(
            run(&shared, send()).await.unwrap_err().to_string(),
            "WhatsApp is not connected"
        );

        let fake =
            Arc::new(FakeTransport::new().failing(CallKind::UploadImageMessage, "cdn is down"));
        attach(&shared, &fake).await;
        assert!(
            run(&shared, send())
                .await
                .unwrap_err()
                .to_string()
                .contains("uploading image")
        );

        let failing_send =
            Arc::new(FakeTransport::new().failing(CallKind::SendMessage, "send is down"));
        attach(&shared, &failing_send).await;
        assert!(run(&shared, send()).await.is_err());

        let mismatched = Arc::new(FakeTransport::new().with_send_receipt_id("SOMETHING-ELSE"));
        attach(&shared, &mismatched).await;
        assert_eq!(
            run(&shared, send()).await.unwrap_err().to_string(),
            "WhatsApp returned a different image message ID"
        );
    }

    #[tokio::test]
    async fn a_sent_image_survives_cache_copy_failure() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new());
        attach(&shared, &fake).await;
        let staged = stage_paste(&shared, "paste-1.png", &pasted_png(2, 3));
        let mut permissions = std::fs::metadata(&shared.media_dir).unwrap().permissions();
        permissions.set_mode(0o555);
        std::fs::set_permissions(&shared.media_dir, permissions).unwrap();

        let ServerEvent::Sent { message } = run(
            &shared,
            Command::SendImage {
                chat_jid: "1@s.whatsapp.net".into(),
                path: staged,
                caption: String::new(),
                delivery_id: "img-1".into(),
                mentions: Vec::new(),
                reply_to: None,
            },
        )
        .await
        .unwrap() else {
            panic!("expected the sent image message");
        };

        let Some(MessageMedia::Image { downloaded, .. }) = message.media else {
            panic!("expected image media");
        };
        assert!(!downloaded);
        assert_eq!(fake.calls_of(CallKind::SendMessage).len(), 1);
    }

    #[tokio::test]
    async fn sending_a_recording_uploads_caches_and_completes_the_outbox_job() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new().with_message_ids(["VOICE-1"]));
        attach(&shared, &fake).await;
        write_recording(&shared, "voice-1", 2_400);
        let mut events = shared.events.subscribe();

        let event = run(
            &shared,
            Command::SendVoiceMessage {
                chat_jid: "1:2@s.whatsapp.net".into(),
                recording_id: "voice-1".into(),
            },
        )
        .await
        .unwrap();

        let ServerEvent::Sent { message } = event else {
            panic!("expected the sent voice message");
        };
        assert_eq!(message.id, "VOICE-1");
        assert_eq!(message.chat_jid, "1@s.whatsapp.net");
        assert_eq!(message.text, "[Voice message]");
        let Some(MessageMedia::Audio {
            path,
            downloaded,
            duration_seconds,
            voice_message,
            ..
        }) = message.media
        else {
            panic!("expected audio media");
        };
        assert!(downloaded);
        assert!(voice_message);
        assert_eq!(duration_seconds, 3);
        assert_eq!(std::fs::read(path).unwrap(), recording(2_400));
        assert_eq!(
            fake.calls_of(CallKind::UploadAudioMessage),
            vec![Call::UploadAudioMessage {
                byte_count: recording(2_400).len(),
                duration_seconds: Some(3),
                ptt: Some(true),
            }]
        );
        assert!(
            shared
                .database
                .message_by_id("1@s.whatsapp.net", "VOICE-1")
                .unwrap()
                .is_some()
        );
        assert!(
            voice_outbox::entries(&shared.voice_outbox_dir)
                .unwrap()
                .is_empty()
        );
        assert!(
            !voice_outbox::recording_path(&shared.voice_outbox_dir, "voice-1")
                .unwrap()
                .exists()
        );
        let published = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert!(
            published
                .iter()
                .any(|event| matches!(event, ServerEvent::VoiceOutbox { .. }))
        );
    }

    #[tokio::test]
    async fn a_retried_recording_keeps_its_delivery_id_and_skips_a_second_upload() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(
            FakeTransport::new()
                .with_message_ids(["VOICE-9"])
                .failing(CallKind::UploadAudioMessage, "upload rejected"),
        );
        attach(&shared, &fake).await;
        write_recording(&shared, "voice-9", 1_000);
        let send = || Command::SendVoiceMessage {
            chat_jid: "1@s.whatsapp.net".into(),
            recording_id: "voice-9".into(),
        };

        assert_eq!(
            run(&shared, send()).await.unwrap_err().to_string(),
            "uploading voice message"
        );
        let entries = voice_outbox::entries(&shared.voice_outbox_dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, VoiceOutboxStatus::Failed);
        assert_eq!(entries[0].error.as_deref(), Some("uploading voice message"));

        // The delivery already reached WhatsApp out of band, so the retry
        // adopts the stored message instead of uploading a duplicate.
        shared
            .database
            .insert_message(
                &stored_message("1@s.whatsapp.net", "VOICE-9", None),
                "Ada",
                false,
                false,
            )
            .unwrap();
        fake.succeed(CallKind::UploadAudioMessage);

        let ServerEvent::Sent { message } = run(&shared, send()).await.unwrap() else {
            panic!("expected the sent voice message");
        };
        assert_eq!(message.id, "VOICE-9");
        assert_eq!(fake.calls_of(CallKind::UploadAudioMessage).len(), 1);
        assert_eq!(fake.calls_of(CallKind::GenerateMessageId).len(), 1);
        assert!(
            voice_outbox::entries(&shared.voice_outbox_dir)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn voice_sends_that_cannot_start_or_finish_fail_the_outbox_job() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let send = |recording_id: &str| Command::SendVoiceMessage {
            chat_jid: "1@s.whatsapp.net".to_owned(),
            recording_id: recording_id.to_owned(),
        };

        assert!(
            run(
                &shared,
                Command::SendVoiceMessage {
                    chat_jid: "not a jid".into(),
                    recording_id: "voice-2".into(),
                }
            )
            .await
            .is_err()
        );
        assert!(run(&shared, send("voice-2")).await.is_err());

        // Without a linked device the job is retained as failed.
        write_recording(&shared, "voice-2", 1_000);
        assert_eq!(
            run(&shared, send("voice-2")).await.unwrap_err().to_string(),
            "WhatsApp is not connected"
        );
        let entries = voice_outbox::entries(&shared.voice_outbox_dir).unwrap();
        assert_eq!(entries[0].status, VoiceOutboxStatus::Failed);

        // WhatsApp must confirm the delivery identity this daemon assigned.
        let fake = Arc::new(
            FakeTransport::new()
                .with_message_ids(["VOICE-2"])
                .with_send_receipt_id("SOMETHING-ELSE"),
        );
        attach(&shared, &fake).await;
        assert_eq!(
            run(&shared, send("voice-2")).await.unwrap_err().to_string(),
            "WhatsApp returned a different voice message ID"
        );
        let entries = voice_outbox::entries(&shared.voice_outbox_dir).unwrap();
        assert_eq!(entries[0].status, VoiceOutboxStatus::Failed);
        assert_eq!(
            entries[0].error.as_deref(),
            Some("WhatsApp returned a different voice message ID")
        );

        assert_eq!(
            run(
                &shared,
                Command::DiscardVoiceRecording {
                    recording_id: "voice-2".into(),
                }
            )
            .await
            .unwrap(),
            ServerEvent::Ack
        );
        assert!(
            voice_outbox::entries(&shared.voice_outbox_dir)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_voice_message_is_still_sent_when_the_private_cache_copy_fails() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(FakeTransport::new().with_message_ids(["VOICE-3"]));
        attach(&shared, &fake).await;
        write_recording(&shared, "voice-3", 1_000);
        std::fs::remove_dir_all(&shared.media_dir).unwrap();

        let ServerEvent::Sent { message } = run(
            &shared,
            Command::SendVoiceMessage {
                chat_jid: "1@s.whatsapp.net".into(),
                recording_id: "voice-3".into(),
            },
        )
        .await
        .unwrap() else {
            panic!("expected the sent voice message");
        };

        let Some(MessageMedia::Audio { downloaded, .. }) = message.media else {
            panic!("expected audio media");
        };
        assert!(!downloaded);
        assert_eq!(fake.calls_of(CallKind::SendMessage).len(), 1);
    }

    #[tokio::test]
    async fn an_unwritable_outbox_never_loses_the_send_result() {
        let directory = tempfile::tempdir().unwrap();
        let shared = shared_with_dirs(&directory);
        let fake = Arc::new(
            FakeTransport::new()
                .with_message_ids(["VOICE-4", "VOICE-5"])
                .with_delay(CallKind::SendMessage, Duration::from_millis(1)),
        );
        attach(&shared, &fake).await;
        write_recording(&shared, "sealed-1", 1_000);
        write_recording(&shared, "sealed-2", 1_000);

        for (recording_id, sent) in [("sealed-1", true), ("sealed-2", false)] {
            if !sent {
                fake.fail(CallKind::SendMessage, "WhatsApp rejected the send");
            }
            let sending = tokio::spawn({
                let shared = Arc::clone(&shared);
                let recording_id = recording_id.to_owned();
                async move {
                    handle_command(
                        Command::SendVoiceMessage {
                            chat_jid: "1@s.whatsapp.net".into(),
                            recording_id,
                        },
                        &shared,
                        0,
                    )
                    .await
                }
            });
            // The upload is recorded immediately before the delayed send, so
            // sealing the outbox here always lands between preparation and the
            // job's final transition.
            while fake.calls_of(CallKind::UploadAudioMessage).is_empty() {
                tokio::task::yield_now().await;
            }
            std::fs::set_permissions(
                &shared.voice_outbox_dir,
                std::fs::Permissions::from_mode(0o500),
            )
            .unwrap();
            let result = sending.await.unwrap();
            std::fs::set_permissions(
                &shared.voice_outbox_dir,
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            assert_eq!(result.is_ok(), sent, "{recording_id}");
            fake.clear_calls();
        }

        // Neither transition reached the outbox, so both jobs stayed as sending.
        let entries = voice_outbox::entries(&shared.voice_outbox_dir).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .all(|entry| entry.status == VoiceOutboxStatus::Sending)
        );
    }
}
