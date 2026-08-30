use omarchy_whatsapp_protocol::Command;
use std::time::Duration;

pub const MAX_CONNECTION_JOBS: usize = 8;
pub const MAX_IPC_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[must_use]
pub fn timeout(command: &Command) -> Duration {
    match command {
        Command::SendVoiceMessage { .. } | Command::CreatePoll { .. } => Duration::from_secs(120),
        Command::DownloadImage { .. }
        | Command::DownloadSticker { .. }
        | Command::DownloadVideo { .. }
        | Command::DownloadAudio { .. }
        | Command::GetGroupParticipants { .. } => Duration::from_secs(60),
        _ => Duration::from_secs(30),
    }
}

#[must_use]
pub fn conflict_key(command: &Command) -> Option<String> {
    let key = match command {
        Command::SendMessage { chat_jid, .. }
        | Command::CreatePoll { chat_jid, .. }
        | Command::VotePoll { chat_jid, .. }
        | Command::DownloadImage { chat_jid, .. }
        | Command::DownloadSticker { chat_jid, .. }
        | Command::DownloadVideo { chat_jid, .. }
        | Command::DownloadAudio { chat_jid, .. }
        | Command::React { chat_jid, .. }
        | Command::MarkRead { chat_jid }
        | Command::SetChatPinned { chat_jid, .. }
        | Command::SetChatState { chat_jid, .. } => format!("chat:{chat_jid}"),
        Command::SendVoiceMessage { recording_id, .. }
        | Command::DiscardVoiceRecording { recording_id } => format!("voice:{recording_id}"),
        Command::RetryTextMessage { delivery_id } | Command::DiscardTextMessage { delivery_id } => {
            format!("text:{delivery_id}")
        }
        Command::RequestAvatar { jid } => format!("avatar:{jid}"),
        Command::SetActiveChat { .. } | Command::SetPresence { .. } => "connection-intent".into(),
        Command::ResyncChatState | Command::Logout => "global-session".into(),
        Command::GetState
        | Command::ListChats { .. }
        | Command::GetMessages { .. }
        | Command::GetGroupParticipants { .. }
        | Command::ListVoiceOutbox
        | Command::ListTextOutbox
        | Command::ListAvatars
        | Command::Ping => return None,
    };
    Some(key)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn long_network_jobs_receive_larger_deadlines() {
        assert_eq!(
            timeout(&Command::SendVoiceMessage {
                chat_jid: "chat".into(),
                recording_id: "voice".into(),
            }),
            Duration::from_secs(120)
        );
        assert_eq!(
            timeout(&Command::DownloadImage {
                chat_jid: "chat".into(),
                message_id: "message".into(),
            }),
            Duration::from_secs(60)
        );
        assert_eq!(timeout(&Command::Ping), Duration::from_secs(30));
        for command in [
            Command::DownloadSticker {
                chat_jid: "chat".into(),
                message_id: "m".into(),
            },
            Command::DownloadVideo {
                chat_jid: "chat".into(),
                message_id: "m".into(),
            },
            Command::DownloadAudio {
                chat_jid: "chat".into(),
                message_id: "m".into(),
            },
            Command::GetGroupParticipants {
                chat_jid: "chat".into(),
            },
        ] {
            assert_eq!(timeout(&command), Duration::from_secs(60));
        }
        assert_eq!(
            timeout(&Command::CreatePoll {
                chat_jid: "chat".into(),
                question: "q".into(),
                options: vec!["a".into(), "b".into()],
                selectable_count: 1,
                correct_option_index: None,
            }),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn conflicting_mutations_share_a_resource_key() {
        let read = Command::MarkRead {
            chat_jid: "chat".into(),
        };
        let react = Command::React {
            chat_jid: "chat".into(),
            message_id: "message".into(),
            sender_jid: "sender".into(),
            target_from_me: false,
            emoji: "👍".into(),
        };
        assert_eq!(conflict_key(&read), conflict_key(&react));
        assert_eq!(conflict_key(&Command::GetState), None);
        assert_eq!(
            conflict_key(&Command::SetPresence { available: true }).as_deref(),
            Some("connection-intent")
        );
        assert_eq!(
            conflict_key(&Command::Logout).as_deref(),
            Some("global-session")
        );
        let keyed = [
            (
                Command::SendMessage {
                    chat_jid: "chat".into(),
                    text: "x".into(),
                    delivery_id: "d".into(),
                },
                "chat:chat",
            ),
            (
                Command::CreatePoll {
                    chat_jid: "chat".into(),
                    question: "q".into(),
                    options: vec![],
                    selectable_count: 1,
                    correct_option_index: None,
                },
                "chat:chat",
            ),
            (
                Command::VotePoll {
                    chat_jid: "chat".into(),
                    message_id: "m".into(),
                    selected_options: vec![],
                },
                "chat:chat",
            ),
            (
                Command::DownloadImage {
                    chat_jid: "chat".into(),
                    message_id: "m".into(),
                },
                "chat:chat",
            ),
            (
                Command::DownloadSticker {
                    chat_jid: "chat".into(),
                    message_id: "m".into(),
                },
                "chat:chat",
            ),
            (
                Command::DownloadVideo {
                    chat_jid: "chat".into(),
                    message_id: "m".into(),
                },
                "chat:chat",
            ),
            (
                Command::DownloadAudio {
                    chat_jid: "chat".into(),
                    message_id: "m".into(),
                },
                "chat:chat",
            ),
            (
                Command::SetChatPinned {
                    chat_jid: "chat".into(),
                    pinned: true,
                },
                "chat:chat",
            ),
            (
                Command::SetChatState {
                    chat_jid: "chat".into(),
                    state: omarchy_whatsapp_protocol::ChatState::Typing,
                },
                "chat:chat",
            ),
            (
                Command::SendVoiceMessage {
                    chat_jid: "chat".into(),
                    recording_id: "voice".into(),
                },
                "voice:voice",
            ),
            (
                Command::DiscardVoiceRecording {
                    recording_id: "voice".into(),
                },
                "voice:voice",
            ),
            (
                Command::RetryTextMessage {
                    delivery_id: "delivery".into(),
                },
                "text:delivery",
            ),
            (
                Command::DiscardTextMessage {
                    delivery_id: "delivery".into(),
                },
                "text:delivery",
            ),
            (
                Command::RequestAvatar {
                    jid: "person".into(),
                },
                "avatar:person",
            ),
            (
                Command::SetActiveChat {
                    chat_jid: Some("chat".into()),
                },
                "connection-intent",
            ),
            (Command::ResyncChatState, "global-session"),
        ];
        for (command, expected) in keyed {
            assert_eq!(conflict_key(&command).as_deref(), Some(expected));
        }
        for command in [
            Command::ListChats { limit: 1 },
            Command::GetMessages {
                chat_jid: "chat".into(),
                limit: 1,
            },
            Command::GetGroupParticipants {
                chat_jid: "chat".into(),
            },
            Command::ListVoiceOutbox,
            Command::ListTextOutbox,
            Command::ListAvatars,
            Command::Ping,
        ] {
            assert_eq!(conflict_key(&command), None);
        }
    }
}
