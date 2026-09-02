// WhatsApp identity resolution: JID normalization, LID/phone-number alias
// reconciliation, participant identity, and contact-name ingestion.

use crate::assets;
use crate::state::{Shared, broadcast_chats};
use crate::transport::Transport;
use crate::util::nonempty;
use anyhow::{Result, anyhow};
use omarchy_whatsapp_protocol::{Chat, ChatParticipant};
use std::collections::HashSet;
use tracing::warn;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

const CHAT_LIST_LIMIT: u32 = 500;

pub(crate) fn normalize_jid(value: &str) -> String {
    value
        .parse::<Jid>()
        .map_or_else(|_| value.to_owned(), |jid| jid.to_non_ad_string())
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn canonical_contact_jid(
    shared: &Shared,
    transport: &dyn Transport,
    jid: &Jid,
) -> String {
    let raw = jid.to_non_ad_string();
    if !jid.is_lid() && !jid.is_pn() {
        return raw;
    }
    let mapping = match transport.lid_pn_entry(jid).await {
        Ok(Some(mapping)) => mapping,
        Ok(None) => {
            if jid.is_pn()
                && let Err(error) = shared.database.update_chat_phone_number(&raw, &jid.user)
            {
                warn!(%error, %raw, "could not persist WhatsApp phone number");
            }
            return raw;
        }
        Err(error) => {
            warn!(%error, %raw, "could not resolve WhatsApp contact alias");
            return raw;
        }
    };
    let canonical = format!("{}@s.whatsapp.net", mapping.phone_number);
    let alias = format!("{}@lid", mapping.lid);
    if alias != canonical {
        match shared.database.migrate_contact_jid(&alias, &canonical) {
            Ok(true) => {
                let replacements = match assets::copy_chat_media_alias(
                    &shared.media_dir,
                    &alias,
                    &canonical,
                ) {
                    Ok(replacements) => replacements,
                    Err(error) => {
                        warn!(%error, %alias, %canonical, "could not preserve aliased WhatsApp media paths");
                        Vec::new()
                    }
                };
                if let Err(error) = shared.database.rewrite_media_paths(&replacements) {
                    warn!(%error, %alias, %canonical, "could not rewrite aliased WhatsApp media paths");
                }
                match assets::copy_avatar_alias(&shared.avatar_dir, &alias, &canonical) {
                    Ok(true) => shared.avatars_changed(),
                    Ok(false) => {}
                    Err(error) => {
                        warn!(%error, %alias, %canonical, "could not preserve aliased WhatsApp avatar");
                    }
                }
            }
            Ok(false) => {}
            Err(error) => {
                warn!(%error, %alias, %canonical, "could not merge WhatsApp contact alias");
            }
        }
    }
    if let Err(error) = shared
        .database
        .update_chat_phone_number(&canonical, &mapping.phone_number)
    {
        warn!(%error, %canonical, "could not cache canonical WhatsApp phone number");
    }
    canonical
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn own_poll_creator_jid(transport: &dyn Transport, chat: &Jid) -> Result<Jid> {
    if chat.is_group()
        && transport.group_metadata(chat).await.is_ok_and(|metadata| {
            metadata.addressing_mode == whatsapp_rust::wacore::types::message::AddressingMode::Lid
        })
    {
        return transport
            .lid()
            .map(|jid| jid.to_non_ad())
            .ok_or_else(|| anyhow!("own LID is unavailable for this group poll"));
    }
    transport
        .pn()
        .map(|jid| jid.to_non_ad())
        .ok_or_else(|| anyhow!("own WhatsApp JID is unavailable"))
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn reconcile_direct_chat_aliases(shared: &Shared, transport: &dyn Transport) {
    let jids = match shared.database.direct_chat_jids(CHAT_LIST_LIMIT) {
        Ok(jids) => jids,
        Err(error) => {
            warn!(%error, "could not enumerate WhatsApp contact aliases");
            return;
        }
    };
    for raw in jids {
        if let Ok(jid) = raw.parse::<Jid>() {
            canonical_contact_jid(shared, transport, &jid).await;
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn list_chats_with_phone_numbers(
    shared: &Shared,
    limit: u32,
) -> Result<Vec<Chat>> {
    // Alias reconciliation belongs to connect, history ingest, and contact
    // updates. The shell lists chats on every invalidation, so repeating a
    // per-chat SDK lookup here would put an unbounded query burst on the
    // cheapest command in the protocol.
    let client = shared.client.read().await.clone();
    let mut chats = shared.database.list_chats(limit)?;
    for chat in &mut chats {
        if chat.is_group || chat.phone_number.is_some() {
            continue;
        }
        let Ok(jid) = chat.jid.parse::<Jid>() else {
            continue;
        };
        let phone_number = if jid.is_pn() {
            Some(jid.user.to_string())
        } else if jid.is_lid() {
            let Some(client) = client.as_ref() else {
                continue;
            };
            if shared.phone_number_is_missing(&chat.jid) {
                continue;
            }
            match client.lid_pn_entry(&jid).await {
                Ok(Some(mapping)) => Some(mapping.phone_number.to_string()),
                Ok(None) => {
                    shared.remember_missing_phone_number(&chat.jid);
                    None
                }
                Err(error) => {
                    warn!(%error, "could not resolve WhatsApp contact phone number");
                    None
                }
            }
        } else {
            None
        };
        if let Some(phone_number) = phone_number.filter(|value| !value.is_empty()) {
            if let Err(error) = shared
                .database
                .update_chat_phone_number(&chat.jid, &phone_number)
            {
                warn!(%error, "could not cache WhatsApp contact phone number");
            }
            chat.phone_number = Some(phone_number);
        }
    }
    Ok(chats)
}

pub(crate) struct GroupParticipantIdentity {
    jid: Jid,
    aliases: Vec<String>,
    profile_name: Option<String>,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) fn group_participant_identity(
    participant: whatsapp_rust::GroupParticipant,
) -> GroupParticipantIdentity {
    let mut aliases = [
        Some(participant.jid.clone()),
        participant.phone_number.clone(),
        participant.lid.clone(),
    ]
    .into_iter()
    .flatten()
    .map(|jid| jid.to_non_ad_string())
    .collect::<Vec<_>>();
    aliases.sort();
    aliases.dedup();
    let profile_name = participant
        .details
        .as_deref()
        .and_then(|details| details.display_name.as_deref())
        .and_then(nonempty);
    GroupParticipantIdentity {
        jid: participant.phone_number.unwrap_or(participant.jid),
        aliases,
        profile_name,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn resolve_group_participants(
    shared: &Shared,
    transport: &dyn Transport,
    identities: Vec<GroupParticipantIdentity>,
) -> Vec<ChatParticipant> {
    let own_pn = transport.pn().map(|jid| jid.to_non_ad_string());
    let own_lid = transport.lid().map(|jid| jid.to_non_ad_string());
    let mut seen = HashSet::new();
    let mut participants = Vec::with_capacity(identities.len());

    for identity in identities {
        let is_me = identity.aliases.iter().any(|alias| {
            own_pn.as_deref() == Some(alias.as_str()) || own_lid.as_deref() == Some(alias.as_str())
        });
        let jid = canonical_contact_jid(shared, transport, &identity.jid).await;
        if !seen.insert(jid.clone()) {
            continue;
        }
        let aliases = identity
            .aliases
            .into_iter()
            .filter(|alias| alias != &jid)
            .collect::<Vec<_>>();
        let name = if is_me {
            String::new()
        } else {
            shared
                .database
                .contact_name(&jid)
                .ok()
                .flatten()
                .or_else(|| shared.database.chat_name(&jid).ok().flatten())
                .filter(|name| name != &jid)
                .or(identity.profile_name)
                .unwrap_or_default()
        };
        participants.push(ChatParticipant {
            jid,
            name,
            aliases,
            is_me,
        });
    }

    participants.sort_by_key(|participant| {
        (
            !participant.is_me,
            participant.name.to_lowercase(),
            participant.jid.clone(),
        )
    });
    participants
}

pub(crate) fn metadata_jid(value: &str, server: &str) -> String {
    if value.contains('@') {
        normalize_jid(value)
    } else {
        format!("{value}@{server}")
    }
}

pub(crate) fn ingest_contact_name(
    shared: &Shared,
    update: &whatsapp_rust::types::events::ContactUpdate,
) {
    let action = &update.action;
    let Some(name) = action
        .full_name
        .as_deref()
        .and_then(nonempty)
        .or_else(|| action.first_name.as_deref().and_then(nonempty))
        .or_else(|| action.username.as_deref().and_then(nonempty))
    else {
        if update.from_full_sync {
            shared.mark_contact_sync_complete();
        }
        return;
    };

    let mut jids = HashSet::new();
    jids.insert(update.jid.to_non_ad_string());
    if let Some(pn) = action.pn_jid.as_deref().and_then(nonempty) {
        jids.insert(metadata_jid(&pn, "s.whatsapp.net"));
    }
    if let Some(lid) = action.lid_jid.as_deref().and_then(nonempty) {
        jids.insert(metadata_jid(&lid, "lid"));
    }

    let mut changed = false;
    for jid in jids {
        match shared.database.update_address_book_name(&jid, &name) {
            Ok(updated) => changed |= updated,
            Err(error) => warn!(%error, %jid, "could not persist WhatsApp contact name"),
        }
    }
    if update.from_full_sync {
        shared.mark_contact_sync_complete();
    }
    if changed {
        broadcast_chats(shared);
    }
}

pub(crate) fn display_name(shared: &Shared, jid: &Jid) -> String {
    let raw = jid.to_non_ad_string();
    shared
        .database
        .chat_name(&raw)
        .ok()
        .flatten()
        .or_else(|| shared.database.contact_name(&raw).ok().flatten())
        .unwrap_or(raw)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::default_trait_access)] // Generated protobuf fixture types are inferred by MessageField.
mod tests {
    use super::*;
    use crate::test_support::test_shared;
    use chrono::Utc;

    #[test]
    fn contact_ingestion_prefers_names_and_normalizes_all_identities() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let update = whatsapp_rust::types::events::ContactUpdate::builder()
            .jid("1:2@s.whatsapp.net".parse().unwrap())
            .timestamp(Utc::now())
            .action(Box::new(wa::sync_action_value::ContactAction {
                full_name: Some("  Ada Lovelace  ".into()),
                pn_jid: Some("31612345678".into()),
                lid_jid: Some("100000012345678".into()),
                ..Default::default()
            }))
            .from_full_sync(true)
            .build();
        ingest_contact_name(&shared, &update);
        for jid in [
            "1@s.whatsapp.net",
            "31612345678@s.whatsapp.net",
            "100000012345678@lid",
        ] {
            assert_eq!(
                shared.database.contact_name(jid).unwrap().as_deref(),
                Some("Ada Lovelace")
            );
        }
        assert!(shared.contact_sync_marker.exists());

        let empty = whatsapp_rust::types::events::ContactUpdate::builder()
            .jid("2@s.whatsapp.net".parse().unwrap())
            .timestamp(Utc::now())
            .action(Box::default())
            .from_full_sync(true)
            .build();
        ingest_contact_name(&shared, &empty);
        assert_eq!(
            shared.database.contact_name("2@s.whatsapp.net").unwrap(),
            None
        );
    }
}
