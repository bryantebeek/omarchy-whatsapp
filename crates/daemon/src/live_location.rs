use aes::Aes256;
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use cbc::Decryptor;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;
use tokio::sync::Mutex;
use whatsapp_rust::bot::MessageContext;
use whatsapp_rust::types::enc_handler::EncHandler;
use whatsapp_rust::types::jid::JidExt as SignalJidExt;
use whatsapp_rust::wacore::libsignal::protocol::{PublicKey, SenderKeyMessage};
use whatsapp_rust::wacore_binary::{Node, builder::NodeBuilder};
use whatsapp_rust::{Client, wacore};

use super::Shared;

const FAST_RATCHET_CHAINS: usize = 8;
const FAST_RATCHET_KEY_BYTES: usize = 32;
const SIGNAL_MESSAGE_VERSION: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FastRatchetState {
    pub sender_key_id: u32,
    pub iteration: u32,
    pub chain_keys: [[u8; FAST_RATCHET_KEY_BYTES]; FAST_RATCHET_CHAINS],
    pub signing_key: Vec<u8>,
}

impl FastRatchetState {
    pub(crate) fn from_distribution(bytes: &[u8]) -> Result<Self> {
        let (&version, protobuf) = bytes
            .split_first()
            .ok_or_else(|| anyhow!("empty fast-ratchet sender-key distribution"))?;
        if version >> 4 != SIGNAL_MESSAGE_VERSION {
            bail!("unsupported fast-ratchet distribution version");
        }

        let mut sender_key_id = None;
        let mut iteration = None;
        let mut chain_keys = Vec::with_capacity(FAST_RATCHET_CHAINS);
        let mut signing_key = None;
        decode_fields(protobuf, |field, wire, value| {
            match (field, wire, value) {
                (1, 0, ProtobufValue::Varint(value)) => {
                    sender_key_id = Some(u32::try_from(value).context("fast-ratchet key id")?);
                }
                (2, 0, ProtobufValue::Varint(value)) => {
                    iteration = Some(u32::try_from(value).context("fast-ratchet iteration")?);
                }
                (3, 2, ProtobufValue::Bytes(value)) => chain_keys.push(value.to_vec()),
                (4, 2, ProtobufValue::Bytes(value)) => signing_key = Some(value.to_vec()),
                _ => {}
            }
            Ok(())
        })?;

        let chain_keys: [[u8; FAST_RATCHET_KEY_BYTES]; FAST_RATCHET_CHAINS] = chain_keys
            .into_iter()
            .map(|key| {
                key.try_into()
                    .map_err(|_| anyhow!("invalid fast-ratchet chain-key length"))
            })
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .map_err(|_| anyhow!("fast-ratchet distribution must contain eight chain keys"))?;
        let signing_key = signing_key.ok_or_else(|| anyhow!("missing fast-ratchet signing key"))?;
        PublicKey::try_from(signing_key.as_slice()).context("invalid fast-ratchet signing key")?;

        Ok(Self {
            sender_key_id: sender_key_id.ok_or_else(|| anyhow!("missing fast-ratchet key id"))?,
            iteration: iteration.ok_or_else(|| anyhow!("missing fast-ratchet iteration"))?,
            chain_keys,
            signing_key,
        })
    }

    pub(crate) fn encode_chain_keys(&self) -> Vec<u8> {
        self.chain_keys.concat()
    }

    pub(crate) fn from_database(
        sender_key_id: u32,
        iteration: u32,
        chain_keys: &[u8],
        signing_key: Vec<u8>,
    ) -> Result<Self> {
        let chain_keys = chain_keys
            .chunks_exact(FAST_RATCHET_KEY_BYTES)
            .map(|key| key.try_into().expect("chunk length is fixed"))
            .collect::<Vec<[u8; FAST_RATCHET_KEY_BYTES]>>()
            .try_into()
            .map_err(|_| anyhow!("stored fast-ratchet state has invalid chain keys"))?;
        Ok(Self {
            sender_key_id,
            iteration,
            chain_keys,
            signing_key,
        })
    }

    fn decrypt(&mut self, serialized: &[u8]) -> Result<Vec<u8>> {
        let message = SenderKeyMessage::try_from(serialized)
            .context("decoding fast-ratchet sender-key message")?;
        if message.chain_id() != self.sender_key_id {
            bail!("fast-ratchet sender-key id does not match stored state");
        }
        let signing_key = PublicKey::try_from(self.signing_key.as_slice())
            .context("reading fast-ratchet signing key")?;
        if !message
            .verify_signature(&signing_key)
            .context("verifying fast-ratchet message")?
        {
            bail!("invalid fast-ratchet message signature");
        }
        if message.iteration() < self.iteration {
            bail!("fast-ratchet update is older than the stored state");
        }

        self.advance_to(message.iteration())?;
        let message_seed = hmac_sha256(&self.chain_keys[FAST_RATCHET_CHAINS - 1], 1)?;
        let mut derived = [0_u8; 48];
        Hkdf::<Sha256>::new(None, &message_seed)
            .expand(b"WhisperGroup", &mut derived)
            .map_err(|_| anyhow!("could not derive fast-ratchet message key"))?;
        let plaintext = Decryptor::<Aes256>::new_from_slices(&derived[16..], &derived[..16])
            .map_err(|_| anyhow!("invalid fast-ratchet AES key"))?
            .decrypt_padded_vec::<Pkcs7>(message.ciphertext()?)
            .map_err(|_| anyhow!("fast-ratchet message decryption failed"))?;
        self.advance_to(
            message
                .iteration()
                .checked_add(1)
                .ok_or_else(|| anyhow!("fast-ratchet iteration overflow"))?,
        )?;
        Ok(plaintext)
    }

    fn advance_to(&mut self, target: u32) -> Result<()> {
        if target < self.iteration {
            bail!("cannot rewind a fast-ratchet state");
        }
        let mut current = chain_iterations(self.iteration);
        let target_iterations = chain_iterations(target);
        for chain in 0..FAST_RATCHET_CHAINS {
            while target_iterations[chain] > current[chain] {
                if chain < FAST_RATCHET_CHAINS - 1 && target_iterations[chain] - 1 == current[chain]
                {
                    self.chain_keys[chain + 1] = hmac_sha256(
                        &self.chain_keys[chain],
                        u8::try_from(chain + 3).expect("chain index fits in u8"),
                    )?;
                    current[chain + 1] = 0;
                }
                self.chain_keys[chain] = hmac_sha256(
                    &self.chain_keys[chain],
                    u8::try_from(chain + 2).expect("chain index fits in u8"),
                )?;
                current[chain] += 1;
            }
        }
        self.iteration = target;
        Ok(())
    }
}

fn chain_iterations(iteration: u32) -> [u32; FAST_RATCHET_CHAINS] {
    let mut values = [0_u32; FAST_RATCHET_CHAINS];
    for digit in 0..FAST_RATCHET_CHAINS {
        values[FAST_RATCHET_CHAINS - digit - 1] = (iteration >> (digit * 4)) & 0x0f;
    }
    for value in &mut values[..FAST_RATCHET_CHAINS - 1] {
        *value += 1;
    }
    values
}

fn hmac_sha256(key: &[u8], value: u8) -> Result<[u8; 32]> {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(key)
        .map_err(|_| anyhow!("invalid fast-ratchet HMAC key"))?;
    mac.update(&[value]);
    Ok(mac.finalize().into_bytes().into())
}

enum ProtobufValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Ignored,
}

fn take_varint(input: &mut &[u8]) -> Result<u64> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let (&byte, rest) = input
            .split_first()
            .ok_or_else(|| anyhow!("truncated protobuf varint"))?;
        *input = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("protobuf varint is too long")
}

fn decode_fields<'a>(
    mut input: &'a [u8],
    mut visit: impl FnMut(u32, u8, ProtobufValue<'a>) -> Result<()>,
) -> Result<()> {
    while !input.is_empty() {
        let tag = take_varint(&mut input)?;
        let field = u32::try_from(tag >> 3).context("protobuf field number")?;
        let wire = u8::try_from(tag & 7).expect("wire type fits in u8");
        match wire {
            0 => {
                let value = take_varint(&mut input)?;
                visit(field, wire, ProtobufValue::Varint(value))?;
            }
            1 => {
                input = input
                    .get(8..)
                    .ok_or_else(|| anyhow!("truncated fixed64 protobuf field"))?;
                visit(field, wire, ProtobufValue::Ignored)?;
            }
            2 => {
                let length =
                    usize::try_from(take_varint(&mut input)?).context("protobuf field length")?;
                let (value, rest) = input
                    .split_at_checked(length)
                    .ok_or_else(|| anyhow!("truncated protobuf bytes field"))?;
                input = rest;
                visit(field, wire, ProtobufValue::Bytes(value))?;
            }
            5 => {
                input = input
                    .get(4..)
                    .ok_or_else(|| anyhow!("truncated fixed32 protobuf field"))?;
                visit(field, wire, ProtobufValue::Ignored)?;
            }
            _ => bail!("unsupported protobuf wire type {wire}"),
        }
    }
    Ok(())
}

pub(crate) struct FastRatchetHandler {
    shared: Arc<Shared>,
    decrypt_lock: Mutex<()>,
}

impl FastRatchetHandler {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            decrypt_lock: Mutex::new(()),
        }
    }
}

#[async_trait]
impl EncHandler for FastRatchetHandler {
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

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockModeEncrypt;
    use cbc::Encryptor;
    use rand::{SeedableRng, rngs::StdRng};
    use whatsapp_rust::wacore::libsignal::protocol::PrivateKey;

    fn field_varint(field: u8, value: u32, output: &mut Vec<u8>) {
        output.push(field << 3);
        let mut value = value;
        while value >= 0x80 {
            output.push(u8::try_from(value & 0x7f).unwrap() | 0x80);
            value >>= 7;
        }
        output.push(u8::try_from(value).unwrap());
    }

    fn field_bytes(field: u8, value: &[u8], output: &mut Vec<u8>) {
        output.push((field << 3) | 2);
        output.push(u8::try_from(value.len()).unwrap());
        output.extend_from_slice(value);
    }

    #[test]
    fn distribution_parser_reads_all_fast_ratchet_chains() {
        let mut bytes = vec![0x33];
        field_varint(1, 42, &mut bytes);
        field_varint(2, 7, &mut bytes);
        for value in 1..=8 {
            field_bytes(3, &[value; 32], &mut bytes);
        }
        let signing_key = PrivateKey::deserialize(&[7; 32])
            .unwrap()
            .public_key()
            .unwrap()
            .serialize();
        field_bytes(4, &signing_key, &mut bytes);
        let parsed = FastRatchetState::from_distribution(&bytes).unwrap();
        assert_eq!(parsed.sender_key_id, 42);
        assert_eq!(parsed.iteration, 7);
        assert_eq!(parsed.chain_keys[0], [1; 32]);
        assert_eq!(parsed.chain_keys[7], [8; 32]);
        assert_eq!(parsed.signing_key, signing_key);
    }

    #[test]
    fn distribution_parser_rejects_incomplete_and_malformed_keys() {
        assert!(FastRatchetState::from_distribution(&[]).is_err());
        assert!(FastRatchetState::from_distribution(&[0x23]).is_err());

        let mut incomplete = vec![0x33];
        field_varint(1, 42, &mut incomplete);
        field_varint(2, 7, &mut incomplete);
        for _ in 0..7 {
            field_bytes(3, &[1; 32], &mut incomplete);
        }
        field_bytes(4, &[5; 33], &mut incomplete);
        assert!(FastRatchetState::from_distribution(&incomplete).is_err());

        let mut wrong_chain_length = vec![0x33];
        field_varint(1, 42, &mut wrong_chain_length);
        field_varint(2, 7, &mut wrong_chain_length);
        for _ in 0..8 {
            field_bytes(3, &[1; 31], &mut wrong_chain_length);
        }
        field_bytes(4, &[5; 33], &mut wrong_chain_length);
        assert!(FastRatchetState::from_distribution(&wrong_chain_length).is_err());
    }

    #[test]
    fn database_state_rejects_wrong_chain_count_and_round_trips_valid_keys() {
        assert!(FastRatchetState::from_database(1, 2, &[0; 32], vec![3; 33]).is_err());

        let state = FastRatchetState {
            sender_key_id: 4,
            iteration: 5,
            chain_keys: std::array::from_fn(|index| [u8::try_from(index).unwrap(); 32]),
            signing_key: vec![6; 33],
        };
        let restored = FastRatchetState::from_database(
            state.sender_key_id,
            state.iteration,
            &state.encode_chain_keys(),
            state.signing_key.clone(),
        )
        .unwrap();
        assert_eq!(restored, state);
    }

    #[test]
    fn protobuf_decoder_rejects_truncated_and_unsupported_fields() {
        assert!(decode_fields(&[0x08, 0x80], |_, _, _| Ok(())).is_err());
        assert!(decode_fields(&[0x09, 0, 0, 0, 0], |_, _, _| Ok(())).is_err());
        assert!(decode_fields(&[0x12, 0x02, 0x01], |_, _, _| Ok(())).is_err());
        assert!(decode_fields(&[0x1d, 0, 0, 0], |_, _, _| Ok(())).is_err());
        assert!(decode_fields(&[0x1b], |_, _, _| Ok(())).is_err());

        let mut visited = 0;
        let mut fixed_fields = vec![0x09];
        fixed_fields.extend_from_slice(&[0; 8]);
        fixed_fields.push(0x15);
        fixed_fields.extend_from_slice(&[0; 4]);
        decode_fields(&fixed_fields, |_, _, value| {
            assert!(matches!(value, ProtobufValue::Ignored));
            visited += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(visited, 2);
    }

    #[test]
    fn fast_jump_matches_one_step_ratchets() {
        let base = FastRatchetState {
            sender_key_id: 1,
            iteration: 0,
            chain_keys: std::array::from_fn(|index| [u8::try_from(index).unwrap() + 1; 32]),
            signing_key: Vec::new(),
        };
        let mut jumped = base.clone();
        jumped.advance_to(4097).unwrap();
        let mut stepped = base;
        for iteration in 1..=4097 {
            stepped.advance_to(iteration).unwrap();
        }
        assert_eq!(jumped.chain_keys, stepped.chain_keys);
        assert!(jumped.advance_to(4096).is_err());
    }

    #[test]
    fn decrypts_a_signed_fast_ratchet_message_and_consumes_its_key() {
        let signing_key = PrivateKey::deserialize(&[7; 32]).unwrap();
        let mut state = FastRatchetState {
            sender_key_id: 42,
            iteration: 0,
            chain_keys: std::array::from_fn(|index| [u8::try_from(index).unwrap() + 1; 32]),
            signing_key: signing_key.public_key().unwrap().serialize().to_vec(),
        };
        let mut sender = state.clone();
        sender.advance_to(17).unwrap();
        let seed = hmac_sha256(&sender.chain_keys[FAST_RATCHET_CHAINS - 1], 1).unwrap();
        let mut derived = [0_u8; 48];
        Hkdf::<Sha256>::new(None, &seed)
            .expand(b"WhisperGroup", &mut derived)
            .unwrap();
        let plaintext = b"live-location protobuf";
        let ciphertext = Encryptor::<Aes256>::new_from_slices(&derived[16..], &derived[..16])
            .unwrap()
            .encrypt_padded_vec::<Pkcs7>(plaintext);
        let mut rng = StdRng::seed_from_u64(9);
        let envelope = SenderKeyMessage::new(
            SIGNAL_MESSAGE_VERSION,
            42,
            17,
            ciphertext.into_boxed_slice(),
            &mut rng,
            &signing_key,
        )
        .unwrap();

        assert_eq!(state.decrypt(envelope.serialized()).unwrap(), plaintext);
        assert_eq!(state.iteration, 18);
    }
}
