use super::{FastRatchetHandler, SenderKeyMessage};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;
use whatsapp_rust::bot::MessageContext;
use whatsapp_rust::types::enc_handler::EncHandler;
use whatsapp_rust::types::jid::JidExt as SignalJidExt;
use whatsapp_rust::wacore_binary::{Node, builder::NodeBuilder};
use whatsapp_rust::{Client, wacore};

#[async_trait]
impl EncHandler for FastRatchetHandler {
    // This adapter terminates in the upstream client's encrypted-stanza ACK.
    // Cryptographic parsing and state transitions in the parent module are
    // covered separately; deployment smoke tests exercise the live transport.
    async fn handle(
        &self,
        client: Arc<Client>,
        enc_node: &Node,
        info: &wacore::types::message::MessageInfo,
    ) -> Result<()> {
        let enc_node_ref = enc_node.as_node_ref();
        let ciphertext = enc_node_ref
            .content_bytes()
            .ok_or_else(|| anyhow!("fast-ratchet enc node has no byte payload"))?;
        let sender_id = info.source.sender.to_signal_address_string();
        let _guard = self.decrypt_lock.lock().await;
        let message =
            SenderKeyMessage::try_from(ciphertext).context("reading fast-ratchet envelope")?;
        let mut state = self
            .shared
            .database
            .fast_ratchet_state(&sender_id, message.chain_id())?
            .ok_or_else(|| anyhow!("no fast-ratchet sender key for live-location update"))?;
        let plaintext = state.decrypt(ciphertext)?;

        let padding_version = enc_node
            .attrs()
            .optional_string("v")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(2);
        let message = wacore::messages::decode_plaintext(&plaintext, padding_version)
            .context("decoding live-location update")?;
        let context = MessageContext::from_parts(&message, info, Arc::clone(&client));
        self.shared.receive_live_location_update(context).await?;
        // Advance the durable ratchet only after the location update commits.
        // If persistence fails, leaving the stanza unacked lets WhatsApp retry
        // it with the still-usable key instead of losing the update forever.
        self.shared
            .database
            .store_fast_ratchet_state(&sender_id, &state)?;

        let from = if info.source.is_group {
            &info.source.chat
        } else {
            &info.source.sender
        };
        let mut ack_source = NodeBuilder::new("message")
            .attr("id", &info.id)
            .attr("from", from);
        if let Some(recipient) = &info.source.recipient {
            ack_source = ack_source.attr("recipient", recipient);
        }
        if info.source.is_group {
            ack_source = ack_source.attr("participant", &info.source.sender);
        }
        let ack_source = ack_source.build();
        client
            .acknowledge_stanza(&ack_source.as_node_ref())
            .await
            .context("acknowledging live-location update")?;
        Ok(())
    }
}
