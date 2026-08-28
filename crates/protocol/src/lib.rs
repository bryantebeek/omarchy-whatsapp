use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u16 = 8;

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
    pub last_timestamp: i64,
    pub unread: u32,
    pub is_group: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatParticipant {
    pub jid: String,
    pub name: String,
    pub is_me: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MessageMedia>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub from_me: bool,
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
    },
    DownloadImage {
        chat_jid: String,
        message_id: String,
    },
    DownloadVideo {
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
    RequestAvatar {
        jid: String,
    },
    ListAvatars,
    SetActiveChat {
        chat_jid: Option<String>,
    },
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
    Sent {
        message: Message,
    },
    Unread {
        total: u32,
    },
    Avatars {
        revision: u64,
        jids: Vec<String>,
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
    #[serde(flatten)]
    pub event: ServerEvent,
}

impl ServerFrame {
    pub fn response(id: Option<u64>, event: ServerEvent) -> Self {
        Self { id, event }
    }

    pub fn event(event: ServerEvent) -> Self {
        Self { id: None, event }
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
    pub fn discover() -> Self {
        let runtime_base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("omarchy-whatsapp-{}", std::process::id()))
            });
        let state_base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(dirs::state_dir)
            .unwrap_or_else(|| PathBuf::from("."));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trip_is_stable() {
        let frame = ClientFrame::new(
            Some(7),
            Command::SendMessage {
                chat_jid: "31612345678@s.whatsapp.net".into(),
                text: "hello".into(),
            },
        );
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
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
                is_me: false,
            }],
        });
        let response_json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerFrame>(&response_json).unwrap(),
            response
        );
    }

    #[test]
    fn event_round_trip_is_stable() {
        let frame = ServerFrame::event(ServerEvent::Unread { total: 3 });
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
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
    fn rich_message_media_round_trip_is_stable() {
        let message = Message {
            id: "image-1".into(),
            chat_jid: "1@s.whatsapp.net".into(),
            sender_jid: "1@s.whatsapp.net".into(),
            sender_name: "Ada".into(),
            text: "A photo".into(),
            timestamp: 42,
            from_me: false,
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
