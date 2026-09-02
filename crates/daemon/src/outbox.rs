// Durable outbox delivery loops for queued text messages and read receipts.

use crate::database;
use crate::identity::canonical_contact_jid;
use crate::state::{Shared, broadcast_chats, broadcast_text_outbox};
use anyhow::{Context, Result};
use chrono::Utc;
use omarchy_whatsapp_protocol::{Message, ServerEvent, TextOutboxStatus};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;
use whatsapp_rust::SendOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

#[cfg_attr(coverage_nightly, coverage(off))]
async fn deliver_pending_text(shared: &Arc<Shared>, client: &Arc<Client>) -> Result<bool> {
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
    match send_claimed_text(shared, client, pending).await {
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn send_claimed_text(
    shared: &Arc<Shared>,
    client: &Arc<Client>,
    pending: database::PendingTextMessage,
) -> Result<Message> {
    let requested: Jid = pending
        .chat_jid
        .parse()
        .context("invalid queued text chat JID")?;
    let canonical = canonical_contact_jid(shared, client, &requested).await;
    let jid: Jid = canonical
        .parse()
        .context("invalid canonical text chat JID")?;
    let result = client
        .send_message_with_options(
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

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn run_text_outbox(shared: Arc<Shared>) {
    loop {
        while let Some(client) = shared.client.read().await.clone() {
            match deliver_pending_text(&shared, &client).await {
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn deliver_pending_read(shared: &Arc<Shared>, client: &Arc<Client>) -> Result<bool> {
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
            client.mark_as_read(&chat, Some(&sender), &refs).await?;
        }
    } else {
        let ids = batch
            .receipts
            .iter()
            .map(|receipt| receipt.message_id.as_str())
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            client.mark_as_read(&chat, None, &ids).await?;
        }
    }
    client
        .chat_actions()
        .mark_chat_as_read(&chat, true, None)
        .await?;
    shared.database.finish_read_batch(&batch)?;
    Ok(true)
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn run_read_outbox(shared: Arc<Shared>) {
    loop {
        while let Some(client) = shared.client.read().await.clone() {
            match deliver_pending_read(&shared, &client).await {
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
