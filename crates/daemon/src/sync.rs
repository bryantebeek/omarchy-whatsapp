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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn backfill_video_previews(shared: Arc<Shared>) {
    let media_dir = shared.media_dir.clone();
    match tokio::task::spawn_blocking(move || assets::backfill_message_video_thumbnails(&media_dir))
        .await
    {
        Ok(Ok(generated)) => {
            info!(generated, "generated missing video previews");
            if generated > 0 {
                for chat_jid in shared.connection_state().active_chats {
                    broadcast_messages(&shared, &chat_jid);
                }
            }
        }
        Ok(Err(error)) => warn!(%error, "could not scan cached videos for missing previews"),
        Err(error) => warn!(%error, "video preview worker panicked"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
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
    use std::os::unix::fs::PermissionsExt;

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
