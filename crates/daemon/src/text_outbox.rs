use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

const MAX_DELIVERY_ID_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 65_536;

pub fn validate(delivery_id: &str, text: &str) -> Result<String> {
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
    let text = text.trim().to_owned();
    ensure!(!text.is_empty(), "message cannot be empty");
    ensure!(text.len() <= MAX_TEXT_BYTES, "message is too large");
    Ok(text)
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
    fn stable_message_ids_are_deterministic_distinct_and_whatsapp_shaped() {
        let first = stable_message_id("delivery-1");
        assert_eq!(first, stable_message_id("delivery-1"));
        assert_ne!(first, stable_message_id("delivery-2"));
        assert_eq!(first.len(), 24);
        assert!(first.starts_with("3EB0"));
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
