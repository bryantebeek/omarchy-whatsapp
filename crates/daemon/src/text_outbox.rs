use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

const MAX_DELIVERY_ID_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 65_536;

pub fn validate_delivery_id(delivery_id: &str) -> Result<()> {
    ensure!(!delivery_id.is_empty(), "delivery ID cannot be empty");
    ensure!(
        delivery_id.len() <= MAX_DELIVERY_ID_BYTES,
        "delivery ID is too large"
    );
    ensure!(
        delivery_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "delivery ID contains unsupported characters"
    );
    Ok(())
}

pub fn validate(delivery_id: &str, text: &str) -> Result<String> {
    validate_delivery_id(delivery_id)?;
    let text = text.trim().to_owned();
    ensure!(!text.is_empty(), "message cannot be empty");
    ensure!(text.len() <= MAX_TEXT_BYTES, "message is too large");
    Ok(text)
}

pub fn validate_mentions(
    chat: &whatsapp_rust::prelude::Jid,
    text: &str,
    mut mentions: Vec<String>,
) -> Result<Vec<String>> {
    use whatsapp_rust::wacore_binary::JidExt;
    ensure!(mentions.len() <= 1024, "too many mentions");
    ensure!(
        mentions.is_empty() || chat.is_group(),
        "mentions require a group chat"
    );
    for jid in &mentions {
        let (user, server) = jid.split_once('@').unwrap_or_default();
        ensure!(
            !user.is_empty()
                && user.bytes().all(|b| b.is_ascii_digit())
                && matches!(server, "s.whatsapp.net" | "lid"),
            "invalid mention JID"
        );
        let token = format!("@{user}");
        ensure!(
            text.match_indices(&token).any(|(index, _)| {
                let before = text[..index].chars().next_back();
                let after = text[index + token.len()..].chars().next();
                before.is_none_or(|c| c.is_whitespace() || "([{".contains(c))
                    && after.is_none_or(|c| !c.is_alphanumeric() && c != '_')
            }),
            "mention is absent from message text"
        );
    }
    mentions.sort();
    mentions.dedup();
    Ok(mentions)
}

pub fn message(text: String, mentions: Vec<String>) -> whatsapp_rust::prelude::wa::Message {
    use whatsapp_rust::prelude::{MessageBuilderExt, wa};
    if mentions.is_empty() {
        return wa::Message::text(text);
    }
    wa::Message {
        extended_text_message: buffa::MessageField::some(wa::message::ExtendedTextMessage {
            text: Some(text),
            context_info: buffa::MessageField::some(wa::ContextInfo {
                mentioned_jid: mentions,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[must_use]
pub fn stable_message_id(delivery_id: &str) -> String {
    let digest = Sha256::digest(delivery_id.as_bytes());
    let mut id = String::with_capacity(24);
    id.push_str("3EB0");
    for byte in &digest[..10] {
        write!(id, "{byte:02X}").expect("writing to a String cannot fail");
    }
    id
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn validation_normalizes_text_and_rejects_unsafe_identity() {
        assert_eq!(validate("client_1.2-a", "  hello  ").unwrap(), "hello");
        for id in ["", "space separated", "💥"] {
            assert!(validate(id, "hello").is_err());
        }
        assert!(validate(&"a".repeat(MAX_DELIVERY_ID_BYTES + 1), "hello").is_err());
        assert!(validate("ok", "  ").is_err());
        assert!(validate("ok", &"a".repeat(MAX_TEXT_BYTES + 1)).is_err());
    }

    #[test]
    fn mentions_validate_and_encode_only_explicit_group_recipients() {
        let group = "123@g.us".parse().unwrap();
        let mentions = vec![
            "200@lid".into(),
            "100@s.whatsapp.net".into(),
            "200@lid".into(),
        ];
        let mentions = validate_mentions(&group, "Hi @100, (@200)!", mentions).unwrap();
        assert_eq!(mentions, ["100@s.whatsapp.net", "200@lid"]);
        let wire = message("Hi @100, (@200)!".into(), mentions.clone());
        let extended = wire.extended_text_message.as_option().unwrap();
        assert_eq!(extended.text.as_deref(), Some("Hi @100, (@200)!"));
        assert_eq!(extended.context_info.mentioned_jid, mentions);
        assert_eq!(
            message("plain".into(), vec![]).conversation.as_deref(),
            Some("plain")
        );
        for jid in ["", "bad", "100@g.us", "100:2@lid", "abc@lid"] {
            assert!(validate_mentions(&group, "@100", vec![jid.into()]).is_err());
        }
        for text in [
            "missing",
            "mail@100.test",
            "https://test/@100",
            "@1000",
            "@100x",
            "@100_",
            "@100é",
        ] {
            assert!(validate_mentions(&group, text, vec!["100@lid".into()]).is_err());
        }
        assert!(validate_mentions(&group, "@100", vec!["100@lid".into(); 1025]).is_err());
        assert!(
            validate_mentions(
                &"100@s.whatsapp.net".parse().unwrap(),
                "@100",
                vec!["100@lid".into()]
            )
            .is_err()
        );
        assert!(validate_mentions(&group, "@100", vec!["100@lid".into()]).is_ok());
    }

    #[test]
    fn stable_message_ids_are_deterministic_distinct_and_whatsapp_shaped() {
        let first = stable_message_id("delivery-1");
        assert_eq!(first, stable_message_id("delivery-1"));
        assert_ne!(first, stable_message_id("delivery-2"));
        assert_eq!(first.len(), 24);
        assert!(first.starts_with("3EB0"));
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
