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
    // The two identities are built from different servers, so they can never
    // name the same row; `migrate_contact_jid` answers `false` for an identity
    // that equals itself anyway, which leaves the alias copies below untouched.
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
    if let Err(error) = shared
        .database
        .update_chat_phone_number(&canonical, &mapping.phone_number)
    {
        warn!(%error, %canonical, "could not cache canonical WhatsApp phone number");
    }
    canonical
}

// Resolve at delivery as well as acceptance: an offline reply may have been
// queued with a phone-number identity before group LID metadata was available.
pub(crate) async fn quote_participant(
    transport: &dyn Transport,
    chat: &Jid,
    mut sender_jid: String,
) -> String {
    if chat.is_group()
        && let Ok(metadata) = transport.group_metadata(chat).await
        && let Some(participant) = metadata.participants.into_iter().find(|participant| {
            [
                Some(&participant.jid),
                participant.phone_number.as_ref(),
                participant.lid.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|jid| jid.to_non_ad_string() == sender_jid)
        })
    {
        sender_jid = if metadata.addressing_mode
            == whatsapp_rust::wacore::types::message::AddressingMode::Lid
        {
            participant.lid.unwrap_or(participant.jid)
        } else {
            participant.phone_number.unwrap_or(participant.jid)
        }
        .to_non_ad_string();
    }
    sender_jid
}

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
    use crate::transport::fake::{Call, CallKind, FakeTransport, transport};
    use chrono::Utc;
    use std::sync::Arc;
    use whatsapp_rust::wacore::types::lid_pn::{LearningSource, LidPnEntry};
    use whatsapp_rust::wacore::types::message::AddressingMode;
    use whatsapp_rust::{GroupParticipant, GroupParticipantDetails, ParticipantType};

    fn seed_chat(shared: &Shared, jid: &str, name: &str, is_group: bool) {
        shared
            .database
            .insert_history_conversation(
                &Chat {
                    jid: jid.to_owned(),
                    name: name.to_owned(),
                    phone_number: None,
                    last_message: String::new(),
                    last_sender_name: String::new(),
                    last_timestamp: 10,
                    unread: 0,
                    pinned: false,
                    muted: false,
                    is_group,
                },
                &[],
            )
            .unwrap();
    }

    fn mapping(lid: &str, phone_number: &str) -> LidPnEntry {
        LidPnEntry {
            lid: lid.into(),
            phone_number: phone_number.into(),
            created_at: 1,
            learning_source: LearningSource::Usync,
        }
    }

    fn participant(jid: &str, phone_number: Option<&str>, lid: Option<&str>) -> GroupParticipant {
        GroupParticipant {
            jid: jid.parse().unwrap(),
            phone_number: phone_number.map(|value| value.parse().unwrap()),
            lid: lid.map(|value| value.parse().unwrap()),
            username: None,
            participant_type: ParticipantType::Member,
            details: None,
        }
    }

    // `GroupParticipantDetails` is `#[non_exhaustive]`, so a struct literal is
    // not available outside its own crate.
    #[allow(clippy::field_reassign_with_default)]
    fn with_profile_name(mut participant: GroupParticipant, name: &str) -> GroupParticipant {
        let mut details = GroupParticipantDetails::default();
        details.display_name = Some(name.into());
        participant.details = Some(Box::new(details));
        participant
    }

    #[tokio::test]
    async fn identities_whatsapp_cannot_alias_stay_exactly_as_requested() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let fake = Arc::new(FakeTransport::new());
        let client = transport(&fake);

        // Groups are never aliased, so the daemon does not even ask.
        assert_eq!(
            canonical_contact_jid(&shared, client.as_ref(), &"123-456@g.us".parse().unwrap()).await,
            "123-456@g.us"
        );
        assert!(fake.calls().is_empty());

        // An unmapped phone-number JID caches its own number on the chat row.
        seed_chat(&shared, "31600000000@s.whatsapp.net", "Ada", false);
        assert_eq!(
            canonical_contact_jid(
                &shared,
                client.as_ref(),
                &"31600000000:3@s.whatsapp.net".parse().unwrap(),
            )
            .await,
            "31600000000@s.whatsapp.net"
        );
        assert_eq!(
            shared.database.list_chats(10).unwrap()[0]
                .phone_number
                .as_deref(),
            Some("31600000000")
        );

        // An unmapped LID has no phone number to cache and stays a LID.
        assert_eq!(
            canonical_contact_jid(
                &shared,
                client.as_ref(),
                &"100000012345678@lid".parse().unwrap(),
            )
            .await,
            "100000012345678@lid"
        );
        assert_eq!(
            fake.calls_of(CallKind::LidPnEntry).len(),
            2,
            "only addressable identities are looked up"
        );

        // A failed lookup degrades to the requested identity.
        fake.fail(CallKind::LidPnEntry, "offline");
        assert_eq!(
            canonical_contact_jid(
                &shared,
                client.as_ref(),
                &"100000012345678@lid".parse().unwrap(),
            )
            .await,
            "100000012345678@lid"
        );

        // A read-only database cannot cache the number but must not panic.
        fake.succeed(CallKind::LidPnEntry);
        shared
            .database
            .execute_test_sql("PRAGMA query_only = ON")
            .unwrap();
        assert_eq!(
            canonical_contact_jid(
                &shared,
                client.as_ref(),
                &"31600000001@s.whatsapp.net".parse().unwrap(),
            )
            .await,
            "31600000001@s.whatsapp.net"
        );
        shared
            .database
            .execute_test_sql("PRAGMA query_only = OFF")
            .unwrap();
    }

    #[tokio::test]
    async fn a_resolved_alias_merges_the_chat_and_carries_its_cached_files() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assets::private_dir(&shared.media_dir).unwrap();
        assets::private_dir(&shared.avatar_dir).unwrap();
        let alias = "100000012345678@lid";
        let canonical = "31612345678@s.whatsapp.net";
        seed_chat(&shared, alias, "Ada", false);
        let alias_media = assets::message_image_path(&shared.media_dir, alias, "MSG-1");
        assets::write_private_bytes(&alias_media, b"image").unwrap();
        assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, alias), b"avatar")
            .unwrap();
        let fake = Arc::new(
            FakeTransport::new()
                .with_lid_pn_entry(alias, Some(mapping("100000012345678", "31612345678"))),
        );
        let client = transport(&fake);

        assert_eq!(
            canonical_contact_jid(&shared, client.as_ref(), &alias.parse().unwrap()).await,
            canonical
        );

        let chats = shared.database.list_chats(10).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].jid, canonical);
        assert_eq!(chats[0].phone_number.as_deref(), Some("31612345678"));
        assert!(
            assets::message_image_path(&shared.media_dir, canonical, "MSG-1").exists(),
            "cached media follows the merged identity"
        );
        assert!(assets::avatar_path(&shared.avatar_dir, canonical).exists());

        // Repeating the resolution finds nothing left to migrate.
        assert_eq!(
            canonical_contact_jid(&shared, client.as_ref(), &alias.parse().unwrap()).await,
            canonical
        );
    }

    #[tokio::test]
    async fn alias_merge_failures_are_logged_and_never_abort_the_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let alias = "100000012345678@lid";
        let canonical = "31612345678@s.whatsapp.net";
        let fake = Arc::new(
            FakeTransport::new()
                .with_lid_pn_entry(alias, Some(mapping("100000012345678", "31612345678"))),
        );
        let client = transport(&fake);

        // The media directory does not exist yet, so the copy pass fails.
        seed_chat(&shared, alias, "Ada", false);
        assert_eq!(
            canonical_contact_jid(&shared, client.as_ref(), &alias.parse().unwrap()).await,
            canonical
        );

        // An avatar whose cache entry is not a file cannot be copied.
        assets::private_dir(&shared.media_dir).unwrap();
        assets::private_dir(&shared.avatar_dir).unwrap();
        seed_chat(&shared, alias, "Ada", false);
        std::fs::create_dir(assets::avatar_path(&shared.avatar_dir, alias)).unwrap();
        assert_eq!(
            canonical_contact_jid(&shared, client.as_ref(), &alias.parse().unwrap()).await,
            canonical
        );
        std::fs::remove_dir(assets::avatar_path(&shared.avatar_dir, alias)).unwrap();

        // A blocked media rewrite leaves the merged rows in place.
        seed_chat(&shared, alias, "Ada", false);
        seed_chat(&shared, "31600000009@s.whatsapp.net", "Bob", false);
        let alias_media = assets::message_image_path(&shared.media_dir, alias, "MSG-1");
        assets::write_private_bytes(&alias_media, b"image").unwrap();
        let media_json = format!(
            "{{\"kind\":\"image\",\"path\":\"{}\"}}",
            alias_media.display()
        );
        shared
            .database
            .execute_test_sql(&format!(
                "INSERT INTO messages
                   (chat_jid, id, sender_jid, sender_name, text, timestamp, from_me, media_json)
                 VALUES ('31600000009@s.whatsapp.net', 'MSG-1', '31600000009@s.whatsapp.net',
                         'Bob', '[Image]', 1, 0, '{media_json}');
                 CREATE TRIGGER block_media_rewrite BEFORE UPDATE OF media_json ON messages
                 BEGIN SELECT RAISE(ABORT, 'media rewrite blocked'); END;"
            ))
            .unwrap();
        assert_eq!(
            canonical_contact_jid(&shared, client.as_ref(), &alias.parse().unwrap()).await,
            canonical
        );
        assert_eq!(
            shared
                .database
                .messages("31600000009@s.whatsapp.net", 10)
                .unwrap()
                .len(),
            1
        );

        // A broken database fails the merge and the phone-number cache alike.
        shared
            .database
            .execute_test_sql("DROP TRIGGER block_media_rewrite; DROP TABLE chats;")
            .unwrap();
        assert_eq!(
            canonical_contact_jid(&shared, client.as_ref(), &alias.parse().unwrap()).await,
            canonical
        );
    }

    #[tokio::test]
    async fn direct_chat_alias_reconciliation_walks_every_stored_lid() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let alias = "100000012345678@lid";
        seed_chat(&shared, alias, "Ada", false);
        shared
            .database
            .execute_test_sql("INSERT INTO chats (jid, name) VALUES ('weird@LID', 'x')")
            .unwrap();
        let fake = Arc::new(
            FakeTransport::new()
                .with_lid_pn_entry(alias, Some(mapping("100000012345678", "31612345678"))),
        );
        let client = transport(&fake);

        reconcile_direct_chat_aliases(&shared, client.as_ref()).await;

        // Every parseable stored LID is asked about exactly once.
        assert_eq!(
            fake.calls_of(CallKind::LidPnEntry),
            vec![Call::LidPnEntry(alias.into())],
            "an unparseable stored identity is skipped"
        );
        assert!(
            shared
                .database
                .list_chats(10)
                .unwrap()
                .iter()
                .any(|chat| chat.jid == "31612345678@s.whatsapp.net")
        );

        shared
            .database
            .execute_test_sql("DROP TABLE chats")
            .unwrap();
        fake.clear_calls();
        reconcile_direct_chat_aliases(&shared, client.as_ref()).await;
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn poll_creators_use_the_addressing_mode_of_their_conversation() {
        let group: Jid = "123-456@g.us".parse().unwrap();
        let direct: Jid = "31600000000@s.whatsapp.net".parse().unwrap();
        let lid_group = whatsapp_rust::GroupMetadata {
            addressing_mode: AddressingMode::Lid,
            ..whatsapp_rust::GroupMetadata::default()
        };
        let fake = Arc::new(
            FakeTransport::new()
                .with_pn("31600000000:2@s.whatsapp.net")
                .with_lid("100000000000000:2@lid")
                .with_group_metadata("123-456@g.us", lid_group),
        );
        let client = transport(&fake);

        assert_eq!(
            own_poll_creator_jid(client.as_ref(), &group).await.unwrap(),
            "100000000000000@lid".parse::<Jid>().unwrap()
        );
        assert_eq!(
            own_poll_creator_jid(client.as_ref(), &direct)
                .await
                .unwrap(),
            "31600000000@s.whatsapp.net".parse::<Jid>().unwrap()
        );

        // A group whose metadata cannot be read falls back to the phone number.
        let unknown_group: Jid = "999-999@g.us".parse().unwrap();
        assert_eq!(
            own_poll_creator_jid(client.as_ref(), &unknown_group)
                .await
                .unwrap(),
            "31600000000@s.whatsapp.net".parse::<Jid>().unwrap()
        );

        let unpaired = Arc::new(FakeTransport::new().with_group_metadata(
            "123-456@g.us",
            whatsapp_rust::GroupMetadata {
                addressing_mode: AddressingMode::Lid,
                ..whatsapp_rust::GroupMetadata::default()
            },
        ));
        let unpaired_client = transport(&unpaired);
        assert_eq!(
            own_poll_creator_jid(unpaired_client.as_ref(), &group)
                .await
                .unwrap_err()
                .to_string(),
            "own LID is unavailable for this group poll"
        );
        assert_eq!(
            own_poll_creator_jid(unpaired_client.as_ref(), &direct)
                .await
                .unwrap_err()
                .to_string(),
            "own WhatsApp JID is unavailable"
        );
    }

    #[tokio::test]
    async fn listed_chats_backfill_phone_numbers_and_remember_the_misses() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let mapped = "100000012345678@lid";
        let unmapped = "100000087654321@lid";
        let failing = "100000011111111@lid";
        seed_chat(&shared, "31600000000@s.whatsapp.net", "Ada", false);
        seed_chat(&shared, "123-456@g.us", "Garden", true);
        seed_chat(&shared, "status@broadcast", "Status", false);
        shared
            .database
            .execute_test_sql("INSERT INTO chats (jid, name) VALUES ('broken', 'x')")
            .unwrap();
        seed_chat(&shared, mapped, "Bob", false);
        seed_chat(&shared, unmapped, "Carol", false);
        seed_chat(&shared, failing, "Dan", false);
        let fake = Arc::new(
            FakeTransport::new()
                .with_lid_pn_entry(mapped, Some(mapping("100000012345678", "31612345678")))
                .with_lid_pn_entry(unmapped, None),
        );

        // Without a client the LID chats cannot be resolved at all.
        let chats = list_chats_with_phone_numbers(&shared, 10).await.unwrap();
        assert!(
            chats
                .iter()
                .filter(|chat| chat.jid.ends_with("@lid"))
                .all(|chat| chat.phone_number.is_none())
        );
        assert!(fake.calls().is_empty());

        *shared.client.write().await = Some(transport(&fake));
        fake.fail(CallKind::LidPnEntry, "offline");
        let chats = list_chats_with_phone_numbers(&shared, 10).await.unwrap();
        let phone_number = |jid: &str| {
            chats
                .iter()
                .find(|chat| chat.jid == jid)
                .and_then(|chat| chat.phone_number.clone())
        };
        assert_eq!(
            phone_number("31600000000@s.whatsapp.net").as_deref(),
            Some("31600000000")
        );
        assert_eq!(phone_number("123-456@g.us"), None);
        assert_eq!(phone_number("status@broadcast"), None);
        assert_eq!(phone_number(mapped), None);

        fake.succeed(CallKind::LidPnEntry);
        let chats = list_chats_with_phone_numbers(&shared, 10).await.unwrap();
        assert_eq!(
            chats
                .iter()
                .find(|chat| chat.jid == mapped)
                .and_then(|chat| chat.phone_number.clone())
                .as_deref(),
            Some("31612345678")
        );
        assert!(shared.phone_number_is_missing(unmapped));

        // The cached number and the cached misses stop every SDK lookup.
        fake.clear_calls();
        list_chats_with_phone_numbers(&shared, 10).await.unwrap();
        assert!(fake.calls().is_empty());

        // A read-only database still reports the resolved number to the shell.
        seed_chat(&shared, "31600000005@s.whatsapp.net", "Eve", false);
        shared
            .database
            .execute_test_sql("PRAGMA query_only = ON")
            .unwrap();
        let chats = list_chats_with_phone_numbers(&shared, 10).await.unwrap();
        assert_eq!(
            chats
                .iter()
                .find(|chat| chat.jid == "31600000005@s.whatsapp.net")
                .and_then(|chat| chat.phone_number.clone())
                .as_deref(),
            Some("31600000005")
        );
        shared
            .database
            .execute_test_sql("PRAGMA query_only = OFF")
            .unwrap();
        assert!(shared.phone_number_is_missing(failing));
    }

    #[tokio::test]
    async fn group_participants_are_canonical_deduplicated_named_and_sorted() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let own_pn = "31600000000@s.whatsapp.net";
        let named = "31600000001@s.whatsapp.net";
        let chat_named = "31600000002@s.whatsapp.net";
        let profile_named = "31600000003@s.whatsapp.net";
        let unnamed = "31600000004@s.whatsapp.net";
        shared
            .database
            .update_address_book_name(named, "Bob")
            .unwrap();
        seed_chat(&shared, chat_named, "Carol", false);
        shared
            .database
            .execute_test_sql(
                "UPDATE chats SET name = 'Carol', name_source = 20 WHERE jid = '31600000002@s.whatsapp.net'",
            )
            .unwrap();
        // A stored name equal to the JID is a placeholder, not a real name.
        shared
            .database
            .update_address_book_name(profile_named, profile_named)
            .unwrap();
        let fake = Arc::new(
            FakeTransport::new()
                .with_pn("31600000000:2@s.whatsapp.net")
                .with_lid("100000000000000@lid"),
        );
        let client = transport(&fake);

        let identity = |participant| group_participant_identity(participant);
        let identities = vec![
            identity(participant(unnamed, None, None)),
            identity(participant(named, None, Some("100000000000001@lid"))),
            identity(with_profile_name(
                participant(profile_named, None, None),
                "Dave",
            )),
            identity(participant(chat_named, None, None)),
            identity(participant("100000000000000@lid", Some(own_pn), None)),
            identity(participant(named, None, None)),
        ];

        let participants = resolve_group_participants(&shared, client.as_ref(), identities).await;

        assert_eq!(
            participants
                .iter()
                .map(|participant| (
                    participant.jid.as_str(),
                    participant.name.as_str(),
                    participant.is_me
                ))
                .collect::<Vec<_>>(),
            // Own identity first, then by display name; unnamed participants
            // sort ahead of named ones and are ordered by JID.
            vec![
                (own_pn, "", true),
                (unnamed, "", false),
                (named, "Bob", false),
                (chat_named, "Carol", false),
                (profile_named, "Dave", false),
            ]
        );
        assert_eq!(
            participants[2].aliases,
            vec!["100000000000001@lid".to_owned()]
        );
        assert_eq!(participants[0].aliases, vec!["100000000000000@lid"]);
    }

    #[test]
    fn participant_identities_collapse_every_addressable_alias() {
        let identity = group_participant_identity(with_profile_name(
            GroupParticipant {
                jid: "100000000000001:5@lid".parse().unwrap(),
                phone_number: Some("31600000001:5@s.whatsapp.net".parse().unwrap()),
                lid: Some("100000000000001@lid".parse().unwrap()),
                username: None,
                participant_type: ParticipantType::Member,
                details: None,
            },
            "   ",
        ));

        assert_eq!(identity.jid.to_string(), "31600000001:5@s.whatsapp.net");
        assert_eq!(
            identity.aliases,
            vec![
                "100000000000001@lid".to_owned(),
                "31600000001@s.whatsapp.net".to_owned(),
            ]
        );
        assert_eq!(identity.profile_name, None);
    }

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
