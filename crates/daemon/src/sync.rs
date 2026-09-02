// Background synchronization passes: group subjects, contact names, avatars,
// video previews, and the one-time app-state resync preparation.

use crate::assets;
use crate::state::{Shared, broadcast_chats, broadcast_messages, write_private_marker};
use crate::transport::Transport;
use crate::util::nonempty;
use anyhow::{Context, Result};
use futures::StreamExt;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

const AVATAR_SYNC_LIMIT: u32 = 1_000;

pub(crate) async fn sync_group_names(shared: Arc<Shared>, transport: Arc<dyn Transport>) {
    let _guard = shared.group_name_sync.lock().await;
    match transport.participating_groups().await {
        Ok(groups) => {
            let mut updated = 0usize;
            for (jid, metadata) in groups {
                match shared
                    .database
                    .update_group_name(&jid.to_non_ad_string(), &metadata.subject)
                {
                    Ok(true) => updated += 1,
                    Ok(false) => {}
                    Err(error) => warn!(%error, %jid, "could not persist WhatsApp group subject"),
                }
            }

            // Participating metadata covers current groups in one request. Old
            // conversations can be absent from that response, so make a small
            // best-effort pass over the few unresolved subjects as well.
            let unresolved = match shared.database.unresolved_chat_jids(true, 32) {
                Ok(unresolved) => unresolved,
                Err(error) => {
                    warn!(%error, "could not select unresolved WhatsApp group subjects");
                    return;
                }
            };
            for raw_jid in unresolved {
                let Ok(jid) = raw_jid.parse::<Jid>() else {
                    continue;
                };
                match transport.group_metadata(&jid).await {
                    Ok(metadata) => {
                        if nonempty(&metadata.subject).is_none() {
                            warn!(%jid, "WhatsApp returned an empty group subject");
                        } else {
                            match shared
                                .database
                                .update_group_name(&raw_jid, &metadata.subject)
                            {
                                Ok(true) => updated += 1,
                                Ok(false) => {}
                                Err(error) => {
                                    warn!(%error, %jid, "could not persist recovered WhatsApp group subject");
                                }
                            }
                        }
                    }
                    Err(error) => {
                        warn!(%error, %jid, "could not recover WhatsApp group subject");
                    }
                }
            }
            if updated > 0 {
                broadcast_chats(&shared);
            }
            info!(updated, "synchronized WhatsApp group subjects");
        }
        Err(error) => warn!(%error, "could not synchronize WhatsApp group subjects"),
    }
}

pub(crate) async fn sync_missing_contact_names(shared: Arc<Shared>, transport: Arc<dyn Transport>) {
    let unresolved = match shared.database.unresolved_chat_jids(false, 100) {
        Ok(unresolved) => unresolved,
        Err(error) => {
            warn!(%error, "could not select unresolved WhatsApp contact names");
            return;
        }
    };
    let jids = unresolved
        .into_iter()
        .filter_map(|raw| raw.parse::<Jid>().ok())
        .filter(|jid| (jid.is_pn() || jid.is_lid()) && jid.user.as_str() != "0")
        .collect::<Vec<_>>();
    if jids.is_empty() {
        return;
    }

    match transport.user_info(&jids).await {
        Ok(infos) => {
            let mut updated = 0usize;
            for info in infos.into_values() {
                let Some(name) = info.verified_name.as_deref().and_then(nonempty) else {
                    continue;
                };
                let mut candidates = vec![info.jid.to_non_ad_string()];
                if let Some(lid) = info.lid {
                    candidates.push(lid.to_non_ad_string());
                }
                for jid in candidates {
                    match shared.database.update_contact_name(&jid, &name) {
                        Ok(true) => updated += 1,
                        Ok(false) => {}
                        Err(error) => {
                            warn!(%error, %jid, "could not persist WhatsApp business profile name");
                        }
                    }
                }
            }
            if updated > 0 {
                broadcast_chats(&shared);
            }
            info!(updated, "synchronized WhatsApp business profile names");
        }
        Err(error) => warn!(%error, "could not synchronize WhatsApp business profile names"),
    }
}

pub(crate) async fn refresh_avatar(
    shared: Arc<Shared>,
    transport: Arc<dyn Transport>,
    jid: Jid,
    force: bool,
) {
    let raw_jid = jid.to_non_ad_string();
    if force {
        assets::remove_avatar(&shared.avatar_dir, &raw_jid);
    } else if assets::avatar_path(&shared.avatar_dir, &raw_jid).exists()
        || assets::avatar_missing_path(&shared.avatar_dir, &raw_jid).exists()
    {
        return;
    }
    match assets::fetch_avatar(transport, shared.avatar_dir.clone(), jid).await {
        Ok(changed) => {
            if changed || force {
                shared.avatars_changed();
            }
        }
        Err(error) => warn!(%error, %raw_jid, "could not refresh WhatsApp avatar"),
    }
}

pub(crate) async fn sync_avatars(shared: Arc<Shared>, transport: Arc<dyn Transport>) {
    // Connected can precede the initial history import on a fresh link. Keep
    // sync passes serialized so a pass queued by each history chunk observes
    // the chats imported by the preceding pass without fetching duplicates.
    let _sync_guard = shared.avatar_sync.lock().await;
    let avatar_jids = match shared.database.avatar_jids(AVATAR_SYNC_LIMIT) {
        Ok(jids) => jids,
        Err(error) => {
            warn!(%error, "could not select WhatsApp avatars to synchronize");
            return;
        }
    };
    let jids = avatar_jids
        .into_iter()
        .filter_map(|raw| raw.parse::<Jid>().ok())
        .filter(|jid| !jid.is_status_broadcast() && !jid.is_newsletter())
        .filter(|jid| {
            let raw = jid.to_non_ad_string();
            !assets::avatar_path(&shared.avatar_dir, &raw).exists()
                && !assets::avatar_missing_path(&shared.avatar_dir, &raw).exists()
        })
        .collect::<Vec<_>>();
    let total = jids.len();
    futures::stream::iter(jids.into_iter().map(|jid| {
        let shared = Arc::clone(&shared);
        let transport = Arc::clone(&transport);
        async move { refresh_avatar(shared, transport, jid, false).await }
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    info!(total, "completed bounded WhatsApp avatar sync");
}

pub(crate) async fn backfill_video_previews(shared: Arc<Shared>) {
    let media_dir = shared.media_dir.clone();
    let outcome =
        tokio::task::spawn_blocking(move || assets::backfill_message_video_thumbnails(&media_dir))
            .await;
    apply_video_preview_backfill(&shared, outcome);
}

/// Reacts to one preview-backfill run. It is separate from the worker above so
/// that the outcomes a hermetic test cannot produce — generating a preview
/// needs `ffmpeg`, and a join error needs a worker that dies — stay decided by
/// measured code instead of by the process boundary.
fn apply_video_preview_backfill(
    shared: &Shared,
    outcome: std::result::Result<Result<usize>, tokio::task::JoinError>,
) {
    match outcome {
        Ok(Ok(generated)) => {
            info!(generated, "generated missing video previews");
            if generated > 0 {
                for chat_jid in shared.connection_state().active_chats {
                    broadcast_messages(shared, &chat_jid);
                }
            }
        }
        Ok(Err(error)) => warn!(%error, "could not scan cached videos for missing previews"),
        Err(error) => warn!(%error, "video preview worker panicked"),
    }
}

pub(crate) async fn request_missing_contact_history(
    shared: Arc<Shared>,
    transport: Arc<dyn Transport>,
) {
    if shared.contact_history_marker.exists() {
        return;
    }

    let cursors = match shared.database.unresolved_contact_history_cursors(24) {
        Ok(cursors) => cursors,
        Err(error) => {
            warn!(%error, "could not select unresolved chats for history recovery");
            return;
        }
    };
    let requested = cursors.len();
    if requested == 0 {
        // On a fresh link, Connected can arrive before the first history chunk.
        // Do not consume the one-time recovery until at least one real chat is
        // present; the HistorySync handler will retry after importing it.
        match shared.database.list_chats(1) {
            Ok(chats) if !chats.is_empty() => {
                if let Err(error) = write_private_marker(&shared.contact_history_marker) {
                    warn!(%error, "could not mark contact history recovery complete");
                }
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "could not inspect chats after contact history recovery");
            }
        }
        return;
    }
    let mut queued = 0usize;
    for cursor in cursors {
        let Ok(jid) = cursor.chat_jid.parse::<Jid>() else {
            warn!(jid = %cursor.chat_jid, "could not parse chat for history recovery");
            continue;
        };
        match transport
            .fetch_message_history(
                &jid,
                &cursor.message_id,
                cursor.from_me,
                cursor.timestamp_ms,
                25,
            )
            .await
        {
            Ok(_) => queued += 1,
            Err(error) => {
                warn!(%error, jid = %cursor.chat_jid, "could not request WhatsApp history for contact-name recovery");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    if queued == requested
        && let Err(error) = write_private_marker(&shared.contact_history_marker)
    {
        warn!(%error, "could not mark contact history recovery complete");
    }
    info!(
        requested,
        queued, "requested one-time WhatsApp history for missing contact names"
    );
}

pub(crate) fn prepare_contact_name_resync(protocol_db: &Path, marker: &Path) -> Result<()> {
    if marker.exists() || !protocol_db.exists() {
        return Ok(());
    }

    let mut connection = rusqlite::Connection::open(protocol_db)
        .with_context(|| format!("opening session database at {}", protocol_db.display()))?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM app_state_mutation_macs
         WHERE name IN ('critical_block', 'critical_unblock_low')",
        [],
    )?;
    transaction.execute(
        "DELETE FROM app_state_versions
         WHERE name IN ('critical_block', 'critical_unblock_low')",
        [],
    )?;
    // whatsapp-rust enters its critical bootstrap when the persisted push name
    // is empty. critical_block restores that name while critical_unblock_low
    // replays the address-book ContactUpdate events we need.
    transaction.execute("UPDATE device SET push_name = ''", [])?;
    transaction.commit()?;
    info!("scheduled one-time WhatsApp contact-name resync");
    Ok(())
}

pub(crate) fn prepare_event_state_resync(protocol_db: &Path, marker: &Path) -> Result<()> {
    if marker.exists() || !protocol_db.exists() {
        return Ok(());
    }
    let mut connection = rusqlite::Connection::open(protocol_db)
        .with_context(|| format!("opening session database at {}", protocol_db.display()))?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM app_state_mutation_macs
         WHERE name IN ('critical_block', 'regular', 'regular_low', 'regular_high')",
        [],
    )?;
    transaction.execute(
        "DELETE FROM app_state_versions
         WHERE name IN ('critical_block', 'regular', 'regular_low', 'regular_high')",
        [],
    )?;
    // A linked device only schedules all non-critical collections during its
    // bootstrap path. Clearing their versions is not sufficient on an already
    // paired session; an empty push name safely re-enters that path. Reset
    // critical_block too so its setting_pushName mutation restores the name
    // before the regular collections are fetched.
    transaction.execute("UPDATE device SET push_name = ''", [])?;
    transaction.commit()?;
    info!("scheduled one-time WhatsApp chat-state event resync");
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::test_support::test_shared;
    use crate::transport::ContactInfo;
    use crate::transport::fake::{Call, CallKind, FakeTransport, transport};
    use omarchy_whatsapp_protocol::{Chat, Message};
    use std::os::unix::fs::PermissionsExt;

    fn chat(jid: &str, name: &str, is_group: bool) -> Chat {
        Chat {
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
        }
    }

    fn message(chat_jid: &str, id: &str) -> Message {
        Message {
            id: id.to_owned(),
            chat_jid: chat_jid.to_owned(),
            sender_jid: chat_jid.to_owned(),
            sender_name: chat_jid.to_owned(),
            text: "synthetic".into(),
            timestamp: 100,
            from_me: false,
            receipt: 0,
            delivered_at: None,
            read_at: None,
            delivered_to: Vec::new(),
            read_by: Vec::new(),
            media: None,
            reactions: Vec::new(),
        }
    }

    fn seed(shared: &Shared, chat: &Chat, messages: &[Message]) {
        shared
            .database
            .insert_history_conversation(chat, messages)
            .unwrap();
    }

    fn subject(name: &str) -> whatsapp_rust::GroupMetadata {
        whatsapp_rust::GroupMetadata {
            subject: name.to_owned(),
            ..whatsapp_rust::GroupMetadata::default()
        }
    }

    fn chat_names(shared: &Shared) -> Vec<(String, String)> {
        let mut names = shared
            .database
            .list_chats(20)
            .unwrap()
            .into_iter()
            .map(|chat| (chat.jid, chat.name))
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[tokio::test]
    async fn group_subjects_come_from_participation_and_are_recovered_per_chat() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        for jid in [
            "123-456@g.us",
            "999-999@g.us",
            "888-888@g.us",
            "777-777@g.us",
            "555-555@g.us",
        ] {
            seed(&shared, &chat(jid, jid, true), &[]);
        }
        shared
            .database
            .execute_test_sql("INSERT INTO chats (jid, name, is_group) VALUES ('broken', '', 1)")
            .unwrap();
        let fake = Arc::new(
            FakeTransport::new()
                .with_participating_group("123-456@g.us", subject("Garden"))
                .with_group_metadata("999-999@g.us", subject("Recovered"))
                .with_group_metadata("888-888@g.us", subject("   "))
                // A group WhatsApp names after its own identity stays
                // unresolved, so the next pass re-reads and re-stores it.
                .with_group_metadata("555-555@g.us", subject("555-555@g.us")),
        );

        sync_group_names(Arc::clone(&shared), transport(&fake)).await;

        assert_eq!(
            chat_names(&shared),
            vec![
                ("123-456@g.us".to_owned(), "Garden".to_owned()),
                ("555-555@g.us".to_owned(), "555-555@g.us".to_owned()),
                ("777-777@g.us".to_owned(), String::new()),
                ("888-888@g.us".to_owned(), String::new()),
                ("999-999@g.us".to_owned(), "Recovered".to_owned()),
                ("broken".to_owned(), String::new()),
            ]
        );
        // The unparseable stored identity is never asked about.
        assert!(
            !fake
                .calls_of(CallKind::GroupMetadata)
                .contains(&Call::GroupMetadata("broken".into()))
        );

        // A repeat pass re-reads the still-unresolved subjects and stores none
        // of them again.
        fake.clear_calls();
        sync_group_names(Arc::clone(&shared), transport(&fake)).await;
        assert_eq!(
            fake.calls_of(CallKind::ParticipatingGroups),
            vec![Call::ParticipatingGroups]
        );
        assert!(
            fake.calls_of(CallKind::GroupMetadata)
                .contains(&Call::GroupMetadata("555-555@g.us".into()))
        );

        // Persisting failures on either path are logged, not fatal.
        shared
            .database
            .execute_test_sql("PRAGMA query_only = ON")
            .unwrap();
        sync_group_names(Arc::clone(&shared), transport(&fake)).await;
        shared
            .database
            .execute_test_sql("PRAGMA query_only = OFF")
            .unwrap();
    }

    #[tokio::test]
    async fn group_subject_synchronization_degrades_on_every_failure() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let failing = Arc::new(
            FakeTransport::new().failing(CallKind::ParticipatingGroups, "WhatsApp is offline"),
        );

        sync_group_names(Arc::clone(&shared), transport(&failing)).await;
        assert_eq!(
            failing.calls(),
            vec![Call::ParticipatingGroups],
            "a failed participation query ends the pass"
        );

        let fake = Arc::new(FakeTransport::new());
        shared
            .database
            .execute_test_sql("DROP TABLE chats")
            .unwrap();
        sync_group_names(Arc::clone(&shared), transport(&fake)).await;
        assert!(fake.calls_of(CallKind::GroupMetadata).is_empty());
    }

    #[tokio::test]
    async fn business_profile_names_fill_in_unresolved_direct_chats() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let business = "31600000001@s.whatsapp.net";
        let business_lid = "100000000000001@lid";
        let anonymous = "100000012345678@lid";
        for jid in [business, anonymous, "0@s.whatsapp.net"] {
            seed(&shared, &chat(jid, jid, false), &[]);
        }
        shared
            .database
            .execute_test_sql("INSERT INTO chats (jid, name) VALUES ('broken', '')")
            .unwrap();
        let fake = Arc::new(
            FakeTransport::new()
                .with_user_info(ContactInfo {
                    jid: business.parse().unwrap(),
                    lid: Some(business_lid.parse().unwrap()),
                    verified_name: Some("Ada's Flowers".into()),
                })
                .with_user_info(ContactInfo {
                    jid: anonymous.parse().unwrap(),
                    lid: None,
                    verified_name: None,
                }),
        );
        // The business's LID already carries an address-book name, which
        // outranks a verified profile name and is therefore left alone.
        shared
            .database
            .update_address_book_name(business_lid, "Ada")
            .unwrap();

        // A read-only database logs instead of aborting the pass.
        shared
            .database
            .execute_test_sql("PRAGMA query_only = ON")
            .unwrap();
        sync_missing_contact_names(Arc::clone(&shared), transport(&fake)).await;
        shared
            .database
            .execute_test_sql("PRAGMA query_only = OFF")
            .unwrap();
        assert!(shared.database.contact_name(business).unwrap().is_none());

        sync_missing_contact_names(Arc::clone(&shared), transport(&fake)).await;

        let queried = match &fake.calls_of(CallKind::UserInfo)[..] {
            [Call::UserInfo(jids), Call::UserInfo(_)] => {
                let mut jids = jids.clone();
                jids.sort();
                jids
            }
            other => panic!("unexpected calls {other:?}"),
        };
        assert_eq!(
            queried,
            vec![anonymous.to_owned(), business.to_owned()],
            "the placeholder and unparseable identities are filtered out"
        );
        assert_eq!(
            shared.database.contact_name(business).unwrap().as_deref(),
            Some("Ada's Flowers")
        );
        assert_eq!(
            shared
                .database
                .contact_name(business_lid)
                .unwrap()
                .as_deref(),
            Some("Ada"),
            "the address book keeps precedence over the verified name"
        );
        assert!(shared.database.contact_name(anonymous).unwrap().is_none());

        fake.fail(CallKind::UserInfo, "WhatsApp is offline");
        sync_missing_contact_names(Arc::clone(&shared), transport(&fake)).await;
        assert_eq!(fake.calls_of(CallKind::UserInfo).len(), 3);
    }

    #[tokio::test]
    async fn contact_name_synchronization_stops_before_asking_for_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let fake = Arc::new(FakeTransport::new());

        // Nothing unresolved: the batch query is never issued.
        sync_missing_contact_names(Arc::clone(&shared), transport(&fake)).await;
        assert!(fake.calls().is_empty());

        shared
            .database
            .execute_test_sql("DROP TABLE chats")
            .unwrap();
        sync_missing_contact_names(Arc::clone(&shared), transport(&fake)).await;
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn avatar_refresh_respects_the_cache_the_marker_and_a_forced_retry() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        assets::private_dir(&shared.avatar_dir).unwrap();
        let raw = "31600000000@s.whatsapp.net";
        let jid: Jid = raw.parse().unwrap();
        let fake = Arc::new(FakeTransport::new().with_profile_picture(raw, None));

        assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, raw), b"avatar")
            .unwrap();
        refresh_avatar(Arc::clone(&shared), transport(&fake), jid.clone(), false).await;
        assert!(
            fake.calls().is_empty(),
            "a cached avatar is never re-fetched"
        );

        // Forcing drops the cache and records that WhatsApp has no picture.
        refresh_avatar(Arc::clone(&shared), transport(&fake), jid.clone(), true).await;
        assert_eq!(
            fake.calls_of(CallKind::ProfilePicture),
            vec![Call::ProfilePicture(raw.into())]
        );
        assert!(!assets::avatar_path(&shared.avatar_dir, raw).exists());
        assert!(assets::avatar_missing_path(&shared.avatar_dir, raw).exists());

        // The missing marker keeps unforced passes from asking again.
        fake.clear_calls();
        refresh_avatar(Arc::clone(&shared), transport(&fake), jid.clone(), false).await;
        assert!(fake.calls().is_empty());

        // A failed lookup is logged and leaves no marker behind.
        fake.fail(CallKind::ProfilePicture, "WhatsApp is offline");
        refresh_avatar(Arc::clone(&shared), transport(&fake), jid, true).await;
        assert_eq!(fake.calls_of(CallKind::ProfilePicture).len(), 1);
        assert!(!assets::avatar_missing_path(&shared.avatar_dir, raw).exists());
    }

    #[tokio::test]
    async fn the_bounded_avatar_sync_only_fetches_uncached_contact_identities() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        assets::private_dir(&shared.avatar_dir).unwrap();
        let wanted = "31600000001@s.whatsapp.net";
        let cached = "31600000002@s.whatsapp.net";
        let known_missing = "31600000003@s.whatsapp.net";
        for jid in [wanted, cached, known_missing] {
            seed(&shared, &chat(jid, jid, false), &[]);
        }
        shared
            .database
            .execute_test_sql(
                "INSERT INTO chats (jid, name) VALUES
                   ('broken', ''), ('status@broadcast', ''), ('1@newsletter', '')",
            )
            .unwrap();
        assets::write_private_bytes(&assets::avatar_path(&shared.avatar_dir, cached), b"avatar")
            .unwrap();
        assets::write_private_bytes(
            &assets::avatar_missing_path(&shared.avatar_dir, known_missing),
            b"none\n",
        )
        .unwrap();
        let fake = Arc::new(FakeTransport::new().with_profile_picture(wanted, None));

        sync_avatars(Arc::clone(&shared), transport(&fake)).await;

        assert_eq!(
            fake.calls_of(CallKind::ProfilePicture),
            vec![Call::ProfilePicture(wanted.into())]
        );

        shared
            .database
            .execute_test_sql("DROP TABLE chats; DROP TABLE messages;")
            .unwrap();
        fake.clear_calls();
        sync_avatars(Arc::clone(&shared), transport(&fake)).await;
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn video_preview_backfill_only_reloads_chats_when_it_produced_something() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(test_shared(&directory));
        let connection = shared.open_connection();
        shared
            .set_connection_active_chat(connection, Some("31600000000@s.whatsapp.net".into()))
            .unwrap();
        let mut events = shared.events.subscribe();

        apply_video_preview_backfill(&shared, Ok(Ok(0)));
        assert!(events.try_recv().is_err());

        apply_video_preview_backfill(&shared, Ok(Ok(2)));
        assert!(matches!(
            events.try_recv().unwrap().event,
            omarchy_whatsapp_protocol::ServerEvent::Invalidated {
                resource: omarchy_whatsapp_protocol::Resource::Messages,
                ..
            }
        ));

        apply_video_preview_backfill(&shared, Ok(Err(anyhow::anyhow!("cache is unreadable"))));
        let aborted = tokio::spawn(std::future::pending::<()>());
        aborted.abort();
        apply_video_preview_backfill(&shared, Err(aborted.await.unwrap_err()));
        assert!(events.try_recv().is_err());

        // The worker itself runs against the real media directory.
        backfill_video_previews(Arc::clone(&shared)).await;
        assets::private_dir(&shared.media_dir).unwrap();
        backfill_video_previews(Arc::clone(&shared)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn one_time_history_recovery_is_consumed_only_once_every_chat_is_queued() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = test_shared(&directory);
        shared.contact_history_marker = directory.path().join("markers/contact-history");
        let shared = Arc::new(shared);
        let first = "31600000001@s.whatsapp.net";
        let second = "31600000002@s.whatsapp.net";
        seed(&shared, &chat(first, first, false), &[message(first, "M1")]);
        seed(
            &shared,
            &chat(second, second, false),
            &[message(second, "M2")],
        );
        shared
            .database
            .execute_test_sql(
                "INSERT INTO chats (jid, name) VALUES ('broken', '');
                 INSERT INTO messages
                   (chat_jid, id, sender_jid, sender_name, text, timestamp, from_me)
                 VALUES ('broken', 'M3', 'broken', '', 'x', 1, 0);",
            )
            .unwrap();
        let fake = Arc::new(
            FakeTransport::new()
                .with_history_request_id("request-1")
                .failing(CallKind::FetchMessageHistory, "WhatsApp is offline"),
        );

        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert_eq!(fake.calls_of(CallKind::FetchMessageHistory).len(), 2);
        assert!(
            !shared.contact_history_marker.exists(),
            "a partially queued recovery stays armed"
        );

        fake.succeed(CallKind::FetchMessageHistory);
        fake.clear_calls();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        let requested = fake
            .calls_of(CallKind::FetchMessageHistory)
            .into_iter()
            .map(|call| match call {
                Call::FetchMessageHistory {
                    chat,
                    oldest_message_id,
                    count,
                    ..
                } => (chat, oldest_message_id, count),
                other => panic!("unexpected call {other:?}"),
            })
            .collect::<Vec<_>>();
        let mut requested = requested;
        requested.sort();
        assert_eq!(
            requested,
            vec![
                (first.to_owned(), "M1".to_owned(), 25),
                (second.to_owned(), "M2".to_owned(), 25),
            ]
        );
        assert!(
            !shared.contact_history_marker.exists(),
            "an unparseable chat leaves one request unqueued"
        );

        shared
            .database
            .execute_test_sql("DELETE FROM messages WHERE chat_jid = 'broken'")
            .unwrap();
        fake.clear_calls();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert_eq!(fake.calls_of(CallKind::FetchMessageHistory).len(), 2);
        assert!(
            !shared.contact_history_marker.exists(),
            "an unwritable marker is logged and leaves the recovery armed"
        );

        std::fs::create_dir(directory.path().join("markers")).unwrap();
        fake.clear_calls();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert_eq!(fake.calls_of(CallKind::FetchMessageHistory).len(), 2);
        assert!(shared.contact_history_marker.exists());

        // The marker makes every later pass a no-op.
        fake.clear_calls();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert!(fake.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn history_recovery_waits_for_a_chat_before_consuming_its_one_shot() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = test_shared(&directory);
        shared.contact_history_marker = directory.path().join("missing/contact-history");
        let shared = Arc::new(shared);
        let fake = Arc::new(FakeTransport::new());

        // A fresh link has no chats yet, so the recovery stays armed.
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert!(!shared.contact_history_marker.exists());

        // With a resolved chat present there is nothing to request, but the
        // marker directory is unwritable, which is logged rather than fatal.
        let resolved = "31600000004@s.whatsapp.net";
        seed(&shared, &chat(resolved, "Ada", false), &[]);
        shared
            .database
            .execute_test_sql(
                "UPDATE chats SET name = 'Ada', name_source = 40 WHERE jid = '31600000004@s.whatsapp.net'",
            )
            .unwrap();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert!(!shared.contact_history_marker.exists());

        // Once the marker can be written the one-shot is finally consumed.
        std::fs::create_dir(directory.path().join("missing")).unwrap();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert!(shared.contact_history_marker.exists());
        std::fs::remove_file(&shared.contact_history_marker).unwrap();

        // A broken chat listing is logged as well.
        shared
            .database
            .execute_test_sql("DROP TABLE chat_settings")
            .unwrap();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;

        shared
            .database
            .execute_test_sql("DROP TABLE chats")
            .unwrap();
        request_missing_contact_history(Arc::clone(&shared), transport(&fake)).await;
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn contact_resync_only_resets_the_contact_collection_once() {
        let directory = tempfile::tempdir().unwrap();
        let protocol_db = directory.path().join("session.db");
        let marker = directory.path().join("contact-names-v2");
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_state_versions (name TEXT PRIMARY KEY, state_data BLOB);
                 CREATE TABLE app_state_mutation_macs (
                   name TEXT NOT NULL, version INTEGER NOT NULL,
                   index_mac BLOB NOT NULL, value_mac BLOB NOT NULL
                 );
                 CREATE TABLE device (push_name TEXT NOT NULL);
                 INSERT INTO device VALUES ('Own profile name');
                 INSERT INTO app_state_versions VALUES ('critical_block', X'00');
                 INSERT INTO app_state_versions VALUES ('critical_unblock_low', X'01');
                 INSERT INTO app_state_versions VALUES ('regular', X'02');
                 INSERT INTO app_state_mutation_macs VALUES
                   ('critical_block', 1, X'00', X'01');
                 INSERT INTO app_state_mutation_macs VALUES
                   ('critical_unblock_low', 1, X'01', X'02');
                 INSERT INTO app_state_mutation_macs VALUES
                   ('regular', 1, X'03', X'04');",
            )
            .unwrap();
        drop(connection);

        prepare_contact_name_resync(&protocol_db, &marker).unwrap();

        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        let critical_versions: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions
                 WHERE name IN ('critical_block', 'critical_unblock_low')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let regular_versions: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name = 'regular'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(critical_versions, 0);
        assert_eq!(regular_versions, 1);
        let push_name: String = connection
            .query_row("SELECT push_name FROM device", [], |row| row.get(0))
            .unwrap();
        assert!(push_name.is_empty());
        assert!(!marker.exists());

        connection
            .execute(
                "INSERT INTO app_state_versions VALUES ('critical_unblock_low', X'03')",
                [],
            )
            .unwrap();
        drop(connection);
        write_private_marker(&marker).unwrap();
        assert_eq!(
            std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        prepare_contact_name_resync(&protocol_db, &marker).unwrap();
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        let preserved: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name = 'critical_unblock_low'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved, 1);
    }

    #[test]
    fn event_resync_resets_bootstrap_and_regular_collections() {
        let directory = tempfile::tempdir().unwrap();
        let protocol_db = directory.path().join("session.db");
        let marker = directory.path().join("event-state-v6");
        prepare_event_state_resync(&directory.path().join("missing.db"), &marker).unwrap();
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_state_versions (name TEXT PRIMARY KEY, state_data BLOB);
                 CREATE TABLE app_state_mutation_macs (
                   name TEXT NOT NULL, version INTEGER NOT NULL,
                   index_mac BLOB NOT NULL, value_mac BLOB NOT NULL
                 );
                 CREATE TABLE device (push_name TEXT NOT NULL);
                 INSERT INTO device VALUES ('Own profile name');
                 INSERT INTO app_state_versions VALUES ('critical_block', X'00');
                 INSERT INTO app_state_versions VALUES ('regular', X'01');
                 INSERT INTO app_state_versions VALUES ('regular_low', X'02');
                 INSERT INTO app_state_versions VALUES ('regular_high', X'03');
                 INSERT INTO app_state_mutation_macs VALUES ('critical_block', 1, X'00', X'01');
                 INSERT INTO app_state_mutation_macs VALUES ('regular_low', 1, X'01', X'02');",
            )
            .unwrap();
        drop(connection);

        prepare_event_state_resync(&protocol_db, &marker).unwrap();
        let connection = rusqlite::Connection::open(&protocol_db).unwrap();
        let regular: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name LIKE 'regular%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let critical: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_state_versions WHERE name = 'critical_block'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(regular, 0);
        assert_eq!(critical, 0);
        let push_name: String = connection
            .query_row("SELECT push_name FROM device", [], |row| row.get(0))
            .unwrap();
        assert!(push_name.is_empty());
    }
}
