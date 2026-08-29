use omarchy_whatsapp_protocol::Message;
use std::process::{Command, Stdio};

pub fn send(message: &Message, chat_name: &str, is_group: bool) {
    let body = message_body(message, is_group);
    let title = positional_text(chat_name);
    launch(message_command(
        available_helper(),
        &title,
        &body,
        &message.chat_jid,
    ));
}

fn available_helper() -> Option<&'static str> {
    [
        "/usr/bin/omarchy-notification-send",
        "/usr/share/omarchy/bin/omarchy-notification-send",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file())
}

fn message_command(helper: Option<&str>, title: &str, body: &str, chat_jid: &str) -> Command {
    if let Some(helper) = helper {
        let mut command = Command::new(helper);
        command.args([
            "--app-name",
            "WhatsApp",
            "--glyph",
            "󰖣",
            "--urgency",
            "normal",
            title,
            body,
            "--exec",
            "omarchy",
            "shell",
            "-q",
            "io.github.bryantebeek.whatsapp",
            "openChat",
            chat_jid,
        ]);
        command
    } else {
        let mut command = Command::new("notify-send");
        command.args(["--app-name=WhatsApp", title, body]);
        command
    }
}

fn launch(mut command: Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(not(test))]
    std::thread::spawn(move || {
        if let Err(error) = command.status() {
            tracing::warn!(%error, "could not launch notification helper");
        }
    });
    #[cfg(test)]
    drop(command);
}

fn message_body(message: &Message, is_group: bool) -> String {
    let mut body = message.text.replace(['\r', '\n'], " ");
    if body.chars().count() > 240 {
        body = body.chars().take(239).collect::<String>() + "…";
    }
    if is_group {
        body = format!("{}: {}", message.sender_name, body);
    }
    positional_text(&body)
}

fn positional_text(value: &str) -> String {
    let mut value = value.replace(['\r', '\n'], " ");
    if value.starts_with('-') {
        // Keep remote text in the helper's positional slots without adding
        // visible content or allowing it to be parsed as an option.
        value.insert(0, '\u{2060}');
    }
    value
}

pub fn send_event(title: &str, body: &str, urgency: &str) {
    let title = positional_text(title);
    let body = positional_text(body);
    launch(event_command(available_helper(), &title, &body, urgency));
}

fn event_command(helper: Option<&str>, title: &str, body: &str, urgency: &str) -> Command {
    if let Some(helper) = helper {
        let mut command = Command::new(helper);
        command.args([
            "--app-name",
            "WhatsApp",
            "--glyph",
            "󰖣",
            "--urgency",
            urgency,
            title,
            body,
        ]);
        command
    } else {
        let mut command = Command::new("notify-send");
        command.args(["--app-name=WhatsApp", title, body]);
        command
    }
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
            receipt: 0,
            delivered_at: None,
            read_at: None,
            read_by: Vec::new(),
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

    #[test]
    fn dash_leading_chat_names_and_event_text_stay_positional() {
        assert_eq!(
            positional_text("--urgency=critical"),
            "\u{2060}--urgency=critical"
        );
        assert_eq!(positional_text("Release\nstatus"), "Release status");

        let message = message("joyce@s.whatsapp.net", "joyce@s.whatsapp.net", "--exec");
        send(&message, "--urgency=critical", false);
        send_event("--glyph", "--exec", "normal");

        for command in [
            message_command(Some("helper"), "title", "body", "chat"),
            message_command(None, "title", "body", "chat"),
            event_command(Some("helper"), "title", "body", "normal"),
            event_command(None, "title", "body", "normal"),
        ] {
            assert!(!command.get_args().collect::<Vec<_>>().is_empty());
        }
    }
}
