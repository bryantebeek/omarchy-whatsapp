// Durable outbox delivery loops for queued text messages and read receipts.

use crate::database;
use crate::identity::canonical_contact_jid;
use crate::state::{Shared, broadcast_chats, broadcast_text_outbox};
use crate::transport::Transport;
use anyhow::{Context, Result};
use chrono::Utc;
use omarchy_whatsapp_protocol::{Message, ServerEvent, TextOutboxStatus};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;
use whatsapp_rust::SendOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

async fn deliver_pending_text(
    shared: &Arc<Shared>,
    transport: &Arc<dyn Transport>,
) -> Result<bool> {
    // The claim is the atomic transition, so the gate only has to cover it and
    // the resulting snapshot. Holding it across the send would make every
    // enqueue wait for the network and time the shell's request out.
    let pending = {
        let _outbox_guard = shared.text_outbox_gate.lock().await;
        let Some(pending) = shared.database.claim_text_message()? else {
            return Ok(false);
        };
        broadcast_text_outbox(shared);
        pending
    };
    let delivery_id = pending.delivery_id.clone();
    match send_claimed_text(shared, transport, pending).await {
        Ok(message) => {
            shared.database.complete_text_message(&delivery_id)?;
            shared.publish(ServerEvent::Sent {
                message: message.clone(),
            });
            shared.publish(ServerEvent::TextDelivery {
                delivery_id,
                status: TextOutboxStatus::Sent,
                error: None,
            });
            broadcast_text_outbox(shared);
            broadcast_chats(shared);
        }
        Err(error) => {
            let detail = error.to_string();
            shared.database.fail_text_message(&delivery_id, &detail)?;
            shared.publish(ServerEvent::TextDelivery {
                delivery_id,
                status: TextOutboxStatus::Failed,
                error: Some(detail),
            });
            broadcast_text_outbox(shared);
        }
    }
    Ok(true)
}

async fn send_claimed_text(
    shared: &Arc<Shared>,
    transport: &Arc<dyn Transport>,
    pending: database::PendingTextMessage,
) -> Result<Message> {
    let requested: Jid = pending
        .chat_jid
        .parse()
        .context("invalid queued text chat JID")?;
    let canonical = canonical_contact_jid(shared, transport.as_ref(), &requested).await;
    let jid: Jid = canonical
        .parse()
        .context("invalid canonical text chat JID")?;
    let result = transport
        .send_message(
            &jid,
            wa::Message::text(pending.text.clone()),
            SendOptions::default().with_message_id(pending.message_id),
        )
        .await?;
    let message = Message {
        id: result.message_id,
        chat_jid: jid.to_non_ad_string(),
        sender_jid: "me".into(),
        sender_name: "You".into(),
        text: pending.text,
        timestamp: Utc::now().timestamp(),
        from_me: true,
        receipt: 1,
        delivered_at: None,
        read_at: None,
        delivered_to: Vec::new(),
        read_by: Vec::new(),
        media: None,
        reactions: Vec::new(),
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
    Ok(message)
}

pub(crate) async fn run_text_outbox(shared: Arc<Shared>) {
    loop {
        while let Some(transport) = shared.client.read().await.clone() {
            match deliver_pending_text(&shared, &transport).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    warn!(%error, "could not process durable text outbox");
                    break;
                }
            }
        }
        shared.text_outbox_notify.notified().await;
    }
}

async fn deliver_pending_read(
    shared: &Arc<Shared>,
    transport: &Arc<dyn Transport>,
) -> Result<bool> {
    // `MarkRead` runs on every chat selection and focus change, so the gate is
    // only held while the batch is selected. `finish_read_batch` deletes just
    // the receipts in this batch, leaving anything queued during the send for
    // the next iteration.
    let batch = {
        let _outbox_guard = shared.read_outbox_gate.lock().await;
        let Some(batch) = shared.database.next_read_batch()? else {
            return Ok(false);
        };
        batch
    };
    let chat: Jid = batch
        .chat_jid
        .parse()
        .context("invalid queued read chat JID")?;
    let is_group = batch
        .receipts
        .first()
        .is_some_and(|receipt| receipt.is_group);
    if is_group {
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for receipt in &batch.receipts {
            grouped
                .entry(receipt.sender_jid.clone())
                .or_default()
                .push(receipt.message_id.clone());
        }
        for (sender, ids) in grouped {
            let sender: Jid = sender.parse().context("invalid queued read sender JID")?;
            let refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
            transport.mark_as_read(&chat, Some(&sender), &refs).await?;
        }
    } else {
        let ids = batch
            .receipts
            .iter()
            .map(|receipt| receipt.message_id.as_str())
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            transport.mark_as_read(&chat, None, &ids).await?;
        }
    }
    transport.mark_chat_as_read(&chat).await?;
    shared.database.finish_read_batch(&batch)?;
    Ok(true)
}

pub(crate) async fn run_read_outbox(shared: Arc<Shared>) {
    loop {
        while let Some(transport) = shared.client.read().await.clone() {
            match deliver_pending_read(&shared, &transport).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    warn!(%error, "could not deliver durable read intent");
                    break;
                }
            }
        }
        shared.read_outbox_notify.notified().await;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::database::UnreadReceipt;
    use crate::test_support::{test_shared, unread_message};
    use crate::transport::fake::{Call, CallKind, FakeTransport};
    use crate::transport::{ContactInfo, MediaSource, SendReceipt};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use whatsapp_rust::wacore::types::lid_pn::{LearningSource, LidPnEntry};
    use whatsapp_rust::{GroupMetadata, PollVoteCiphertext, ProfilePicture, media};

    /// A [`Transport`] that samples the outbox gates while the daemon is making
    /// its network call. The gates exist to make the claim atomic; holding one
    /// across the send would make every enqueue wait for `WhatsApp`, so the probe
    /// records that the relevant gate was free by the time the call was issued.
    struct GateProbe {
        inner: Arc<FakeTransport>,
        shared: Arc<Shared>,
        text_gate_free_during_send: StdMutex<Vec<bool>>,
        read_gate_free_during_receipt: StdMutex<Vec<bool>>,
    }

    impl GateProbe {
        fn new(inner: &Arc<FakeTransport>, shared: &Arc<Shared>) -> Self {
            Self {
                inner: Arc::clone(inner),
                shared: Arc::clone(shared),
                text_gate_free_during_send: StdMutex::new(Vec::new()),
                read_gate_free_during_receipt: StdMutex::new(Vec::new()),
            }
        }

        fn samples(field: &StdMutex<Vec<bool>>) -> Vec<bool> {
            field.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Transport for GateProbe {
        async fn lid_pn_entry(&self, jid: &Jid) -> Result<Option<LidPnEntry>> {
            self.inner.lid_pn_entry(jid).await
        }

        async fn send_message(
            &self,
            chat: &Jid,
            message: wa::Message,
            options: SendOptions,
        ) -> Result<SendReceipt> {
            self.text_gate_free_during_send
                .lock()
                .unwrap()
                .push(self.shared.text_outbox_gate.try_lock().is_ok());
            self.inner.send_message(chat, message, options).await
        }

        async fn mark_as_read(
            &self,
            chat: &Jid,
            sender: Option<&Jid>,
            message_ids: &[&str],
        ) -> Result<()> {
            self.read_gate_free_during_receipt
                .lock()
                .unwrap()
                .push(self.shared.read_outbox_gate.try_lock().is_ok());
            self.inner.mark_as_read(chat, sender, message_ids).await
        }

        async fn mark_chat_as_read(&self, chat: &Jid) -> Result<()> {
            self.inner.mark_chat_as_read(chat).await
        }

        fn pn(&self) -> Option<Jid> {
            unimplemented!("the outbox never asks for the own phone number")
        }
        fn lid(&self) -> Option<Jid> {
            unimplemented!("the outbox never asks for the own LID")
        }
        fn push_name(&self) -> String {
            unimplemented!("the outbox never reads the push name")
        }
        fn generate_message_id(&self) -> String {
            unimplemented!("queued messages carry their own ID")
        }
        async fn group_metadata(&self, _jid: &Jid) -> Result<GroupMetadata> {
            unimplemented!("the outbox never reads group metadata")
        }
        async fn participating_groups(&self) -> Result<HashMap<Jid, GroupMetadata>> {
            unimplemented!("the outbox never enumerates groups")
        }
        async fn user_info(&self, _jids: &[Jid]) -> Result<HashMap<Jid, ContactInfo>> {
            unimplemented!("the outbox never reads business profiles")
        }
        async fn profile_picture(&self, _jid: &Jid) -> Result<Option<ProfilePicture>> {
            unimplemented!("the outbox never fetches avatars")
        }
        async fn upload_audio_message(
            &self,
            _data: Vec<u8>,
            _options: media::AudioOptions,
        ) -> Result<wa::Message> {
            unimplemented!("voice notes have their own outbox")
        }
        async fn upload_image_message(
            &self,
            _data: Vec<u8>,
            _options: media::ImageOptions,
        ) -> Result<wa::Message> {
            unimplemented!("pasted images send without an outbox")
        }
        async fn send_reaction(
            &self,
            _chat: &Jid,
            _target_key: wa::MessageKey,
            _emoji: &str,
        ) -> Result<SendReceipt> {
            unimplemented!("reactions are not queued in the text outbox")
        }
        async fn pin_chat(&self, _chat: &Jid) -> Result<()> {
            unimplemented!("pinning is a command, not outbox work")
        }
        async fn unpin_chat(&self, _chat: &Jid) -> Result<()> {
            unimplemented!("pinning is a command, not outbox work")
        }
        async fn set_available(&self) -> Result<()> {
            unimplemented!("presence is reconciled elsewhere")
        }
        async fn set_unavailable(&self) -> Result<()> {
            unimplemented!("presence is reconciled elsewhere")
        }
        async fn subscribe_presence(&self, _jid: Jid) -> Result<()> {
            unimplemented!("presence is reconciled elsewhere")
        }
        async fn unsubscribe_presence(&self, _jid: &Jid) -> Result<()> {
            unimplemented!("presence is reconciled elsewhere")
        }
        async fn send_composing(&self, _jid: &Jid) -> Result<()> {
            unimplemented!("chat state is a command, not outbox work")
        }
        async fn send_recording(&self, _jid: &Jid) -> Result<()> {
            unimplemented!("chat state is a command, not outbox work")
        }
        async fn send_paused(&self, _jid: &Jid) -> Result<()> {
            unimplemented!("chat state is a command, not outbox work")
        }
        async fn create_poll(
            &self,
            _chat: Jid,
            _question: &str,
            _options: &[String],
            _selectable_count: u32,
        ) -> Result<(SendReceipt, Vec<u8>)> {
            unimplemented!("polls are not queued in the text outbox")
        }
        async fn create_quiz(
            &self,
            _chat: Jid,
            _question: &str,
            _options: &[String],
            _correct_index: usize,
        ) -> Result<(SendReceipt, Vec<u8>)> {
            unimplemented!("polls are not queued in the text outbox")
        }
        async fn vote_poll(
            &self,
            _chat: Jid,
            _poll_message_id: &str,
            _poll_creator_jid: &Jid,
            _message_secret: &[u8],
            _option_names: &[String],
        ) -> Result<SendReceipt> {
            unimplemented!("polls are not queued in the text outbox")
        }
        async fn decrypt_poll_vote(
            &self,
            _ciphertext: PollVoteCiphertext<'_>,
            _message_secret: &[u8],
            _poll_message_id: &str,
            _poll_creator_jid: &Jid,
            _voter_jid: &Jid,
        ) -> Result<Vec<Vec<u8>>> {
            unimplemented!("polls are not queued in the text outbox")
        }
        async fn fetch_message_history(
            &self,
            _chat: &Jid,
            _oldest_message_id: &str,
            _oldest_message_from_me: bool,
            _oldest_message_timestamp_ms: i64,
            _count: i32,
        ) -> Result<String> {
            unimplemented!("history recovery is a sync pass")
        }
        async fn request_placeholder_resend(&self, _info: &Arc<MessageInfo>) -> Result<()> {
            unimplemented!("placeholder resends belong to the message reducer")
        }
        async fn download(
            &self,
            _source: &MediaSource,
            _file: std::fs::File,
        ) -> Result<std::fs::File> {
            unimplemented!("the outbox never downloads media")
        }
        async fn logout(&self) {
            unimplemented!("logout is a command, not outbox work")
        }
    }

    fn receipt(message_id: &str, sender_jid: &str, is_group: bool) -> UnreadReceipt {
        UnreadReceipt {
            message_id: message_id.to_owned(),
            sender_jid: sender_jid.to_owned(),
            is_group,
        }
    }

    fn scripted(fake: &Arc<FakeTransport>) -> Arc<dyn Transport> {
        crate::transport::fake::transport(fake)
    }

    /// Runs the spawned outbox loops far enough to drain everything that is
    /// ready; the loops only suspend on their `Notify`.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn an_empty_outbox_asks_whatsapp_for_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let fake = Arc::new(FakeTransport::new());
        let transport = scripted(&fake);

        assert!(!deliver_pending_text(&shared, &transport).await.unwrap());
        assert!(!deliver_pending_read(&shared, &transport).await.unwrap());

        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_delivered_text_completes_its_row_and_publishes_the_stored_message() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "31600000000@s.whatsapp.net";
        shared
            .database
            .enqueue_text_message("delivery-1", chat, "hello", "MSG-1", 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new());
        let transport = scripted(&fake);
        let mut events = shared.events.subscribe();

        assert!(deliver_pending_text(&shared, &transport).await.unwrap());

        let sent = fake.calls_of(CallKind::SendMessage);
        assert_eq!(sent.len(), 1);
        assert!(matches!(
            &sent[0],
            Call::SendMessage { chat: recipient, message_id, .. }
                if recipient == chat && message_id.as_deref() == Some("MSG-1")
        ));
        assert!(shared.database.text_outbox().unwrap().is_empty());
        let stored = shared.database.messages(chat, 10).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, "MSG-1");
        assert_eq!(stored[0].text, "hello");
        assert!(stored[0].from_me);
        assert_eq!(stored[0].sender_name, "You");

        let published = std::iter::from_fn(|| events.try_recv().ok())
            .map(|frame| frame.event)
            .collect::<Vec<_>>();
        assert!(published.iter().any(|event| matches!(
            event,
            ServerEvent::Sent { message } if message.id == "MSG-1"
        )));
        assert!(published.iter().any(|event| matches!(
            event,
            ServerEvent::TextDelivery { delivery_id, status: TextOutboxStatus::Sent, error: None }
                if delivery_id == "delivery-1"
        )));
        assert!(
            published
                .iter()
                .any(|event| matches!(event, ServerEvent::TextOutbox { .. }))
        );
    }

    #[tokio::test]
    async fn a_lid_chat_is_canonicalized_and_named_before_the_message_is_stored() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let alias = "100000012345678@lid";
        let canonical = "31612345678@s.whatsapp.net";
        shared
            .database
            .enqueue_text_message("delivery-1", alias, "hello", "MSG-1", 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new().with_lid_pn_entry(
            alias,
            Some(LidPnEntry {
                lid: "100000012345678".into(),
                phone_number: "31612345678".into(),
                created_at: 1,
                learning_source: LearningSource::Usync,
            }),
        ));
        let transport = scripted(&fake);

        assert!(deliver_pending_text(&shared, &transport).await.unwrap());

        assert_eq!(
            fake.calls_of(CallKind::SendMessage),
            vec![Call::SendMessage {
                chat: canonical.into(),
                message: Box::new(wa::Message::text("hello")),
                message_id: Some("MSG-1".into()),
            }]
        );
        assert_eq!(shared.database.messages(canonical, 10).unwrap().len(), 1);

        // A known chat name labels the stored message instead of the raw JID.
        shared
            .database
            .insert_message(&unread_message("SEED"), "1@s.whatsapp.net", false, false)
            .unwrap();
        shared
            .database
            .update_address_book_name("1@s.whatsapp.net", "Ada")
            .unwrap();
        shared
            .database
            .enqueue_text_message("delivery-2", "1@s.whatsapp.net", "again", "MSG-2", 11)
            .unwrap();
        assert!(deliver_pending_text(&shared, &transport).await.unwrap());
        assert_eq!(
            shared
                .database
                .chat_name("1@s.whatsapp.net")
                .unwrap()
                .as_deref(),
            Some("Ada")
        );
    }

    #[tokio::test]
    async fn a_contact_name_labels_a_chat_that_has_no_name_of_its_own() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "31600000000@s.whatsapp.net";
        shared
            .database
            .update_address_book_name(chat, "Ada")
            .unwrap();
        shared
            .database
            .enqueue_text_message("delivery-1", chat, "hello", "MSG-1", 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new());
        // No chat row exists yet, so only the address book can name this chat.
        assert!(shared.database.chat_name(chat).unwrap().is_none());
        assert_eq!(
            shared.database.contact_name(chat).unwrap().as_deref(),
            Some("Ada")
        );

        assert!(
            deliver_pending_text(&shared, &scripted(&fake))
                .await
                .unwrap()
        );

        let chats = shared.database.list_chats(10).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].name, "Ada");
        assert_eq!(shared.database.messages(chat, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_rejected_send_fails_the_row_and_publishes_the_reason() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        shared
            .database
            .enqueue_text_message(
                "delivery-1",
                "31600000000@s.whatsapp.net",
                "hello",
                "MSG-1",
                10,
            )
            .unwrap();
        let fake =
            Arc::new(FakeTransport::new().failing(CallKind::SendMessage, "WhatsApp is down"));
        let mut events = shared.events.subscribe();

        assert!(
            deliver_pending_text(&shared, &scripted(&fake))
                .await
                .unwrap()
        );

        let entries = shared.database.text_outbox().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, TextOutboxStatus::Failed);
        assert_eq!(entries[0].error.as_deref(), Some("WhatsApp is down"));
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(|frame| matches!(
                frame.event,
                ServerEvent::TextDelivery {
                    status: TextOutboxStatus::Failed,
                    ref error,
                    ..
                } if error.as_deref() == Some("WhatsApp is down")
            ))
        );
    }

    #[tokio::test]
    async fn an_unparseable_queued_chat_never_reaches_whatsapp() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        shared
            .database
            .enqueue_text_message("delivery-1", "not a jid", "hello", "MSG-1", 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new());

        assert!(
            deliver_pending_text(&shared, &scripted(&fake))
                .await
                .unwrap()
        );

        assert!(fake.calls_of(CallKind::SendMessage).is_empty());
        let entries = shared.database.text_outbox().unwrap();
        assert_eq!(entries[0].status, TextOutboxStatus::Failed);
        assert_eq!(
            entries[0].error.as_deref(),
            Some("invalid queued text chat JID")
        );
    }

    #[tokio::test]
    async fn a_group_read_batch_is_grouped_per_sender() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "123-456@g.us";
        shared
            .database
            .queue_read_receipts(
                chat,
                &[
                    receipt("m1", "31600000001@s.whatsapp.net", true),
                    receipt("m2", "31600000002@s.whatsapp.net", true),
                    receipt("m3", "31600000001@s.whatsapp.net", true),
                ],
                10,
            )
            .unwrap();
        let fake = Arc::new(FakeTransport::new());

        assert!(
            deliver_pending_read(&shared, &scripted(&fake))
                .await
                .unwrap()
        );

        let mut grouped = fake
            .calls_of(CallKind::MarkAsRead)
            .into_iter()
            .map(|call| match call {
                Call::MarkAsRead {
                    sender,
                    message_ids,
                    ..
                } => (sender.unwrap_or_default(), message_ids),
                other => panic!("unexpected call {other:?}"),
            })
            .collect::<Vec<_>>();
        grouped.sort();
        assert_eq!(
            grouped,
            vec![
                (
                    "31600000001@s.whatsapp.net".to_owned(),
                    vec!["m1".to_owned(), "m3".to_owned()]
                ),
                (
                    "31600000002@s.whatsapp.net".to_owned(),
                    vec!["m2".to_owned()]
                ),
            ]
        );
        assert_eq!(
            fake.calls_of(CallKind::MarkChatAsRead),
            vec![Call::MarkChatAsRead(chat.into())]
        );
        assert!(shared.database.next_read_batch().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_direct_read_batch_sends_one_receipt_without_a_sender() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "31600000000@s.whatsapp.net";
        shared
            .database
            .queue_read_receipts(chat, &[receipt("m1", chat, false)], 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new());

        assert!(
            deliver_pending_read(&shared, &scripted(&fake))
                .await
                .unwrap()
        );

        assert_eq!(
            fake.calls_of(CallKind::MarkAsRead),
            vec![Call::MarkAsRead {
                chat: chat.into(),
                sender: None,
                message_ids: vec!["m1".into()],
            }]
        );
        assert!(shared.database.next_read_batch().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_receiptless_chat_is_still_marked_read_across_devices() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "31600000000@s.whatsapp.net";
        shared.database.queue_read_receipts(chat, &[], 10).unwrap();
        let fake = Arc::new(FakeTransport::new());

        assert!(
            deliver_pending_read(&shared, &scripted(&fake))
                .await
                .unwrap()
        );

        assert!(fake.calls_of(CallKind::MarkAsRead).is_empty());
        assert_eq!(
            fake.calls_of(CallKind::MarkChatAsRead),
            vec![Call::MarkChatAsRead(chat.into())]
        );
        assert!(shared.database.next_read_batch().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_failed_receipt_leaves_the_batch_for_the_next_iteration() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "31600000000@s.whatsapp.net";
        shared
            .database
            .queue_read_receipts(chat, &[receipt("m1", chat, false)], 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new().failing(CallKind::MarkAsRead, "offline"));
        let transport = scripted(&fake);

        assert_eq!(
            deliver_pending_read(&shared, &transport)
                .await
                .unwrap_err()
                .to_string(),
            "offline"
        );
        assert!(shared.database.next_read_batch().unwrap().is_some());

        // The cross-device action can fail on its own after the receipts land.
        fake.succeed(CallKind::MarkAsRead);
        fake.fail(CallKind::MarkChatAsRead, "app state is unavailable");
        assert_eq!(
            deliver_pending_read(&shared, &transport)
                .await
                .unwrap_err()
                .to_string(),
            "app state is unavailable"
        );
        assert!(shared.database.next_read_batch().unwrap().is_some());

        fake.succeed(CallKind::MarkChatAsRead);
        assert!(deliver_pending_read(&shared, &transport).await.unwrap());
        assert!(shared.database.next_read_batch().unwrap().is_none());
    }

    #[tokio::test]
    async fn unparseable_queued_read_identities_are_reported_not_sent() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        shared
            .database
            .queue_read_receipts("not a jid", &[receipt("m1", "not a jid", false)], 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new());
        let transport = scripted(&fake);

        assert_eq!(
            deliver_pending_read(&shared, &transport)
                .await
                .unwrap_err()
                .to_string(),
            "invalid queued read chat JID"
        );

        let group_directory = tempfile::tempdir().unwrap();
        let group_shared = Arc::new(test_shared(&group_directory));
        group_shared
            .database
            .queue_read_receipts("123-456@g.us", &[receipt("m1", "not a jid", true)], 10)
            .unwrap();
        assert_eq!(
            deliver_pending_read(&group_shared, &transport)
                .await
                .unwrap_err()
                .to_string(),
            "invalid queued read sender JID"
        );
        assert!(fake.calls_of(CallKind::MarkAsRead).is_empty());
    }

    #[tokio::test]
    async fn the_outbox_gates_cover_the_claim_but_not_the_network_call() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "31600000000@s.whatsapp.net";
        shared
            .database
            .enqueue_text_message("delivery-1", chat, "hello", "MSG-1", 10)
            .unwrap();
        shared
            .database
            .queue_read_receipts(chat, &[receipt("m1", chat, false)], 10)
            .unwrap();
        let fake = Arc::new(FakeTransport::new());
        let probe = Arc::new(GateProbe::new(&fake, &shared));
        let transport: Arc<dyn Transport> = Arc::clone(&probe) as Arc<dyn Transport>;

        let text_guard = shared.text_outbox_gate.lock().await;
        let text_shared = Arc::clone(&shared);
        let text_transport = Arc::clone(&transport);
        let text_delivery =
            tokio::spawn(async move { deliver_pending_text(&text_shared, &text_transport).await });
        settle().await;
        assert!(
            fake.calls().is_empty(),
            "the claim must not run while the gate is held"
        );
        drop(text_guard);
        assert!(text_delivery.await.unwrap().unwrap());

        let read_guard = shared.read_outbox_gate.lock().await;
        let read_shared = Arc::clone(&shared);
        let read_transport = Arc::clone(&transport);
        let read_delivery =
            tokio::spawn(async move { deliver_pending_read(&read_shared, &read_transport).await });
        settle().await;
        assert!(fake.calls_of(CallKind::MarkAsRead).is_empty());
        drop(read_guard);
        assert!(read_delivery.await.unwrap().unwrap());

        assert_eq!(
            GateProbe::samples(&probe.text_gate_free_during_send),
            vec![true],
            "the text gate must be released before the send"
        );
        assert_eq!(
            GateProbe::samples(&probe.read_gate_free_during_receipt),
            vec![true],
            "the read gate must be released before the receipt"
        );
    }

    #[tokio::test]
    async fn the_outbox_loops_drain_on_notification_and_survive_database_failures() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let chat = "31600000000@s.whatsapp.net";
        let fake = Arc::new(FakeTransport::new());
        let text_loop = tokio::spawn(run_text_outbox(Arc::clone(&shared)));
        let read_loop = tokio::spawn(run_read_outbox(Arc::clone(&shared)));

        // Without a client both loops park on their notification.
        settle().await;
        assert!(fake.calls().is_empty());

        *shared.client.write().await = Some(scripted(&fake));
        shared
            .database
            .enqueue_text_message("delivery-1", chat, "hello", "MSG-1", 10)
            .unwrap();
        shared
            .database
            .queue_read_receipts(chat, &[receipt("m1", chat, false)], 10)
            .unwrap();
        shared.text_outbox_notify.notify_one();
        shared.read_outbox_notify.notify_one();
        settle().await;

        assert_eq!(fake.calls_of(CallKind::SendMessage).len(), 1);
        assert_eq!(fake.calls_of(CallKind::MarkChatAsRead).len(), 1);
        assert!(shared.database.text_outbox().unwrap().is_empty());

        // A broken database stops the current drain instead of spinning.
        shared
            .database
            .execute_test_sql("DROP TABLE text_outbox; DROP TABLE pending_read_chats;")
            .unwrap();
        fake.clear_calls();
        shared.text_outbox_notify.notify_one();
        shared.read_outbox_notify.notify_one();
        settle().await;
        assert!(fake.calls().is_empty());

        text_loop.abort();
        read_loop.abort();
    }
}
