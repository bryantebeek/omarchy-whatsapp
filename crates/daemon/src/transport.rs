// The seam between the daemon's orchestration and `whatsapp-rust`. Every
// outbound SDK call the daemon makes goes through `Transport`, so command
// dispatch, event reduction, identity resolution, and the outbox loops can be
// driven by a scripted double instead of a live linked device.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use whatsapp_rust::prelude::{Client, Jid, MessageInfo, SendOptions, wa};
use whatsapp_rust::wacore::download::MediaType;
use whatsapp_rust::wacore::types::lid_pn::LidPnEntry;
use whatsapp_rust::{GroupMetadata, PollVoteCiphertext, ProfilePicture, UploadOptions, media};

/// The part of `whatsapp_rust::SendResult` the daemon reads. The SDK type is
/// `#[non_exhaustive]`, so nothing outside that crate can build one and a
/// scripted transport would have no way to answer a send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendReceipt {
    pub(crate) message_id: String,
}

/// The part of `whatsapp_rust::UserInfo` the daemon reads. That type is
/// `#[non_exhaustive]` as well, so the adapter projects it here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactInfo {
    pub(crate) jid: Jid,
    pub(crate) lid: Option<Jid>,
    pub(crate) verified_name: Option<String>,
}

/// One encrypted media payload to stream into a file. The upstream download
/// entry point is generic over its writer and its payload, so naming the five
/// payloads the daemon downloads keeps [`Transport`] object safe.
pub(crate) enum MediaSource {
    Image(wa::message::ImageMessage),
    Sticker(wa::message::StickerMessage),
    Video(wa::message::VideoMessage),
    Audio(wa::message::AudioMessage),
    Document(wa::message::DocumentMessage),
}

/// Every `WhatsApp` operation the daemon performs. Implementations are the live
/// SDK adapter below and, in tests, a scripted fake.
#[async_trait]
pub(crate) trait Transport: Send + Sync {
    /// This device's phone-number JID, or `None` before pairing completes.
    fn pn(&self) -> Option<Jid>;
    /// This device's LID JID, or `None` before pairing completes.
    fn lid(&self) -> Option<Jid>;
    /// This device's push name; empty until `WhatsApp` restores it.
    fn push_name(&self) -> String;
    /// A fresh outgoing message ID.
    fn generate_message_id(&self) -> String;

    /// The LID/phone-number mapping for `jid`, if `WhatsApp` knows one.
    async fn lid_pn_entry(&self, jid: &Jid) -> Result<Option<LidPnEntry>>;
    /// Full metadata for one group.
    async fn group_metadata(&self, jid: &Jid) -> Result<GroupMetadata>;
    /// Metadata for every group this device participates in.
    async fn participating_groups(&self) -> Result<HashMap<Jid, GroupMetadata>>;
    /// Business-profile information for a batch of contacts.
    async fn user_info(&self, jids: &[Jid]) -> Result<HashMap<Jid, ContactInfo>>;
    /// The preview profile picture for `jid`, under the daemon's own timeout.
    async fn profile_picture(&self, jid: &Jid) -> Result<Option<ProfilePicture>>;

    /// Sends one already-composed message.
    async fn send_message(
        &self,
        chat: &Jid,
        message: wa::Message,
        options: SendOptions,
    ) -> Result<SendReceipt>;
    /// Uploads voice-note bytes and composes the outbound audio message.
    ///
    /// `whatsapp_rust::upload::UploadResponse` is `#[non_exhaustive]`, so a
    /// scripted transport cannot return one; composing the infallible message
    /// here keeps the upload's arguments, ordering, and error unchanged.
    async fn upload_audio_message(
        &self,
        data: Vec<u8>,
        options: media::AudioOptions,
    ) -> Result<wa::Message>;
    /// Sends a reaction to an existing message.
    async fn send_reaction(
        &self,
        chat: &Jid,
        target_key: wa::MessageKey,
        emoji: &str,
    ) -> Result<SendReceipt>;

    /// Sends read receipts for individual messages.
    async fn mark_as_read(
        &self,
        chat: &Jid,
        sender: Option<&Jid>,
        message_ids: &[&str],
    ) -> Result<()>;
    /// Writes the cross-device "chat is read" app-state action.
    async fn mark_chat_as_read(&self, chat: &Jid) -> Result<()>;
    async fn pin_chat(&self, chat: &Jid) -> Result<()>;
    async fn unpin_chat(&self, chat: &Jid) -> Result<()>;

    async fn set_available(&self) -> Result<()>;
    async fn set_unavailable(&self) -> Result<()>;
    async fn subscribe_presence(&self, jid: Jid) -> Result<()>;
    async fn unsubscribe_presence(&self, jid: &Jid) -> Result<()>;
    async fn send_composing(&self, jid: &Jid) -> Result<()>;
    async fn send_recording(&self, jid: &Jid) -> Result<()>;
    async fn send_paused(&self, jid: &Jid) -> Result<()>;

    /// Creates a poll and returns its send receipt plus the creation secret.
    async fn create_poll(
        &self,
        chat: Jid,
        question: &str,
        options: &[String],
        selectable_count: u32,
    ) -> Result<(SendReceipt, Vec<u8>)>;
    /// Creates a single-answer quiz poll with one correct option.
    async fn create_quiz(
        &self,
        chat: Jid,
        question: &str,
        options: &[String],
        correct_index: usize,
    ) -> Result<(SendReceipt, Vec<u8>)>;
    /// Casts this device's vote on an existing poll.
    async fn vote_poll(
        &self,
        chat: Jid,
        poll_message_id: &str,
        poll_creator_jid: &Jid,
        message_secret: &[u8],
        option_names: &[String],
    ) -> Result<SendReceipt>;
    /// Decrypts an incoming vote into its option hashes.
    async fn decrypt_poll_vote(
        &self,
        ciphertext: PollVoteCiphertext<'_>,
        message_secret: &[u8],
        poll_message_id: &str,
        poll_creator_jid: &Jid,
        voter_jid: &Jid,
    ) -> Result<Vec<Vec<u8>>>;

    /// Requests on-demand history around one message from the primary phone.
    async fn fetch_message_history(
        &self,
        chat: &Jid,
        oldest_message_id: &str,
        oldest_message_from_me: bool,
        oldest_message_timestamp_ms: i64,
        count: i32,
    ) -> Result<String>;
    /// Asks the primary phone to resend one exact message.
    async fn request_placeholder_resend(&self, info: &Arc<MessageInfo>) -> Result<()>;
    /// Streams and decrypts one media payload into `file`.
    async fn download(&self, source: &MediaSource, file: std::fs::File) -> Result<std::fs::File>;
    /// Unlinks this device from the `WhatsApp` account.
    async fn logout(&self);
}

/// The live adapter. Every method below is a single upstream SDK/network call
/// whose behavior terminates outside this process, which is exactly the
/// carve-out `QUALITY.md` allows `coverage(off)` for; the orchestration that
/// decides when and with what to call them stays measured.
pub(crate) struct ClientTransport(pub(crate) Arc<Client>);

#[async_trait]
impl Transport for ClientTransport {
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn pn(&self) -> Option<Jid> {
        self.0.pn()
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn lid(&self) -> Option<Jid> {
        self.0.lid()
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn push_name(&self) -> String {
        self.0.push_name()
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn generate_message_id(&self) -> String {
        self.0.generate_message_id()
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn lid_pn_entry(&self, jid: &Jid) -> Result<Option<LidPnEntry>> {
        self.0.get_lid_pn_entry(jid).await
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn group_metadata(&self, jid: &Jid) -> Result<GroupMetadata> {
        Ok(self.0.groups().get_metadata(jid).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn participating_groups(&self) -> Result<HashMap<Jid, GroupMetadata>> {
        Ok(self.0.groups().get_participating().await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn user_info(&self, jids: &[Jid]) -> Result<HashMap<Jid, ContactInfo>> {
        Ok(self
            .0
            .contacts()
            .get_user_info(jids)
            .await?
            .into_iter()
            .map(|(key, info)| {
                (
                    key,
                    ContactInfo {
                        jid: info.jid,
                        lid: info.lid,
                        verified_name: info.verified_name.and_then(|verified| verified.name),
                    },
                )
            })
            .collect())
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn profile_picture(&self, jid: &Jid) -> Result<Option<ProfilePicture>> {
        // The generic picture IQ accepts both contact and group JIDs and
        // supports a short timeout. The dedicated group batch IQ can wait for
        // the global IQ timeout when even one stale group is included,
        // delaying every avatar.
        Ok(self
            .0
            .contacts()
            .get_profile_picture_with_timeout(jid, true, Some(std::time::Duration::from_secs(6)))
            .await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn send_message(
        &self,
        chat: &Jid,
        message: wa::Message,
        options: SendOptions,
    ) -> Result<SendReceipt> {
        let result = self
            .0
            .send_message_with_options(chat, message, options)
            .await?;
        Ok(SendReceipt {
            message_id: result.message_id,
        })
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn upload_audio_message(
        &self,
        data: Vec<u8>,
        options: media::AudioOptions,
    ) -> Result<wa::Message> {
        let upload = self
            .0
            .upload(data, MediaType::Audio, UploadOptions::new())
            .await?;
        Ok(media::audio_message(upload, options))
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn send_reaction(
        &self,
        chat: &Jid,
        target_key: wa::MessageKey,
        emoji: &str,
    ) -> Result<SendReceipt> {
        let result = self.0.send_reaction(chat, target_key, emoji).await?;
        Ok(SendReceipt {
            message_id: result.message_id,
        })
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn mark_as_read(
        &self,
        chat: &Jid,
        sender: Option<&Jid>,
        message_ids: &[&str],
    ) -> Result<()> {
        self.0.mark_as_read(chat, sender, message_ids).await
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn mark_chat_as_read(&self, chat: &Jid) -> Result<()> {
        Ok(self
            .0
            .chat_actions()
            .mark_chat_as_read(chat, true, None)
            .await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn pin_chat(&self, chat: &Jid) -> Result<()> {
        Ok(self.0.chat_actions().pin_chat(chat).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn unpin_chat(&self, chat: &Jid) -> Result<()> {
        Ok(self.0.chat_actions().unpin_chat(chat).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn set_available(&self) -> Result<()> {
        Ok(self.0.presence().set_available().await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn set_unavailable(&self) -> Result<()> {
        Ok(self.0.presence().set_unavailable().await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn subscribe_presence(&self, jid: Jid) -> Result<()> {
        Ok(self.0.presence().subscribe(jid).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn unsubscribe_presence(&self, jid: &Jid) -> Result<()> {
        Ok(self.0.presence().unsubscribe(jid).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn send_composing(&self, jid: &Jid) -> Result<()> {
        Ok(self.0.chatstate().send_composing(jid).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn send_recording(&self, jid: &Jid) -> Result<()> {
        Ok(self.0.chatstate().send_recording(jid).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn send_paused(&self, jid: &Jid) -> Result<()> {
        Ok(self.0.chatstate().send_paused(jid).await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn create_poll(
        &self,
        chat: Jid,
        question: &str,
        options: &[String],
        selectable_count: u32,
    ) -> Result<(SendReceipt, Vec<u8>)> {
        let (result, message_secret) = self
            .0
            .polls()
            .create(chat, question, options, selectable_count)
            .await?;
        Ok((
            SendReceipt {
                message_id: result.message_id,
            },
            message_secret,
        ))
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn create_quiz(
        &self,
        chat: Jid,
        question: &str,
        options: &[String],
        correct_index: usize,
    ) -> Result<(SendReceipt, Vec<u8>)> {
        let (result, message_secret) = self
            .0
            .polls()
            .create_quiz(chat, question, options, correct_index)
            .await?;
        Ok((
            SendReceipt {
                message_id: result.message_id,
            },
            message_secret,
        ))
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn vote_poll(
        &self,
        chat: Jid,
        poll_message_id: &str,
        poll_creator_jid: &Jid,
        message_secret: &[u8],
        option_names: &[String],
    ) -> Result<SendReceipt> {
        let result = self
            .0
            .polls()
            .vote(
                chat,
                poll_message_id,
                poll_creator_jid,
                message_secret,
                option_names,
            )
            .await?;
        Ok(SendReceipt {
            message_id: result.message_id,
        })
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn decrypt_poll_vote(
        &self,
        ciphertext: PollVoteCiphertext<'_>,
        message_secret: &[u8],
        poll_message_id: &str,
        poll_creator_jid: &Jid,
        voter_jid: &Jid,
    ) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .0
            .polls()
            .decrypt_vote(
                ciphertext,
                message_secret,
                poll_message_id,
                poll_creator_jid,
                voter_jid,
            )
            .await?)
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn fetch_message_history(
        &self,
        chat: &Jid,
        oldest_message_id: &str,
        oldest_message_from_me: bool,
        oldest_message_timestamp_ms: i64,
        count: i32,
    ) -> Result<String> {
        self.0
            .fetch_message_history(
                chat,
                oldest_message_id,
                oldest_message_from_me,
                oldest_message_timestamp_ms,
                count,
            )
            .await
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn request_placeholder_resend(&self, info: &Arc<MessageInfo>) -> Result<()> {
        self.0.send_pdo_placeholder_resend_request(info).await
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn download(&self, source: &MediaSource, file: std::fs::File) -> Result<std::fs::File> {
        match source {
            MediaSource::Image(image) => self.0.download_to_writer(image, file).await,
            MediaSource::Sticker(sticker) => self.0.download_to_writer(sticker, file).await,
            MediaSource::Video(video) => self.0.download_to_writer(video, file).await,
            MediaSource::Audio(audio) => self.0.download_to_writer(audio, file).await,
            MediaSource::Document(document) => self.0.download_to_writer(document, file).await,
        }
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    async fn logout(&self) {
        self.0.logout().await;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod fake {
    //! A scripted [`Transport`] for daemon tests: every method answers from a
    //! settable field and appends a typed entry to one call log, so a test can
    //! assert both what the daemon decided to ask `WhatsApp` and how it reacted
    //! to the answer.

    use super::{ContactInfo, MediaSource, SendReceipt, Transport};
    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use buffa::MessageField;
    use std::collections::{HashMap, VecDeque};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use whatsapp_rust::prelude::{Jid, MessageInfo, SendOptions, wa};
    use whatsapp_rust::wacore::types::lid_pn::LidPnEntry;
    use whatsapp_rust::{GroupMetadata, PollVoteCiphertext, ProfilePicture, media};

    /// Which [`Transport`] method a [`Call`] came from. Doubles as the key for
    /// scripted failures and for [`FakeTransport::calls_of`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub(crate) enum CallKind {
        Pn,
        Lid,
        PushName,
        GenerateMessageId,
        LidPnEntry,
        GroupMetadata,
        ParticipatingGroups,
        UserInfo,
        ProfilePicture,
        SendMessage,
        UploadAudioMessage,
        SendReaction,
        MarkAsRead,
        MarkChatAsRead,
        PinChat,
        UnpinChat,
        SetAvailable,
        SetUnavailable,
        SubscribePresence,
        UnsubscribePresence,
        SendComposing,
        SendRecording,
        SendPaused,
        CreatePoll,
        CreateQuiz,
        VotePoll,
        DecryptPollVote,
        FetchMessageHistory,
        RequestPlaceholderResend,
        Download,
        Logout,
    }

    /// Which media payload a `Download` call carried.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub(crate) enum MediaKind {
        Image,
        Sticker,
        Video,
        Audio,
        Document,
    }

    /// One recorded [`Transport`] call with its salient arguments.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum Call {
        Pn,
        Lid,
        PushName,
        GenerateMessageId,
        LidPnEntry(String),
        GroupMetadata(String),
        ParticipatingGroups,
        UserInfo(Vec<String>),
        ProfilePicture(String),
        SendMessage {
            chat: String,
            message: Box<wa::Message>,
            message_id: Option<String>,
        },
        UploadAudioMessage {
            byte_count: usize,
            duration_seconds: Option<u32>,
            ptt: Option<bool>,
        },
        SendReaction {
            chat: String,
            target_key: Box<wa::MessageKey>,
            emoji: String,
        },
        MarkAsRead {
            chat: String,
            sender: Option<String>,
            message_ids: Vec<String>,
        },
        MarkChatAsRead(String),
        PinChat(String),
        UnpinChat(String),
        SetAvailable,
        SetUnavailable,
        SubscribePresence(String),
        UnsubscribePresence(String),
        SendComposing(String),
        SendRecording(String),
        SendPaused(String),
        CreatePoll {
            chat: String,
            question: String,
            options: Vec<String>,
            selectable_count: u32,
        },
        CreateQuiz {
            chat: String,
            question: String,
            options: Vec<String>,
            correct_index: usize,
        },
        VotePoll {
            chat: String,
            poll_message_id: String,
            creator_jid: String,
            message_secret: Vec<u8>,
            option_names: Vec<String>,
        },
        DecryptPollVote {
            enc_payload: Vec<u8>,
            enc_iv: Vec<u8>,
            message_secret: Vec<u8>,
            poll_message_id: String,
            creator_jid: String,
            voter_jid: String,
        },
        FetchMessageHistory {
            chat: String,
            oldest_message_id: String,
            oldest_message_from_me: bool,
            oldest_message_timestamp_ms: i64,
            count: i32,
        },
        RequestPlaceholderResend {
            chat: String,
            message_id: String,
        },
        Download(MediaKind),
        Logout,
    }

    impl Call {
        pub(crate) fn kind(&self) -> CallKind {
            match self {
                Self::Pn => CallKind::Pn,
                Self::Lid => CallKind::Lid,
                Self::PushName => CallKind::PushName,
                Self::GenerateMessageId => CallKind::GenerateMessageId,
                Self::LidPnEntry(_) => CallKind::LidPnEntry,
                Self::GroupMetadata(_) => CallKind::GroupMetadata,
                Self::ParticipatingGroups => CallKind::ParticipatingGroups,
                Self::UserInfo(_) => CallKind::UserInfo,
                Self::ProfilePicture(_) => CallKind::ProfilePicture,
                Self::SendMessage { .. } => CallKind::SendMessage,
                Self::UploadAudioMessage { .. } => CallKind::UploadAudioMessage,
                Self::SendReaction { .. } => CallKind::SendReaction,
                Self::MarkAsRead { .. } => CallKind::MarkAsRead,
                Self::MarkChatAsRead(_) => CallKind::MarkChatAsRead,
                Self::PinChat(_) => CallKind::PinChat,
                Self::UnpinChat(_) => CallKind::UnpinChat,
                Self::SetAvailable => CallKind::SetAvailable,
                Self::SetUnavailable => CallKind::SetUnavailable,
                Self::SubscribePresence(_) => CallKind::SubscribePresence,
                Self::UnsubscribePresence(_) => CallKind::UnsubscribePresence,
                Self::SendComposing(_) => CallKind::SendComposing,
                Self::SendRecording(_) => CallKind::SendRecording,
                Self::SendPaused(_) => CallKind::SendPaused,
                Self::CreatePoll { .. } => CallKind::CreatePoll,
                Self::CreateQuiz { .. } => CallKind::CreateQuiz,
                Self::VotePoll { .. } => CallKind::VotePoll,
                Self::DecryptPollVote { .. } => CallKind::DecryptPollVote,
                Self::FetchMessageHistory { .. } => CallKind::FetchMessageHistory,
                Self::RequestPlaceholderResend { .. } => CallKind::RequestPlaceholderResend,
                Self::Download(_) => CallKind::Download,
                Self::Logout => CallKind::Logout,
            }
        }
    }

    fn media_kind(source: &MediaSource) -> MediaKind {
        match source {
            MediaSource::Image(_) => MediaKind::Image,
            MediaSource::Sticker(_) => MediaKind::Sticker,
            MediaSource::Video(_) => MediaKind::Video,
            MediaSource::Audio(_) => MediaKind::Audio,
            MediaSource::Document(_) => MediaKind::Document,
        }
    }

    /// Scripted answers plus the recorded call log. Defaults succeed with empty
    /// results, so a test only scripts the answers its scenario depends on.
    #[derive(Default)]
    pub(crate) struct FakeTransport {
        pub(crate) pn: Mutex<Option<Jid>>,
        pub(crate) lid: Mutex<Option<Jid>>,
        pub(crate) push_name: Mutex<String>,
        /// Consumed in order by `generate_message_id`; empty yields `FAKE-<n>`.
        pub(crate) message_ids: Mutex<VecDeque<String>>,
        /// Keyed by the queried JID's `to_string()`.
        pub(crate) lid_pn_entries: Mutex<HashMap<String, Option<LidPnEntry>>>,
        /// Keyed by the queried JID's `to_string()`.
        pub(crate) group_metadata: Mutex<HashMap<String, GroupMetadata>>,
        pub(crate) participating_groups: Mutex<HashMap<Jid, GroupMetadata>>,
        pub(crate) user_info: Mutex<HashMap<Jid, ContactInfo>>,
        /// Keyed by the queried JID's `to_string()`.
        pub(crate) profile_pictures: Mutex<HashMap<String, Option<ProfilePicture>>>,
        /// Returned by `create_poll` and `create_quiz`.
        pub(crate) poll_secret: Mutex<Vec<u8>>,
        /// Returned by `decrypt_poll_vote`.
        pub(crate) poll_vote_hashes: Mutex<Vec<Vec<u8>>>,
        /// Returned by `fetch_message_history`.
        pub(crate) history_request_id: Mutex<String>,
        /// Written into the file handed to `download`.
        pub(crate) download_bytes: Mutex<Vec<u8>>,
        /// Per-method scripted failure messages.
        pub(crate) errors: Mutex<HashMap<CallKind, String>>,
        pub(crate) calls: Mutex<Vec<Call>>,
        generated_ids: AtomicU64,
        /// Per-method scripted delays. A caller's own timeout is only
        /// observable when the answer arrives late, and under
        /// `#[tokio::test(start_paused = true)]` that stays instant.
        pub(crate) delays: Mutex<HashMap<CallKind, std::time::Duration>>,
        /// Overrides the receipt `send_message` returns, so the daemon's
        /// "`WhatsApp` answered with a different message ID" guard is reachable.
        pub(crate) send_receipt_id: Mutex<Option<String>>,
        /// Runs after a call is recorded and before its scripted answer, so a
        /// test can mutate daemon state exactly while one transport call is in
        /// flight. Unset by default, which keeps every existing scenario
        /// unchanged.
        #[allow(clippy::type_complexity)]
        pub(crate) call_hook: Mutex<Option<Box<dyn Fn(CallKind) + Send + Sync>>>,
    }

    impl FakeTransport {
        #[must_use]
        pub(crate) fn new() -> Self {
            Self::default()
        }

        #[must_use]
        pub(crate) fn with_pn(self, jid: &str) -> Self {
            *Self::lock(&self.pn) = Some(jid.parse().expect("valid fake PN JID"));
            self
        }

        #[must_use]
        pub(crate) fn with_lid(self, jid: &str) -> Self {
            *Self::lock(&self.lid) = Some(jid.parse().expect("valid fake LID JID"));
            self
        }

        #[must_use]
        pub(crate) fn with_push_name(self, name: &str) -> Self {
            *Self::lock(&self.push_name) = name.to_owned();
            self
        }

        #[must_use]
        pub(crate) fn with_message_ids<I, S>(self, ids: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            *Self::lock(&self.message_ids) = ids.into_iter().map(Into::into).collect();
            self
        }

        #[must_use]
        pub(crate) fn with_lid_pn_entry(self, jid: &str, entry: Option<LidPnEntry>) -> Self {
            Self::lock(&self.lid_pn_entries).insert(jid.to_owned(), entry);
            self
        }

        #[must_use]
        pub(crate) fn with_group_metadata(self, jid: &str, metadata: GroupMetadata) -> Self {
            Self::lock(&self.group_metadata).insert(jid.to_owned(), metadata);
            self
        }

        #[must_use]
        pub(crate) fn with_participating_group(self, jid: &str, metadata: GroupMetadata) -> Self {
            Self::lock(&self.participating_groups)
                .insert(jid.parse().expect("valid fake group JID"), metadata);
            self
        }

        #[must_use]
        pub(crate) fn with_user_info(self, info: ContactInfo) -> Self {
            Self::lock(&self.user_info).insert(info.jid.clone(), info);
            self
        }

        #[must_use]
        pub(crate) fn with_profile_picture(
            self,
            jid: &str,
            picture: Option<ProfilePicture>,
        ) -> Self {
            Self::lock(&self.profile_pictures).insert(jid.to_owned(), picture);
            self
        }

        #[must_use]
        pub(crate) fn with_poll_secret(self, secret: &[u8]) -> Self {
            *Self::lock(&self.poll_secret) = secret.to_vec();
            self
        }

        #[must_use]
        pub(crate) fn with_poll_vote_hashes(self, hashes: Vec<Vec<u8>>) -> Self {
            *Self::lock(&self.poll_vote_hashes) = hashes;
            self
        }

        #[must_use]
        pub(crate) fn with_history_request_id(self, id: &str) -> Self {
            *Self::lock(&self.history_request_id) = id.to_owned();
            self
        }

        #[must_use]
        pub(crate) fn with_download_bytes(self, bytes: &[u8]) -> Self {
            *Self::lock(&self.download_bytes) = bytes.to_vec();
            self
        }

        /// Makes every later call of `kind` fail with `message`.
        #[must_use]
        pub(crate) fn failing(self, kind: CallKind, message: &str) -> Self {
            Self::lock(&self.errors).insert(kind, message.to_owned());
            self
        }

        /// Arms a failure after construction, so one scenario can script a
        /// success and a later failure for the same method.
        pub(crate) fn fail(&self, kind: CallKind, message: &str) {
            Self::lock(&self.errors).insert(kind, message.to_owned());
        }

        /// Clears a scripted failure armed by [`FakeTransport::fail`].
        pub(crate) fn succeed(&self, kind: CallKind) {
            Self::lock(&self.errors).remove(&kind);
        }

        /// Every recorded call, oldest first.
        pub(crate) fn calls(&self) -> Vec<Call> {
            Self::lock(&self.calls).clone()
        }

        /// Every recorded call from one method, oldest first.
        pub(crate) fn calls_of(&self, kind: CallKind) -> Vec<Call> {
            Self::lock(&self.calls)
                .iter()
                .filter(|call| call.kind() == kind)
                .cloned()
                .collect()
        }

        /// The method of every recorded call, in order.
        pub(crate) fn call_kinds(&self) -> Vec<CallKind> {
            Self::lock(&self.calls)
                .iter()
                .map(Call::kind)
                .collect::<Vec<_>>()
        }

        /// Drops every recorded call, so a later phase asserts on its own.
        pub(crate) fn clear_calls(&self) {
            Self::lock(&self.calls).clear();
        }

        fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
            value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn record(&self, call: Call) -> Result<()> {
            let kind = call.kind();
            Self::lock(&self.calls).push(call);
            if let Some(hook) = Self::lock(&self.call_hook).as_ref() {
                hook(kind);
            }
            match Self::lock(&self.errors).get(&kind) {
                Some(message) => Err(anyhow!(message.clone())),
                None => Ok(()),
            }
        }

        /// Makes every later call of `kind` answer only after `delay`, so a
        /// caller's timeout is reachable under paused test time. Honored by
        /// `profile_picture`, `send_message`, and `download`.
        #[must_use]
        pub(crate) fn with_delay(self, kind: CallKind, delay: std::time::Duration) -> Self {
            Self::lock(&self.delays).insert(kind, delay);
            self
        }

        /// Makes `send_message` answer with `message_id` instead of echoing
        /// the requested one.
        #[must_use]
        pub(crate) fn with_send_receipt_id(self, message_id: &str) -> Self {
            *Self::lock(&self.send_receipt_id) = Some(message_id.to_owned());
            self
        }

        async fn wait(&self, kind: CallKind) {
            let delay = Self::lock(&self.delays).get(&kind).copied();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
        }

        /// Installs the [`FakeTransport::call_hook`] closure. A daemon function
        /// that re-checks shared state across an `await` can only be observed
        /// doing so if that state changes while one call is in flight.
        pub(crate) fn on_call<F>(&self, hook: F)
        where
            F: Fn(CallKind) + Send + Sync + 'static,
        {
            *Self::lock(&self.call_hook) = Some(Box::new(hook));
        }
    }

    /// Builds an `Arc<dyn Transport>` view of a fake a test still holds.
    pub(crate) fn transport(fake: &Arc<FakeTransport>) -> Arc<dyn Transport> {
        Arc::clone(fake) as Arc<dyn Transport>
    }

    #[async_trait]
    impl Transport for FakeTransport {
        fn pn(&self) -> Option<Jid> {
            let _ = self.record(Call::Pn);
            Self::lock(&self.pn).clone()
        }

        fn lid(&self) -> Option<Jid> {
            let _ = self.record(Call::Lid);
            Self::lock(&self.lid).clone()
        }

        fn push_name(&self) -> String {
            let _ = self.record(Call::PushName);
            Self::lock(&self.push_name).clone()
        }

        fn generate_message_id(&self) -> String {
            let _ = self.record(Call::GenerateMessageId);
            self.next_id()
        }

        async fn lid_pn_entry(&self, jid: &Jid) -> Result<Option<LidPnEntry>> {
            let key = jid.to_string();
            self.record(Call::LidPnEntry(key.clone()))?;
            Ok(Self::lock(&self.lid_pn_entries)
                .get(&key)
                .cloned()
                .flatten())
        }

        async fn group_metadata(&self, jid: &Jid) -> Result<GroupMetadata> {
            let key = jid.to_string();
            self.record(Call::GroupMetadata(key.clone()))?;
            Self::lock(&self.group_metadata)
                .get(&key)
                .cloned()
                .ok_or_else(|| anyhow!("no scripted group metadata for {key}"))
        }

        async fn participating_groups(&self) -> Result<HashMap<Jid, GroupMetadata>> {
            self.record(Call::ParticipatingGroups)?;
            Ok(Self::lock(&self.participating_groups).clone())
        }

        async fn user_info(&self, jids: &[Jid]) -> Result<HashMap<Jid, ContactInfo>> {
            self.record(Call::UserInfo(
                jids.iter().map(ToString::to_string).collect(),
            ))?;
            let scripted = Self::lock(&self.user_info);
            Ok(jids
                .iter()
                .filter_map(|jid| scripted.get(jid).map(|info| (jid.clone(), info.clone())))
                .collect())
        }

        async fn profile_picture(&self, jid: &Jid) -> Result<Option<ProfilePicture>> {
            self.wait(CallKind::ProfilePicture).await;
            let key = jid.to_string();
            self.record(Call::ProfilePicture(key.clone()))?;
            Ok(Self::lock(&self.profile_pictures)
                .get(&key)
                .cloned()
                .flatten())
        }

        async fn send_message(
            &self,
            chat: &Jid,
            message: wa::Message,
            options: SendOptions,
        ) -> Result<SendReceipt> {
            self.wait(CallKind::SendMessage).await;
            let message_id = options.message_id.clone();
            self.record(Call::SendMessage {
                chat: chat.to_string(),
                message: Box::new(message),
                message_id: message_id.clone(),
            })?;
            let forced = Self::lock(&self.send_receipt_id).clone();
            Ok(SendReceipt {
                message_id: forced.or(message_id).unwrap_or_else(|| self.next_id()),
            })
        }

        async fn upload_audio_message(
            &self,
            data: Vec<u8>,
            options: media::AudioOptions,
        ) -> Result<wa::Message> {
            self.record(Call::UploadAudioMessage {
                byte_count: data.len(),
                duration_seconds: options.duration_seconds,
                ptt: options.ptt,
            })?;
            Ok(wa::Message {
                audio_message: MessageField::some(wa::message::AudioMessage {
                    mimetype: options.mimetype,
                    seconds: options.duration_seconds,
                    ptt: options.ptt,
                    file_length: Some(u64::try_from(data.len()).unwrap_or(u64::MAX)),
                    ..wa::message::AudioMessage::default()
                }),
                ..wa::Message::default()
            })
        }

        async fn send_reaction(
            &self,
            chat: &Jid,
            target_key: wa::MessageKey,
            emoji: &str,
        ) -> Result<SendReceipt> {
            self.record(Call::SendReaction {
                chat: chat.to_string(),
                target_key: Box::new(target_key),
                emoji: emoji.to_owned(),
            })?;
            Ok(SendReceipt {
                message_id: self.next_id(),
            })
        }

        async fn mark_as_read(
            &self,
            chat: &Jid,
            sender: Option<&Jid>,
            message_ids: &[&str],
        ) -> Result<()> {
            self.record(Call::MarkAsRead {
                chat: chat.to_string(),
                sender: sender.map(ToString::to_string),
                message_ids: message_ids.iter().map(|id| (*id).to_owned()).collect(),
            })
        }

        async fn mark_chat_as_read(&self, chat: &Jid) -> Result<()> {
            self.record(Call::MarkChatAsRead(chat.to_string()))
        }

        async fn pin_chat(&self, chat: &Jid) -> Result<()> {
            self.record(Call::PinChat(chat.to_string()))
        }

        async fn unpin_chat(&self, chat: &Jid) -> Result<()> {
            self.record(Call::UnpinChat(chat.to_string()))
        }

        async fn set_available(&self) -> Result<()> {
            self.record(Call::SetAvailable)
        }

        async fn set_unavailable(&self) -> Result<()> {
            self.record(Call::SetUnavailable)
        }

        async fn subscribe_presence(&self, jid: Jid) -> Result<()> {
            self.record(Call::SubscribePresence(jid.to_string()))
        }

        async fn unsubscribe_presence(&self, jid: &Jid) -> Result<()> {
            self.record(Call::UnsubscribePresence(jid.to_string()))
        }

        async fn send_composing(&self, jid: &Jid) -> Result<()> {
            self.record(Call::SendComposing(jid.to_string()))
        }

        async fn send_recording(&self, jid: &Jid) -> Result<()> {
            self.record(Call::SendRecording(jid.to_string()))
        }

        async fn send_paused(&self, jid: &Jid) -> Result<()> {
            self.record(Call::SendPaused(jid.to_string()))
        }

        async fn create_poll(
            &self,
            chat: Jid,
            question: &str,
            options: &[String],
            selectable_count: u32,
        ) -> Result<(SendReceipt, Vec<u8>)> {
            self.record(Call::CreatePoll {
                chat: chat.to_string(),
                question: question.to_owned(),
                options: options.to_vec(),
                selectable_count,
            })?;
            Ok((
                SendReceipt {
                    message_id: self.next_id(),
                },
                Self::lock(&self.poll_secret).clone(),
            ))
        }

        async fn create_quiz(
            &self,
            chat: Jid,
            question: &str,
            options: &[String],
            correct_index: usize,
        ) -> Result<(SendReceipt, Vec<u8>)> {
            self.record(Call::CreateQuiz {
                chat: chat.to_string(),
                question: question.to_owned(),
                options: options.to_vec(),
                correct_index,
            })?;
            Ok((
                SendReceipt {
                    message_id: self.next_id(),
                },
                Self::lock(&self.poll_secret).clone(),
            ))
        }

        async fn vote_poll(
            &self,
            chat: Jid,
            poll_message_id: &str,
            poll_creator_jid: &Jid,
            message_secret: &[u8],
            option_names: &[String],
        ) -> Result<SendReceipt> {
            self.record(Call::VotePoll {
                chat: chat.to_string(),
                poll_message_id: poll_message_id.to_owned(),
                creator_jid: poll_creator_jid.to_string(),
                message_secret: message_secret.to_vec(),
                option_names: option_names.to_vec(),
            })?;
            Ok(SendReceipt {
                message_id: self.next_id(),
            })
        }

        async fn decrypt_poll_vote(
            &self,
            ciphertext: PollVoteCiphertext<'_>,
            message_secret: &[u8],
            poll_message_id: &str,
            poll_creator_jid: &Jid,
            voter_jid: &Jid,
        ) -> Result<Vec<Vec<u8>>> {
            self.record(Call::DecryptPollVote {
                enc_payload: ciphertext.enc_payload.to_vec(),
                enc_iv: ciphertext.enc_iv.to_vec(),
                message_secret: message_secret.to_vec(),
                poll_message_id: poll_message_id.to_owned(),
                creator_jid: poll_creator_jid.to_string(),
                voter_jid: voter_jid.to_string(),
            })?;
            Ok(Self::lock(&self.poll_vote_hashes).clone())
        }

        async fn fetch_message_history(
            &self,
            chat: &Jid,
            oldest_message_id: &str,
            oldest_message_from_me: bool,
            oldest_message_timestamp_ms: i64,
            count: i32,
        ) -> Result<String> {
            self.record(Call::FetchMessageHistory {
                chat: chat.to_string(),
                oldest_message_id: oldest_message_id.to_owned(),
                oldest_message_from_me,
                oldest_message_timestamp_ms,
                count,
            })?;
            Ok(Self::lock(&self.history_request_id).clone())
        }

        async fn request_placeholder_resend(&self, info: &Arc<MessageInfo>) -> Result<()> {
            self.record(Call::RequestPlaceholderResend {
                chat: info.source.chat.to_string(),
                message_id: info.id.clone(),
            })
        }

        async fn download(
            &self,
            source: &MediaSource,
            mut file: std::fs::File,
        ) -> Result<std::fs::File> {
            self.wait(CallKind::Download).await;
            self.record(Call::Download(media_kind(source)))?;
            file.write_all(&Self::lock(&self.download_bytes).clone())?;
            file.flush()?;
            Ok(file)
        }

        async fn logout(&self) {
            let _ = self.record(Call::Logout);
        }
    }

    impl FakeTransport {
        fn next_id(&self) -> String {
            if let Some(id) = Self::lock(&self.message_ids).pop_front() {
                return id;
            }
            format!(
                "FAKE-{}",
                self.generated_ids.fetch_add(1, Ordering::Relaxed) + 1
            )
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::fake::{Call, CallKind, FakeTransport, MediaKind};
    use super::{ClientTransport, ContactInfo, MediaSource, SendReceipt, Transport};
    use crate::test_support::synthetic_client;
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::Arc;
    use whatsapp_rust::prelude::{Jid, SendOptions, wa};
    use whatsapp_rust::wacore::types::lid_pn::{LearningSource, LidPnEntry};
    use whatsapp_rust::{GroupMetadata, ProfilePicture};

    #[tokio::test]
    async fn the_fake_answers_from_its_script_and_records_every_call() {
        let fake = Arc::new(
            FakeTransport::new()
                .with_pn("31600000000@s.whatsapp.net")
                .with_lid("100000000000000@lid")
                .with_push_name("Ada")
                .with_message_ids(["SCRIPTED-1"])
                .with_group_metadata(
                    "123-456@g.us",
                    GroupMetadata {
                        subject: "Garden".into(),
                        ..GroupMetadata::default()
                    },
                )
                .with_profile_picture(
                    "31600000000@s.whatsapp.net",
                    Some(ProfilePicture {
                        id: "1".into(),
                        url: "https://example.invalid/avatar.jpg".into(),
                        direct_path: None,
                        hash: None,
                    }),
                )
                .with_user_info(ContactInfo {
                    jid: "31600000000@s.whatsapp.net".parse().unwrap(),
                    lid: None,
                    verified_name: Some("Ada's Flowers".into()),
                })
                .with_lid_pn_entry(
                    "100000000000000@lid",
                    Some(LidPnEntry {
                        lid: "100000000000000".into(),
                        phone_number: "31600000000".into(),
                        created_at: 1,
                        learning_source: LearningSource::Usync,
                    }),
                )
                .with_participating_group(
                    "123-456@g.us",
                    GroupMetadata {
                        subject: "Garden".into(),
                        ..GroupMetadata::default()
                    },
                )
                .with_poll_secret(b"secret")
                .with_poll_vote_hashes(vec![b"hash".to_vec()])
                .with_history_request_id("request-1")
                .with_download_bytes(b"payload"),
        );
        let transport: Arc<dyn Transport> = super::fake::transport(&fake);
        let chat: Jid = "123-456@g.us".parse().unwrap();
        let contact: Jid = "31600000000@s.whatsapp.net".parse().unwrap();

        assert_eq!(transport.push_name(), "Ada");
        assert_eq!(transport.pn(), Some(contact.clone()));
        assert_eq!(
            transport.lid(),
            Some("100000000000000@lid".parse().unwrap())
        );
        assert_eq!(transport.generate_message_id(), "SCRIPTED-1");
        assert!(transport.generate_message_id().starts_with("FAKE-"));
        assert!(transport.lid_pn_entry(&contact).await.unwrap().is_none());
        assert_eq!(
            transport
                .lid_pn_entry(&"100000000000000@lid".parse().unwrap())
                .await
                .unwrap()
                .unwrap()
                .phone_number
                .as_ref(),
            "31600000000"
        );
        assert_eq!(
            transport.group_metadata(&chat).await.unwrap().subject,
            "Garden"
        );
        assert_eq!(
            transport.participating_groups().await.unwrap()[&chat].subject,
            "Garden"
        );
        assert_eq!(
            transport
                .user_info(std::slice::from_ref(&contact))
                .await
                .unwrap()[&contact]
                .verified_name,
            Some("Ada's Flowers".into())
        );
        assert_eq!(
            transport
                .profile_picture(&contact)
                .await
                .unwrap()
                .unwrap()
                .url,
            "https://example.invalid/avatar.jpg"
        );
        assert_eq!(
            transport
                .send_message(
                    &contact,
                    wa::Message::default(),
                    SendOptions::default().with_message_id("OUT-1"),
                )
                .await
                .unwrap(),
            SendReceipt {
                message_id: "OUT-1".into(),
            }
        );
        assert_eq!(
            transport
                .create_poll(chat.clone(), "Lunch?", &["Soup".to_owned()], 1)
                .await
                .unwrap()
                .1,
            b"secret".to_vec()
        );
        assert_eq!(
            transport
                .decrypt_poll_vote(
                    whatsapp_rust::PollVoteCiphertext {
                        enc_payload: b"payload",
                        enc_iv: b"iv",
                    },
                    b"secret",
                    "poll-1",
                    &contact,
                    &contact,
                )
                .await
                .unwrap(),
            vec![b"hash".to_vec()]
        );
        assert_eq!(
            transport
                .fetch_message_history(&chat, "oldest", false, 1_000, 25)
                .await
                .unwrap(),
            "request-1"
        );
        transport
            .mark_as_read(&chat, Some(&contact), &["m1"])
            .await
            .unwrap();
        transport.mark_chat_as_read(&chat).await.unwrap();
        transport.logout().await;

        let mut file = tempfile::tempfile().unwrap();
        file = transport
            .download(
                &MediaSource::Image(wa::message::ImageMessage::default()),
                file,
            )
            .await
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut downloaded = Vec::new();
        file.read_to_end(&mut downloaded).unwrap();
        assert_eq!(downloaded, b"payload");

        assert_eq!(
            fake.calls_of(CallKind::GroupMetadata),
            vec![Call::GroupMetadata("123-456@g.us".into())]
        );
        assert_eq!(
            fake.calls_of(CallKind::MarkAsRead),
            vec![Call::MarkAsRead {
                chat: "123-456@g.us".into(),
                sender: Some("31600000000@s.whatsapp.net".into()),
                message_ids: vec!["m1".into()],
            }]
        );
        assert_eq!(
            fake.calls_of(CallKind::Download),
            vec![Call::Download(MediaKind::Image)]
        );
        assert_eq!(fake.calls().len(), fake.call_kinds().len());
        assert!(fake.call_kinds().contains(&CallKind::Logout));
        fake.clear_calls();
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn scripted_failures_apply_per_method_and_can_be_cleared() {
        let fake =
            Arc::new(FakeTransport::new().failing(CallKind::PinChat, "WhatsApp rejected the pin"));
        let transport: Arc<dyn Transport> = super::fake::transport(&fake);
        let chat: Jid = "1@s.whatsapp.net".parse().unwrap();

        assert_eq!(
            transport.pin_chat(&chat).await.unwrap_err().to_string(),
            "WhatsApp rejected the pin"
        );
        transport.unpin_chat(&chat).await.unwrap();
        fake.succeed(CallKind::PinChat);
        transport.pin_chat(&chat).await.unwrap();
        fake.fail(CallKind::UnpinChat, "offline");
        assert_eq!(
            transport.unpin_chat(&chat).await.unwrap_err().to_string(),
            "offline"
        );

        // A missing scripted answer is a failure rather than a silent default,
        // so a test cannot pass on an unconfigured group lookup.
        assert!(transport.group_metadata(&chat).await.is_err());
    }

    #[tokio::test]
    async fn the_live_adapter_wraps_a_real_client() {
        let directory = tempfile::tempdir().unwrap();
        let client = synthetic_client(&directory).await;
        let transport: Arc<dyn Transport> = Arc::new(ClientTransport(Arc::clone(&client)));

        assert!(transport.pn().is_none());
        assert!(transport.lid().is_none());
        assert!(transport.push_name().is_empty());
        assert!(!transport.generate_message_id().is_empty());
    }
}
