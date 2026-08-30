use crate::database::Database;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use buffa::Message as _;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use whatsapp_rust::InboundDurabilityHook;
use whatsapp_rust::prelude::{Client, InboundMessage, MessageContext};
use whatsapp_rust::wacore::types::message::{MessageInfo, MessageSource};
use whatsapp_rust::waproto::whatsapp as wa;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InboundKey {
    pub chat_jid: String,
    pub sender_jid: String,
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableInbound {
    pub key: InboundKey,
    pub message: Vec<u8>,
    pub push_name: String,
    pub timestamp: i64,
    pub media_type: String,
    pub is_from_me: bool,
    pub is_group: bool,
    pub is_offline: bool,
    pub committed_at: i64,
}

impl DurableInbound {
    #[must_use]
    pub fn from_inbound(inbound: &InboundMessage, committed_at: i64) -> Self {
        Self {
            key: InboundKey {
                chat_jid: inbound.info.source.chat.to_string(),
                sender_jid: inbound.info.source.sender.to_string(),
                message_id: inbound.info.id.clone(),
            },
            message: inbound.message.encode_to_vec(),
            push_name: inbound.info.push_name.clone(),
            timestamp: inbound.info.timestamp.timestamp(),
            media_type: inbound.info.media_type.clone(),
            is_from_me: inbound.info.source.is_from_me,
            is_group: inbound.info.source.is_group,
            is_offline: inbound.info.is_offline,
            committed_at,
        }
    }

    fn decode(&self) -> Result<(Arc<wa::Message>, MessageInfo)> {
        let chat = self
            .key
            .chat_jid
            .parse()
            .with_context(|| format!("parsing durable chat JID {}", self.key.chat_jid))?;
        let sender = self
            .key
            .sender_jid
            .parse()
            .with_context(|| format!("parsing durable sender JID {}", self.key.sender_jid))?;
        let timestamp = DateTime::<Utc>::from_timestamp(self.timestamp, 0)
            .ok_or_else(|| anyhow!("invalid durable message timestamp {}", self.timestamp))?;
        let message = Arc::new(
            wa::Message::decode_from_slice(&self.message)
                .context("decoding durable inbound message")?,
        );
        let info = MessageInfo {
            source: MessageSource {
                chat,
                sender,
                is_from_me: self.is_from_me,
                is_group: self.is_group,
                ..MessageSource::default()
            },
            id: self.key.message_id.clone(),
            push_name: self.push_name.clone(),
            timestamp,
            media_type: self.media_type.clone(),
            is_offline: self.is_offline,
            ..MessageInfo::default()
        };
        Ok((message, info))
    }

    pub fn rehydrate_context(&self, client: Arc<Client>) -> Result<MessageContext> {
        let (message, info) = self.decode()?;
        Ok(MessageContext::from_arc(message, &info, client))
    }
}

pub struct DurableInboundHook {
    database: Arc<Database>,
}

impl DurableInboundHook {
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        Self { database }
    }

    async fn commit(&self, batch: &[InboundMessage], committed_at: i64) -> Result<()> {
        let records = batch
            .iter()
            .map(|message| DurableInbound::from_inbound(message, committed_at))
            .collect::<Vec<_>>();
        let database = Arc::clone(&self.database);
        tokio::task::spawn_blocking(move || database.commit_inbound_batch(&records))
            .await
            .context("durable inbound commit task failed")??;
        Ok(())
    }
}

#[async_trait]
impl InboundDurabilityHook for DurableInboundHook {
    async fn on_messages(&self, _client: Arc<Client>, batch: &[InboundMessage]) -> Result<()> {
        self.commit(batch, Utc::now().timestamp()).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;
    use whatsapp_rust::prelude::{Bot, MessageBuilderExt, MessageExt};
    use whatsapp_rust::store::SqliteStore;
    use whatsapp_rust::wacore::types::events::InboundMessage;
    use whatsapp_rust::wacore_binary::jid::Jid;

    async fn synthetic_client(directory: &tempfile::TempDir) -> Arc<Client> {
        let store = SqliteStore::new(
            directory
                .path()
                .join("protocol.db")
                .to_string_lossy()
                .as_ref(),
        )
        .await
        .unwrap();
        Bot::builder()
            .with_backend(store)
            .build()
            .await
            .unwrap()
            .client()
    }

    fn synthetic_inbound() -> InboundMessage {
        InboundMessage::builder()
            .message(Arc::new(wa::Message::text("synthetic")))
            .info(Arc::new(MessageInfo {
                source: MessageSource {
                    chat: "123-456@g.us".parse::<Jid>().unwrap(),
                    sender: "1:2@s.whatsapp.net".parse::<Jid>().unwrap(),
                    is_group: true,
                    ..MessageSource::default()
                },
                id: "same-id".into(),
                push_name: "Synthetic".into(),
                timestamp: Utc.timestamp_opt(42, 0).unwrap(),
                media_type: "text".into(),
                is_offline: true,
                ..MessageInfo::default()
            }))
            .build()
    }

    #[test]
    fn durable_record_preserves_the_full_source_identity_and_payload() {
        let record = DurableInbound::from_inbound(&synthetic_inbound(), 50);
        assert_eq!(record.key.chat_jid, "123-456@g.us");
        assert_eq!(record.key.sender_jid, "1:2@s.whatsapp.net");
        assert_eq!(record.key.message_id, "same-id");
        assert_eq!(record.timestamp, 42);
        assert_eq!(record.committed_at, 50);
        assert!(record.is_group);
        assert!(record.is_offline);
        assert!(!record.message.is_empty());
    }

    #[test]
    fn malformed_durable_records_are_rejected_before_reduction() {
        let mut record = DurableInbound::from_inbound(&synthetic_inbound(), 50);
        record.key.chat_jid = "not a jid".into();
        let error = record.decode().unwrap_err();
        assert!(!error.to_string().is_empty());

        let mut record = DurableInbound::from_inbound(&synthetic_inbound(), 50);
        record.key.sender_jid = "not a jid".into();
        assert!(record.decode().is_err());

        let mut record = DurableInbound::from_inbound(&synthetic_inbound(), 50);
        record.timestamp = i64::MAX;
        assert!(record.decode().is_err());
        let mut record = DurableInbound::from_inbound(&synthetic_inbound(), 50);
        record.message = vec![0xff];
        assert!(record.decode().is_err());

        let record = DurableInbound::from_inbound(&synthetic_inbound(), 50);
        let (decoded, info) = record.decode().unwrap();
        assert_eq!(decoded.text_content(), Some("synthetic"));
        assert_eq!(info.id, "same-id");
    }

    #[test]
    fn hook_construction_keeps_the_shared_database() {
        let directory = tempdir().unwrap();
        let database = Arc::new(Database::open(&directory.path().join("history.db")).unwrap());
        let hook = DurableInboundHook::new(Arc::clone(&database));
        assert!(Arc::ptr_eq(&hook.database, &database));
    }

    #[tokio::test]
    async fn hook_commit_persists_the_complete_batch_before_returning() {
        let directory = tempdir().unwrap();
        let database = Arc::new(Database::open(&directory.path().join("history.db")).unwrap());
        let hook = DurableInboundHook::new(Arc::clone(&database));
        hook.commit(&[synthetic_inbound(), synthetic_inbound()], 70)
            .await
            .unwrap();
        let records = database.pending_inbound(10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].committed_at, 70);
    }

    #[tokio::test]
    async fn durable_record_reconstructs_context_and_trait_hook_commits() {
        let directory = tempdir().unwrap();
        let client = synthetic_client(&directory).await;
        let record = DurableInbound::from_inbound(&synthetic_inbound(), 80);
        let context = record.rehydrate_context(Arc::clone(&client)).unwrap();
        assert_eq!(context.info.id, "same-id");
        assert_eq!(context.message.text_content(), Some("synthetic"));

        let database = Arc::new(Database::open(&directory.path().join("history.db")).unwrap());
        let hook = DurableInboundHook::new(Arc::clone(&database));
        hook.on_messages(client, &[synthetic_inbound()])
            .await
            .unwrap();
        assert_eq!(database.pending_inbound(10).unwrap().len(), 1);
    }
}
