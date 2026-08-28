use omarchy_whatsapp_protocol::Message;
use std::process::{Command, Stdio};

pub fn send(message: &Message, chat_name: &str, is_group: bool) {
    let body = message_body(message, is_group);

    let omarchy_helper = [
        "/usr/bin/omarchy-notification-send",
        "/usr/share/omarchy/bin/omarchy-notification-send",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file());

    let mut command = if let Some(helper) = omarchy_helper {
        let mut command = Command::new(helper);
        command.args([
            "--app-name",
            "WhatsApp",
            "--glyph",
            "󰖣",
            "--urgency",
            "normal",
            chat_name,
            &body,
            "--exec",
            "omarchy",
            "shell",
            "-q",
            "io.github.bryantebeek.whatsapp",
            "openChat",
            &message.chat_jid,
        ]);
        command
    } else {
        let mut command = Command::new("notify-send");
        command.args(["--app-name=WhatsApp", chat_name, &body]);
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    std::thread::spawn(move || {
        if let Err(error) = command.status() {
            tracing::warn!(%error, "could not launch notification helper");
        }
    });
}

fn message_body(message: &Message, is_group: bool) -> String {
    let mut body = message.text.replace(['\r', '\n'], " ");
    if body.chars().count() > 240 {
        body = body.chars().take(239).collect::<String>() + "…";
    }
    if is_group {
        body = format!("{}: {}", message.sender_name, body);
    } else if body.starts_with('-') {
        // Keep a direct message such as "--exec" in the helper's positional
        // body slot without adding visible text to the notification.
        body.insert(0, '\u{2060}');
    }
    body
}

pub fn send_event(title: &str, body: &str, urgency: &str) {
    let body = body.replace(['\r', '\n'], " ");
    let omarchy_helper = [
        "/usr/bin/omarchy-notification-send",
        "/usr/share/omarchy/bin/omarchy-notification-send",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file());

    let mut command = if let Some(helper) = omarchy_helper {
        let mut command = Command::new(helper);
        command.args([
            "--app-name",
            "WhatsApp",
            "--glyph",
            "󰖣",
            "--urgency",
            urgency,
            title,
            &body,
        ]);
        command
    } else {
        let mut command = Command::new("notify-send");
        command.args(["--app-name=WhatsApp", title, &body]);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    std::thread::spawn(move || {
        if let Err(error) = command.status() {
            tracing::warn!(%error, "could not launch notification helper");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(chat_jid: &str, sender_jid: &str, text: &str) -> Message {
        Message {
            id: "message-1".into(),
            chat_jid: chat_jid.into(),
            sender_jid: sender_jid.into(),
            sender_name: "Joyce Bakker".into(),
            text: text.into(),
            timestamp: 0,
            from_me: false,
            media: None,
            reactions: Vec::new(),
        }
    }

    #[test]
    fn direct_message_body_does_not_repeat_sender() {
        let message = message("joyce@s.whatsapp.net", "joyce@s.whatsapp.net", "Hello");

        assert_eq!(message_body(&message, false), "Hello");
    }

    #[test]
    fn group_message_body_includes_sender() {
        let message = message("friends@g.us", "joyce@s.whatsapp.net", "Hello");

        assert_eq!(message_body(&message, true), "Joyce Bakker: Hello");
    }

    #[test]
    fn dash_leading_direct_message_stays_positional() {
        let message = message("joyce@s.whatsapp.net", "joyce@s.whatsapp.net", "--exec");

        assert_eq!(message_body(&message, false), "\u{2060}--exec");
    }
}
