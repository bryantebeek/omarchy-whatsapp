#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const PROTOCOL_VERSION: u16 = 28;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionStatus {
    #[default]
    Starting,
    Pairing {
        code: String,
        expires_at: i64,
    },
    Connected,
    Disconnected {
        reason: String,
    },
    LoggedOut,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Chat {
    pub jid: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    pub last_message: String,
    #[serde(default)]
    pub last_sender_name: String,
    pub last_timestamp: i64,
    pub unread: u32,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub muted: bool,
    pub is_group: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatParticipant {
    pub jid: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub is_me: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatState {
    Typing,
    Recording,
    Paused,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatStateResyncStatus {
    Idle,
    Requested,
    Syncing,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub chat_jid: String,
    pub sender_jid: String,
    pub sender_name: String,
    pub text: String,
    pub timestamp: i64,
    pub from_me: bool,
    /// Outgoing receipt state: 0 unknown, 1 sent, 2 delivered, 3 read, 4 played.
    #[serde(default)]
    pub receipt: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered_to: Vec<MessageDelivery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_by: Vec<MessageReader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MessageMedia>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageDelivery {
    pub jid: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageReader {
    pub jid: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub from_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollOption {
    pub name: String,
    #[serde(default)]
    pub votes: u32,
    #[serde(default)]
    pub selected_by_me: bool,
    #[serde(default)]
    pub voter_jids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceOutboxStatus {
    Sending,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceOutboxEntry {
    pub recording_id: String,
    pub chat_jid: String,
    pub duration_ms: u64,
    pub status: VoiceOutboxStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextOutboxStatus {
    Queued,
    Sending,
    Failed,
    Sent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextOutboxEntry {
    pub delivery_id: String,
    pub chat_jid: String,
    pub text: String,
    pub status: TextOutboxStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Chats,
    Messages,
    Unread,
    Avatars,
    TextOutbox,
    VoiceOutbox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageMedia {
    Image {
        path: String,
        #[serde(default)]
        thumbnail_path: String,
        #[serde(default)]
        downloaded: bool,
        mime_type: String,
        width: u32,
        height: u32,
    },
    Sticker {
        path: String,
        #[serde(default)]
        thumbnail_path: String,
        #[serde(default)]
        downloaded: bool,
        mime_type: String,
        width: u32,
        height: u32,
        #[serde(default)]
        animated: bool,
        #[serde(default)]
        lottie: bool,
        #[serde(default)]
        accessibility_label: String,
    },
    Video {
        path: String,
        thumbnail_path: String,
        #[serde(default)]
        downloaded: bool,
        mime_type: String,
        width: u32,
        height: u32,
        duration_seconds: u32,
        gif_playback: bool,
    },
    Audio {
        path: String,
        #[serde(default)]
        downloaded: bool,
        mime_type: String,
        duration_seconds: u32,
        voice_message: bool,
    },
    Document {
        path: String,
        file_name: String,
        mime_type: String,
        file_size: u64,
        page_count: u32,
    },
    Location {
        latitude_e7: i64,
        longitude_e7: i64,
        accuracy_m: u32,
        name: String,
        address: String,
        thumbnail_path: Option<String>,
        live: bool,
        updated_at: i64,
        #[serde(default)]
        duration_seconds: u32,
    },
    Poll {
        question: String,
        options: Vec<PollOption>,
        selectable_count: u32,
        #[serde(default)]
        total_voters: u32,
        #[serde(default)]
        quiz: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correct_option_index: Option<u32>,
        #[serde(default)]
        end_timestamp: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    GetState,
    ListChats {
        limit: u32,
    },
    GetMessages {
        chat_jid: String,
        limit: u32,
    },
    GetGroupParticipants {
        chat_jid: String,
    },
    SendMessage {
        chat_jid: String,
        text: String,
        /// Stable client-generated identity used for durable, idempotent delivery.
        delivery_id: String,
    },
    SendVoiceMessage {
        chat_jid: String,
        recording_id: String,
    },
    DiscardVoiceRecording {
        recording_id: String,
    },
    ListVoiceOutbox,
    ListTextOutbox,
    RetryTextMessage {
        delivery_id: String,
    },
    DiscardTextMessage {
        delivery_id: String,
    },
    CreatePoll {
        chat_jid: String,
        question: String,
        options: Vec<String>,
        selectable_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correct_option_index: Option<u32>,
    },
    VotePoll {
        chat_jid: String,
        message_id: String,
        selected_options: Vec<String>,
    },
    DownloadImage {
        chat_jid: String,
        message_id: String,
    },
    DownloadSticker {
        chat_jid: String,
        message_id: String,
    },
    DownloadVideo {
        chat_jid: String,
        message_id: String,
    },
    DownloadAudio {
        chat_jid: String,
        message_id: String,
    },
    React {
        chat_jid: String,
        message_id: String,
        sender_jid: String,
        target_from_me: bool,
        emoji: String,
    },
    MarkRead {
        chat_jid: String,
    },
    SetChatPinned {
        chat_jid: String,
        pinned: bool,
    },
    RequestAvatar {
        jid: String,
    },
    ListAvatars,
    SetActiveChat {
        chat_jid: Option<String>,
    },
    SetPresence {
        available: bool,
    },
    SetChatState {
        chat_jid: String,
        state: ChatState,
    },
    ResyncChatState,
    Logout,
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(flatten)]
    pub command: Command,
}

impl ClientFrame {
    #[must_use]
    pub fn new(id: Option<u64>, command: Command) -> Self {
        Self { id, command }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ServerEvent {
    Hello {
        protocol_version: u16,
    },
    State {
        status: ConnectionStatus,
        unread_total: u32,
    },
    Chats {
        chats: Vec<Chat>,
    },
    Messages {
        chat_jid: String,
        messages: Vec<Message>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_unread_message_id: Option<String>,
    },
    GroupParticipants {
        chat_jid: String,
        participants: Vec<ChatParticipant>,
    },
    Message {
        message: Message,
    },
    MediaDownloaded {
        chat_jid: String,
        message_id: String,
        media: MessageMedia,
    },
    MediaDownloadFailed {
        chat_jid: String,
        message_id: String,
        message: String,
    },
    Sent {
        message: Message,
    },
    TextAccepted {
        delivery_id: String,
    },
    TextOutbox {
        entries: Vec<TextOutboxEntry>,
    },
    TextDelivery {
        delivery_id: String,
        status: TextOutboxStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Invalidated {
        resource: Resource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Receipts {
        message_ids: Vec<String>,
        receipt: u8,
        #[serde(default)]
        timestamp: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<MessageDelivery>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reader: Option<MessageReader>,
    },
    Presence {
        jid: String,
        available: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_seen: Option<i64>,
    },
    ChatState {
        chat_jid: String,
        sender_jid: String,
        sender_name: String,
        state: ChatState,
    },
    ChatStateResync {
        status: ChatStateResyncStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Unread {
        total: u32,
    },
    Avatars {
        revision: u64,
        jids: Vec<String>,
        #[serde(default)]
        changed_jids: Vec<String>,
    },
    VoiceOutbox {
        entries: Vec<VoiceOutboxEntry>,
    },
    Ack,
    Pong,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// `WhatsApp` client generation that produced this frame. A newer generation
    /// invalidates delayed callbacks and responses from an older connection.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub generation: u64,
    /// Monotonic daemon-local publication revision. Clients track it per
    /// resource so delayed snapshots cannot regress newer state.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sequence: u64,
    #[serde(flatten)]
    pub event: ServerEvent,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip predicates receive references.
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ServerFrame {
    #[must_use]
    pub fn response(id: Option<u64>, event: ServerEvent) -> Self {
        Self {
            id,
            generation: 0,
            sequence: 0,
            event,
        }
    }

    #[must_use]
    pub fn event(event: ServerEvent) -> Self {
        Self::response(None, event)
    }

    #[must_use]
    pub const fn with_metadata(mut self, generation: u64, sequence: u64) -> Self {
        self.generation = generation;
        self.sequence = sequence;
        self
    }
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub runtime_dir: PathBuf,
    pub state_dir: PathBuf,
    pub socket: PathBuf,
    pub protocol_db: PathBuf,
    pub history_db: PathBuf,
}

impl AppPaths {
    #[must_use]
    pub fn discover() -> Self {
        let runtime_base = runtime_base(
            std::env::var_os("XDG_RUNTIME_DIR"),
            &std::env::temp_dir(),
            std::process::id(),
        );
        let state_base = state_base(std::env::var_os("XDG_STATE_HOME"), dirs::state_dir());
        let runtime_dir = runtime_base.join("omarchy-whatsapp");
        let state_dir = state_base.join("omarchy-whatsapp");
        Self {
            socket: runtime_dir.join("daemon.sock"),
            protocol_db: state_dir.join("session.db"),
            history_db: state_dir.join("history.db"),
            runtime_dir,
            state_dir,
        }
    }
}

fn runtime_base(xdg_runtime_dir: Option<OsString>, temp_dir: &Path, pid: u32) -> PathBuf {
    xdg_runtime_dir.map_or_else(
        || temp_dir.join(format!("omarchy-whatsapp-{pid}")),
        PathBuf::from,
    )
}

fn state_base(xdg_state_home: Option<OsString>, platform_state_dir: Option<PathBuf>) -> PathBuf {
    xdg_state_home
        .map(PathBuf::from)
        .or(platform_state_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn command_round_trip_is_stable() {
        let frame = ClientFrame::new(
            Some(7),
            Command::SendMessage {
                chat_jid: "31612345678@s.whatsapp.net".into(),
                text: "hello".into(),
                delivery_id: "synthetic-7".into(),
            },
        );
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn voice_message_commands_round_trip_are_stable() {
        for command in [
            Command::SendVoiceMessage {
                chat_jid: "31612345678@s.whatsapp.net".into(),
                recording_id: "42-7".into(),
            },
            Command::DiscardVoiceRecording {
                recording_id: "42-7".into(),
            },
            Command::ListVoiceOutbox,
        ] {
            let frame = ClientFrame::new(Some(8), command);
            let json = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
        }

        let event = ServerFrame::event(ServerEvent::VoiceOutbox {
            entries: vec![VoiceOutboxEntry {
                recording_id: "42-7".into(),
                chat_jid: "31612345678@s.whatsapp.net".into(),
                duration_ms: 2_400,
                status: VoiceOutboxStatus::Failed,
                error: Some("offline".into()),
                created_at: 42,
            }],
        });
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), event);
    }

    #[test]
    fn sticker_command_and_media_round_trip_are_stable() {
        let frame = ClientFrame::new(
            Some(8),
            Command::DownloadSticker {
                chat_jid: "1@s.whatsapp.net".into(),
                message_id: "sticker-1".into(),
            },
        );
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);

        let media = MessageMedia::Sticker {
            path: "/private/cache/sticker-1.sticker.webp".into(),
            thumbnail_path: "/private/cache/sticker-1.sticker-thumbnail.png".into(),
            downloaded: true,
            mime_type: "image/webp".into(),
            width: 512,
            height: 512,
            animated: true,
            lottie: false,
            accessibility_label: "Dancing parrot".into(),
        };
        let json = serde_json::to_string(&media).unwrap();
        assert_eq!(serde_json::from_str::<MessageMedia>(&json).unwrap(), media);
    }

    #[test]
    fn reaction_command_round_trip_is_stable() {
        let frame = ClientFrame::new(
            Some(8),
            Command::React {
                chat_jid: "123-456@g.us".into(),
                message_id: "parent".into(),
                sender_jid: "1@s.whatsapp.net".into(),
                target_from_me: false,
                emoji: "❤️".into(),
            },
        );
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn presence_and_chat_state_round_trips_are_stable() {
        for command in [
            Command::SetPresence { available: true },
            Command::SetChatState {
                chat_jid: "1@s.whatsapp.net".into(),
                state: ChatState::Typing,
            },
            Command::SetChatState {
                chat_jid: "123-456@g.us".into(),
                state: ChatState::Paused,
            },
        ] {
            let frame = ClientFrame::new(Some(8), command);
            let json = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
        }

        for event in [
            ServerEvent::Presence {
                jid: "1@s.whatsapp.net".into(),
                available: false,
                last_seen: Some(42),
            },
            ServerEvent::ChatState {
                chat_jid: "123-456@g.us".into(),
                sender_jid: "1@s.whatsapp.net".into(),
                sender_name: "Ada".into(),
                state: ChatState::Recording,
            },
        ] {
            let frame = ServerFrame::event(event);
            let json = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
        }
    }

    #[test]
    fn set_chat_pinned_command_round_trip_is_stable() {
        let frame = ClientFrame::new(
            Some(9),
            Command::SetChatPinned {
                chat_jid: "123-456@g.us".into(),
                pinned: true,
            },
        );
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            json,
            r#"{"id":9,"command":"set_chat_pinned","chat_jid":"123-456@g.us","pinned":true}"#
        );
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn poll_commands_and_media_round_trip_are_stable() {
        for command in [
            Command::CreatePoll {
                chat_jid: "123-456@g.us".into(),
                question: "Lunch?".into(),
                options: vec!["Soup".into(), "Salad".into()],
                selectable_count: 1,
                correct_option_index: None,
            },
            Command::VotePoll {
                chat_jid: "123-456@g.us".into(),
                message_id: "poll-1".into(),
                selected_options: vec!["Soup".into()],
            },
        ] {
            let frame = ClientFrame::new(Some(9), command);
            let json = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
        }

        let media = MessageMedia::Poll {
            question: "Lunch?".into(),
            options: vec![PollOption {
                name: "Soup".into(),
                votes: 2,
                selected_by_me: true,
                voter_jids: vec!["me".into(), "2@s.whatsapp.net".into()],
            }],
            selectable_count: 1,
            total_voters: 2,
            quiz: false,
            correct_option_index: None,
            end_timestamp: 0,
        };
        let json = serde_json::to_string(&media).unwrap();
        assert_eq!(serde_json::from_str::<MessageMedia>(&json).unwrap(), media);
        assert!(json.contains("\"voter_jids\":[\"me\",\"2@s.whatsapp.net\"]"));
        assert!(!json.contains("message_secret"));
        let legacy = serde_json::from_str::<MessageMedia>(
            r#"{"kind":"poll","question":"Old","options":[{"name":"One","votes":1,"selected_by_me":false}],"selectable_count":1}"#,
        )
        .unwrap();
        let MessageMedia::Poll { options, .. } = legacy else {
            unreachable!()
        };
        assert!(options[0].voter_jids.is_empty());
    }

    #[test]
    fn logout_command_round_trip_is_stable() {
        let frame = ClientFrame::new(Some(10), Command::Logout);
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(json, r#"{"id":10,"command":"logout"}"#);
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn chat_state_resync_round_trip_is_stable() {
        let request = ClientFrame::new(Some(11), Command::ResyncChatState);
        let request_json = serde_json::to_string(&request).unwrap();
        assert_eq!(request_json, r#"{"id":11,"command":"resync_chat_state"}"#);
        assert_eq!(
            serde_json::from_str::<ClientFrame>(&request_json).unwrap(),
            request
        );

        let response = ServerFrame::event(ServerEvent::ChatStateResync {
            status: ChatStateResyncStatus::Failed,
            message: Some("synthetic failure".into()),
        });
        let response_json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerFrame>(&response_json).unwrap(),
            response
        );
    }

    #[test]
    fn group_participants_round_trip_is_stable() {
        let request = ClientFrame::new(
            Some(9),
            Command::GetGroupParticipants {
                chat_jid: "123-456@g.us".into(),
            },
        );
        let request_json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientFrame>(&request_json).unwrap(),
            request
        );

        let response = ServerFrame::event(ServerEvent::GroupParticipants {
            chat_jid: "123-456@g.us".into(),
            participants: vec![ChatParticipant {
                jid: "1@s.whatsapp.net".into(),
                name: "Ada".into(),
                aliases: vec!["246204789186724@lid".into()],
                is_me: false,
            }],
        });
        let response_json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerFrame>(&response_json).unwrap(),
            response
        );

        let legacy = serde_json::from_str::<ChatParticipant>(
            r#"{"jid":"1@s.whatsapp.net","name":"Ada","is_me":false}"#,
        )
        .unwrap();
        assert!(legacy.aliases.is_empty());
    }

    #[test]
    fn event_round_trip_is_stable() {
        let frame = ServerFrame::event(ServerEvent::Unread { total: 3 });
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn legacy_avatar_event_defaults_to_no_changed_jids() {
        let frame = serde_json::from_str::<ServerFrame>(
            r#"{"event":"avatars","revision":7,"jids":["1@s.whatsapp.net"]}"#,
        )
        .unwrap();
        assert_eq!(
            frame.event,
            ServerEvent::Avatars {
                revision: 7,
                jids: vec!["1@s.whatsapp.net".into()],
                changed_jids: Vec::new(),
            }
        );
    }

    #[test]
    fn response_preserves_the_request_id() {
        let frame = ServerFrame::response(Some(42), ServerEvent::Pong);
        assert_eq!(frame.id, Some(42));
        assert_eq!(frame.event, ServerEvent::Pong);
    }

    #[test]
    fn discovered_paths_share_private_application_directories() {
        let paths = AppPaths::discover();
        assert_eq!(paths.runtime_dir.file_name().unwrap(), "omarchy-whatsapp");
        assert_eq!(paths.state_dir.file_name().unwrap(), "omarchy-whatsapp");
        assert_eq!(paths.socket, paths.runtime_dir.join("daemon.sock"));
        assert_eq!(paths.protocol_db, paths.state_dir.join("session.db"));
        assert_eq!(paths.history_db, paths.state_dir.join("history.db"));
    }

    #[test]
    fn application_base_directories_have_explicit_fallbacks() {
        assert_eq!(
            runtime_base(Some(OsString::from("/run/user/1000")), Path::new("/tmp"), 7,),
            PathBuf::from("/run/user/1000")
        );
        assert_eq!(
            runtime_base(None, Path::new("/tmp"), 7),
            PathBuf::from("/tmp/omarchy-whatsapp-7")
        );
        assert_eq!(
            state_base(Some(OsString::from("/state")), Some("/fallback".into())),
            PathBuf::from("/state")
        );
        assert_eq!(
            state_base(None, Some("/fallback".into())),
            PathBuf::from("/fallback")
        );
        assert_eq!(state_base(None, None), PathBuf::from("."));
    }

    #[test]
    fn unread_message_boundary_round_trip_is_stable() {
        let frame = ServerFrame::event(ServerEvent::Messages {
            chat_jid: "1@s.whatsapp.net".into(),
            messages: Vec::new(),
            first_unread_message_id: Some("first-unread".into()),
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn downloaded_media_event_round_trip_is_stable() {
        let frame = ServerFrame::response(
            Some(7),
            ServerEvent::MediaDownloaded {
                chat_jid: "1@s.whatsapp.net".into(),
                message_id: "image-1".into(),
                media: MessageMedia::Image {
                    path: "/private/cache/image-1.img".into(),
                    thumbnail_path: "/private/cache/image-1.thumbnail.jpg".into(),
                    downloaded: true,
                    mime_type: "image/jpeg".into(),
                    width: 640,
                    height: 480,
                },
            },
        );
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn failed_media_download_event_round_trip_is_stable() {
        let frame = ServerFrame::event(ServerEvent::MediaDownloadFailed {
            chat_jid: "1@s.whatsapp.net".into(),
            message_id: "image-1".into(),
            message: "WhatsApp did not return download details for this image".into(),
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn rich_message_media_round_trip_is_stable() {
        let message = Message {
            id: "image-1".into(),
            chat_jid: "1@s.whatsapp.net".into(),
            sender_jid: "1@s.whatsapp.net".into(),
            sender_name: "Ada".into(),
            text: "A photo".into(),
            timestamp: 42,
            from_me: false,
            receipt: 3,
            delivered_at: Some(40),
            read_at: Some(41),
            delivered_to: vec![MessageDelivery {
                jid: "2@s.whatsapp.net".into(),
                name: "Grace".into(),
                delivered_at: Some(40),
            }],
            read_by: vec![MessageReader {
                jid: "1@s.whatsapp.net".into(),
                name: "Ada".into(),
                read_at: Some(41),
            }],
            media: Some(MessageMedia::Image {
                path: "/private/cache/image-1.img".into(),
                thumbnail_path: "/private/cache/image-1.thumbnail.jpg".into(),
                downloaded: false,
                mime_type: "image/jpeg".into(),
                width: 640,
                height: 480,
            }),
            reactions: vec![Reaction {
                emoji: "👍".into(),
                count: 2,
                from_me: true,
            }],
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), message);
    }

    #[test]
    fn legacy_messages_default_to_an_unknown_receipt() {
        let message = serde_json::from_str::<Message>(
            r#"{"id":"message-1","chat_jid":"1@s.whatsapp.net","sender_jid":"me","sender_name":"You","text":"Hi","timestamp":42,"from_me":true}"#,
        )
        .unwrap();
        assert_eq!(message.receipt, 0);
        assert_eq!(message.delivered_at, None);
        assert_eq!(message.read_at, None);
        assert!(message.delivered_to.is_empty());
        assert!(message.read_by.is_empty());
    }

    #[test]
    fn receipt_event_round_trip_is_stable() {
        let frame = ServerFrame::event(ServerEvent::Receipts {
            message_ids: vec!["message-1".into(), "message-2".into()],
            receipt: 3,
            timestamp: 42,
            delivery: None,
            reader: Some(MessageReader {
                jid: "1@s.whatsapp.net".into(),
                name: "Ada".into(),
                read_at: Some(42),
            }),
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn delivery_event_round_trip_includes_the_recipient() {
        let frame = ServerFrame::event(ServerEvent::Receipts {
            message_ids: vec!["message-1".into()],
            receipt: 2,
            timestamp: 40,
            delivery: Some(MessageDelivery {
                jid: "1@s.whatsapp.net".into(),
                name: "Ada".into(),
                delivered_at: Some(40),
            }),
            reader: None,
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
    }

    #[test]
    fn legacy_image_media_defaults_to_an_undownloaded_preview() {
        let media = serde_json::from_str::<MessageMedia>(
            r#"{"kind":"image","path":"/cache/legacy.img","mime_type":"image/jpeg","width":71,"height":48}"#,
        )
        .unwrap();
        assert_eq!(
            media,
            MessageMedia::Image {
                path: "/cache/legacy.img".into(),
                thumbnail_path: String::new(),
                downloaded: false,
                mime_type: "image/jpeg".into(),
                width: 71,
                height: 48,
            }
        );
    }

    #[test]
    fn document_media_round_trip_is_stable() {
        let media = MessageMedia::Document {
            path: "/private/cache/quote.pdf".into(),
            file_name: "Garden quote.pdf".into(),
            mime_type: "application/pdf".into(),
            file_size: 42_000,
            page_count: 3,
        };
        let json = serde_json::to_string(&media).unwrap();
        assert_eq!(serde_json::from_str::<MessageMedia>(&json).unwrap(), media);
    }

    #[test]
    fn video_media_round_trip_is_stable() {
        let media = MessageMedia::Video {
            path: "/private/cache/clip.video.mp4".into(),
            thumbnail_path: "/private/cache/clip.video-thumbnail.jpg".into(),
            downloaded: false,
            mime_type: "video/mp4".into(),
            width: 1920,
            height: 1080,
            duration_seconds: 12,
            gif_playback: false,
        };
        let json = serde_json::to_string(&media).unwrap();
        assert_eq!(serde_json::from_str::<MessageMedia>(&json).unwrap(), media);
    }

    #[test]
    fn audio_media_round_trip_is_stable() {
        let media = MessageMedia::Audio {
            path: "/private/cache/note.audio.ogg".into(),
            downloaded: false,
            mime_type: "audio/ogg; codecs=opus".into(),
            duration_seconds: 18,
            voice_message: true,
        };
        let json = serde_json::to_string(&media).unwrap();
        assert_eq!(serde_json::from_str::<MessageMedia>(&json).unwrap(), media);
    }

    #[test]
    fn legacy_live_location_defaults_to_an_unknown_duration() {
        let media = serde_json::from_str::<MessageMedia>(
            r#"{"kind":"location","latitude_e7":523701600,"longitude_e7":48953000,"accuracy_m":8,"name":"","address":"","thumbnail_path":null,"live":true,"updated_at":42}"#,
        )
        .unwrap();
        assert_eq!(
            media,
            MessageMedia::Location {
                latitude_e7: 523_701_600,
                longitude_e7: 48_953_000,
                accuracy_m: 8,
                name: String::new(),
                address: String::new(),
                thumbnail_path: None,
                live: true,
                updated_at: 42,
                duration_seconds: 0,
            }
        );
    }
}
