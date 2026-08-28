use anyhow::{Context, Result};
use omarchy_whatsapp_protocol::{Chat, Message, MessageMedia, Reaction};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const CHAT_NAME_UNKNOWN: i64 = 0;
const CHAT_NAME_HISTORY: i64 = 10;
const CHAT_NAME_MESSAGE: i64 = 20;
const CHAT_NAME_GROUP_METADATA: i64 = 30;
const CHAT_NAME_ADDRESS_BOOK: i64 = 40;

pub struct Database {
    connection: Mutex<Connection>,
    protocol_db: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UnreadReceipt {
    pub message_id: String,
    pub sender_jid: String,
    pub is_group: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCursor {
    pub chat_jid: String,
    pub message_id: String,
    pub sender_jid: String,
    pub from_me: bool,
    pub timestamp_ms: i64,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("opening history database at {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS chats (
                jid            TEXT PRIMARY KEY,
                name           TEXT NOT NULL,
                name_source    INTEGER NOT NULL DEFAULT 0,
                phone_number   TEXT,
                last_message   TEXT NOT NULL DEFAULT '',
                last_timestamp INTEGER NOT NULL DEFAULT 0,
                unread         INTEGER NOT NULL DEFAULT 0,
                is_group       INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS messages (
                chat_jid    TEXT NOT NULL,
                id          TEXT NOT NULL,
                sender_jid  TEXT NOT NULL,
                sender_name TEXT NOT NULL,
                text        TEXT NOT NULL,
                timestamp   INTEGER NOT NULL,
                from_me     INTEGER NOT NULL,
                read        INTEGER NOT NULL DEFAULT 0,
                starred     INTEGER NOT NULL DEFAULT 0,
                receipt     INTEGER NOT NULL DEFAULT 0,
                media_json      TEXT,
                media_download  BLOB,
                PRIMARY KEY (chat_jid, id),
                FOREIGN KEY (chat_jid) REFERENCES chats(jid) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS messages_by_chat_time
                ON messages(chat_jid, timestamp DESC);
            CREATE INDEX IF NOT EXISTS messages_by_sender
                ON messages(sender_jid);
            CREATE TABLE IF NOT EXISTS contacts (
                jid    TEXT PRIMARY KEY,
                name   TEXT NOT NULL,
                source INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS chat_settings (
                jid                       TEXT PRIMARY KEY,
                pinned                    INTEGER,
                archived                  INTEGER,
                muted                     INTEGER,
                mute_end                  INTEGER,
                read_state                INTEGER,
                status_muted              INTEGER,
                disappearing_duration     INTEGER,
                disappearing_updated_at   INTEGER,
                deleted                    INTEGER NOT NULL DEFAULT 0,
                cleared_at                 INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS message_tombstones (
                chat_jid  TEXT NOT NULL,
                id        TEXT NOT NULL,
                PRIMARY KEY (chat_jid, id)
            );
            CREATE TABLE IF NOT EXISTS reactions (
                chat_jid    TEXT NOT NULL,
                message_id  TEXT NOT NULL,
                reactor_jid TEXT NOT NULL,
                emoji       TEXT NOT NULL,
                from_me     INTEGER NOT NULL DEFAULT 0,
                timestamp   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (chat_jid, message_id, reactor_jid)
            );
            CREATE INDEX IF NOT EXISTS reactions_by_chat_message
                ON reactions(chat_jid, message_id);
            CREATE INDEX IF NOT EXISTS reactions_by_reactor
                ON reactions(reactor_jid);
            CREATE TABLE IF NOT EXISTS labels (
                id      TEXT PRIMARY KEY,
                name    TEXT NOT NULL DEFAULT '',
                color   INTEGER,
                deleted INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS chat_labels (
                chat_jid TEXT NOT NULL,
                label_id TEXT NOT NULL,
                PRIMARY KEY (chat_jid, label_id)
            );
            ",
        )?;
        // Forward-compatible migration for databases created by early builds.
        let _ = connection.execute(
            "ALTER TABLE messages ADD COLUMN read INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = connection.execute(
            "ALTER TABLE contacts ADD COLUMN source INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = connection.execute(
            "ALTER TABLE messages ADD COLUMN starred INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = connection.execute(
            "ALTER TABLE messages ADD COLUMN receipt INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = connection.execute("ALTER TABLE messages ADD COLUMN media_json TEXT", []);
        let _ = connection.execute("ALTER TABLE messages ADD COLUMN media_download BLOB", []);
        let _ = connection.execute("ALTER TABLE chats ADD COLUMN phone_number TEXT", []);
        let _ = connection.execute(
            "ALTER TABLE chats ADD COLUMN name_source INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = connection.execute(
            "ALTER TABLE chat_settings ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = connection.execute(
            "ALTER TABLE chat_settings ADD COLUMN cleared_at INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // A JID is an identifier, never a chat name. Legacy databases stored
        // it in `name` as a rendering fallback, which made later syncs unable
        // to distinguish missing metadata from a real title.
        connection.execute(
            "UPDATE chats SET name = '', name_source = ?1
             WHERE TRIM(name) = '' OR name = jid",
            [CHAT_NAME_UNKNOWN],
        )?;
        connection.execute(
            "UPDATE chats SET name_source = ?1
             WHERE name_source = ?2 AND name != ''",
            params![CHAT_NAME_HISTORY, CHAT_NAME_UNKNOWN],
        )?;
        connection.execute(
            "UPDATE chats SET
                name = contacts.name,
                name_source = CASE contacts.source WHEN 1 THEN ?1 ELSE ?2 END
             FROM contacts
             WHERE chats.jid = contacts.jid AND chats.is_group = 0
               AND (chats.name_source = ?3 OR chats.name = contacts.name)",
            params![CHAT_NAME_ADDRESS_BOOK, CHAT_NAME_MESSAGE, CHAT_NAME_UNKNOWN],
        )?;
        // Address-book names are authoritative for every rendering path. Older
        // builds could store a newer WhatsApp push name on individual messages
        // even while preserving the saved contact name in `contacts`.
        connection.execute(
            "UPDATE messages SET sender_name = (
                SELECT contacts.name FROM contacts
                WHERE contacts.jid = messages.sender_jid AND contacts.source = 1
             )
             WHERE EXISTS (
                SELECT 1 FROM contacts
                WHERE contacts.jid = messages.sender_jid AND contacts.source = 1
                  AND contacts.name != messages.sender_name
             )",
            [],
        )?;
        Self::restore_legacy_chat_names(&connection, path);
        // Early builds represented protocol/control envelopes and unknown
        // add-ons as user-visible placeholder bubbles. They contain no
        // recoverable user content and must not survive after renderability is
        // classified before insertion.
        connection.execute(
            "DELETE FROM messages
             WHERE text = '[Unsupported message]' AND media_json IS NULL",
            [],
        )?;
        connection.execute(
            "UPDATE chats SET
                last_message = COALESCE((
                    SELECT text FROM messages
                    WHERE messages.chat_jid = chats.jid
                    ORDER BY timestamp DESC LIMIT 1
                ), ''),
                last_timestamp = COALESCE((
                    SELECT timestamp FROM messages
                    WHERE messages.chat_jid = chats.jid
                    ORDER BY timestamp DESC LIMIT 1
                ), 0)
             WHERE last_message = '[Unsupported message]'",
            [],
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
            protocol_db: path.with_file_name("session.db"),
        })
    }

    pub fn clear_account_data(&self) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "
            DELETE FROM reactions;
            DELETE FROM message_tombstones;
            DELETE FROM messages;
            DELETE FROM chat_labels;
            DELETE FROM chat_settings;
            DELETE FROM chats;
            DELETE FROM contacts;
            DELETE FROM labels;
            ",
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn restore_legacy_chat_names(connection: &Connection, database_path: &Path) {
        let Some(state_dir) = database_path.parent() else {
            return;
        };
        let Ok(contents) = fs::read(state_dir.join("store.json")) else {
            return;
        };
        let Ok(store) = serde_json::from_slice::<serde_json::Value>(&contents) else {
            return;
        };
        let Some(chats) = store.get("chats").and_then(serde_json::Value::as_array) else {
            return;
        };
        for chat in chats {
            let Some(jid) = chat.get("jid").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(name) = chat
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty() && *name != "Group" && *name != jid)
            else {
                continue;
            };
            let _ = connection.execute(
                "UPDATE chats SET name = ?2, name_source = ?3
                 WHERE jid = ?1 AND name_source = ?4",
                params![jid, name, CHAT_NAME_HISTORY, CHAT_NAME_UNKNOWN],
            );
        }
    }

    pub fn insert_message(
        &self,
        message: &Message,
        chat_name: &str,
        is_group: bool,
        increment_unread: bool,
    ) -> Result<bool> {
        self.insert_message_inner(message, chat_name, is_group, increment_unread, false)
    }

    pub fn insert_history_message(
        &self,
        message: &Message,
        chat_name: &str,
        is_group: bool,
    ) -> Result<bool> {
        self.insert_message_inner(message, chat_name, is_group, false, true)
    }

    fn insert_message_inner(
        &self,
        message: &Message,
        chat_name: &str,
        is_group: bool,
        increment_unread: bool,
        from_history: bool,
    ) -> Result<bool> {
        let candidate = chat_name.trim();
        let (candidate, name_source) = if candidate.is_empty() || candidate == message.chat_jid {
            ("", CHAT_NAME_UNKNOWN)
        } else if from_history {
            (candidate, CHAT_NAME_HISTORY)
        } else {
            (candidate, CHAT_NAME_MESSAGE)
        };
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.transaction()?;
        let tombstoned = transaction
            .query_row(
                "SELECT 1 FROM message_tombstones WHERE chat_jid = ?1 AND id = ?2",
                params![message.chat_jid, message.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let history_suppressed = from_history
            && transaction
                .query_row(
                    "SELECT deleted, cleared_at FROM chat_settings WHERE jid = ?1",
                    [&message.chat_jid],
                    |row| Ok(row.get::<_, bool>(0)? || message.timestamp <= row.get::<_, i64>(1)?),
                )
                .optional()?
                .unwrap_or(false);
        if tombstoned || history_suppressed {
            transaction.commit()?;
            return Ok(false);
        }
        if !from_history {
            transaction.execute(
                "INSERT INTO chat_settings (jid, deleted) VALUES (?1, 0)
                 ON CONFLICT(jid) DO UPDATE SET deleted = 0",
                [&message.chat_jid],
            )?;
        }
        transaction.execute(
            "INSERT INTO chats
             (jid, name, name_source, last_message, last_timestamp, unread, is_group)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(jid) DO UPDATE SET
                last_message = CASE
                    WHEN excluded.last_timestamp >= chats.last_timestamp
                    THEN excluded.last_message ELSE chats.last_message END,
                last_timestamp = MAX(chats.last_timestamp, excluded.last_timestamp),
                is_group = excluded.is_group",
            params![
                message.chat_jid,
                candidate,
                name_source,
                message.text,
                message.timestamp,
                0,
                is_group,
            ],
        )?;
        let media_json = message
            .media
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO messages
             (chat_jid, id, sender_jid, sender_name, text, timestamp, from_me, read, media_json)
             VALUES (?1, ?2, ?3,
                COALESCE((SELECT name FROM contacts WHERE jid = ?3 AND source = 1), ?4),
                ?5, ?6, ?7, ?8, ?9)",
            params![
                message.chat_jid,
                message.id,
                message.sender_jid,
                message.sender_name,
                message.text,
                message.timestamp,
                message.from_me,
                !increment_unread,
                media_json,
            ],
        )? > 0;
        if inserted && increment_unread {
            transaction.execute(
                "UPDATE chats SET unread = unread + 1 WHERE jid = ?1",
                [&message.chat_jid],
            )?;
            transaction.execute(
                "INSERT INTO chat_settings (jid, read_state) VALUES (?1, 0)
                 ON CONFLICT(jid) DO UPDATE SET read_state = 0",
                [&message.chat_jid],
            )?;
        }
        // Keep history bounded without a background cleaner. Protocol state is in a
        // separate database and is never touched by this retention policy.
        transaction.execute(
            "DELETE FROM messages WHERE chat_jid = ?1 AND rowid NOT IN
             (SELECT rowid FROM messages WHERE chat_jid = ?1 ORDER BY timestamp DESC LIMIT 1000)",
            [&message.chat_jid],
        )?;
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn list_chats(&self, limit: u32) -> Result<Vec<Chat>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT chats.jid, chats.name, chats.phone_number, chats.last_message,
                    COALESCE((
                        SELECT sender_name FROM messages
                        WHERE messages.chat_jid = chats.jid
                        ORDER BY timestamp DESC, rowid DESC LIMIT 1
                    ), ''),
                    chats.last_timestamp,
                    CASE WHEN COALESCE(chat_settings.archived, 0) = 0
                         THEN chats.unread ELSE 0 END,
                    chats.is_group
             FROM chats
             LEFT JOIN chat_settings ON chat_settings.jid = chats.jid
             WHERE chats.jid != '0@s.whatsapp.net'
             ORDER BY COALESCE(chat_settings.archived, 0) ASC,
                      COALESCE(chat_settings.pinned, 0) DESC,
                      last_timestamp DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(limit.clamp(1, 500))], |row| {
            Ok(Chat {
                jid: row.get(0)?,
                name: row.get(1)?,
                phone_number: row.get(2)?,
                last_message: row.get(3)?,
                last_sender_name: row.get(4)?,
                last_timestamp: row.get(5)?,
                unread: row.get(6)?,
                is_group: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_chat_phone_number(&self, jid: &str, phone_number: &str) -> Result<bool> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        Ok(connection.execute(
            "UPDATE chats SET phone_number = ?2
             WHERE jid = ?1 AND phone_number IS NOT ?2",
            params![jid, phone_number],
        )? > 0)
    }

    pub fn direct_chat_jids(&self, limit: u32) -> Result<Vec<String>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT jid FROM chats
             WHERE is_group = 0 AND jid LIKE '%@lid'
             ORDER BY last_timestamp DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(limit.clamp(1, 1000))], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn rewrite_media_paths(&self, replacements: &[(String, String)]) -> Result<()> {
        if replacements.is_empty() {
            return Ok(());
        }
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.transaction()?;
        for (old, new) in replacements {
            transaction.execute(
                "UPDATE messages SET media_json = replace(media_json, ?1, ?2)
                 WHERE media_json LIKE '%' || ?1 || '%'",
                params![old, new],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_history_chat(&self, chat: &Chat) -> Result<()> {
        let candidate = chat.name.trim();
        let (candidate, name_source) = if candidate.is_empty() || candidate == chat.jid {
            ("", CHAT_NAME_UNKNOWN)
        } else {
            (candidate, CHAT_NAME_HISTORY)
        };
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.transaction()?;
        let suppressed = transaction
            .query_row(
                "SELECT deleted FROM chat_settings WHERE jid = ?1",
                [&chat.jid],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        if suppressed {
            transaction.commit()?;
            return Ok(());
        }
        transaction.execute(
            "INSERT INTO chats
             (jid, name, name_source, last_message, last_timestamp, unread, is_group)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(jid) DO UPDATE SET
                name = CASE
                    WHEN chats.name_source = ?8 AND excluded.name_source > ?8
                    THEN excluded.name ELSE chats.name END,
                name_source = MAX(chats.name_source, excluded.name_source),
                last_timestamp = MAX(chats.last_timestamp, excluded.last_timestamp),
                is_group = excluded.is_group",
            params![
                chat.jid,
                candidate,
                name_source,
                chat.last_message,
                chat.last_timestamp,
                chat.unread,
                chat.is_group,
                CHAT_NAME_UNKNOWN,
            ],
        )?;
        transaction.execute(
            "UPDATE chats SET unread = CASE
                 (SELECT read_state FROM chat_settings WHERE jid = chats.jid)
                 WHEN 1 THEN 0
                 WHEN 0 THEN MAX(unread, 1)
                 ELSE unread END
             WHERE jid = ?1",
            [&chat.jid],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_contact_name(&self, jid: &str, name: &str) -> Result<bool> {
        self.update_contact_name_from(jid, name, 0)
    }

    pub fn update_address_book_name(&self, jid: &str, name: &str) -> Result<bool> {
        self.update_contact_name_from(jid, name, 1)
    }

    fn update_contact_name_from(&self, jid: &str, name: &str, source: i64) -> Result<bool> {
        if name.trim().is_empty() {
            return Ok(false);
        }
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.transaction()?;
        let existing_source = transaction
            .query_row("SELECT source FROM contacts WHERE jid = ?1", [jid], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        if existing_source.is_some_and(|existing| existing > source) {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO contacts (jid, name, source) VALUES (?1, ?2, ?3)
             ON CONFLICT(jid) DO UPDATE SET
                name = excluded.name,
                source = excluded.source",
            params![jid, name.trim(), source],
        )?;
        let chat_name_source = if source > 0 {
            CHAT_NAME_ADDRESS_BOOK
        } else {
            CHAT_NAME_MESSAGE
        };
        transaction.execute(
            "UPDATE chats SET name = ?2, name_source = ?3
             WHERE jid = ?1 AND is_group = 0 AND name_source <= ?3",
            params![jid, name.trim(), chat_name_source],
        )?;
        transaction.execute(
            "UPDATE messages SET sender_name = ?2 WHERE sender_jid = ?1",
            params![jid, name.trim()],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn update_group_name(&self, jid: &str, name: &str) -> Result<bool> {
        if name.trim().is_empty() {
            return Ok(false);
        }
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        Ok(connection.execute(
            "UPDATE chats SET name = ?2, name_source = ?3
             WHERE jid = ?1 AND is_group = 1
               AND (name != ?2 OR name_source != ?3)",
            params![jid, name.trim(), CHAT_NAME_GROUP_METADATA],
        )? > 0)
    }

    pub fn contact_name(&self, jid: &str) -> Result<Option<String>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection
            .query_row("SELECT name FROM contacts WHERE jid = ?1", [jid], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn messages(&self, chat_jid: &str, limit: u32) -> Result<Vec<Message>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, chat_jid, sender_jid, sender_name, text, timestamp, from_me, media_json
             FROM (
                SELECT id, chat_jid, sender_jid, sender_name, text, timestamp, from_me, media_json
                FROM messages WHERE chat_jid = ?1
                ORDER BY timestamp DESC LIMIT ?2
             ) ORDER BY timestamp ASC",
        )?;
        let rows =
            statement.query_map(params![chat_jid, i64::from(limit.clamp(1, 1000))], |row| {
                let mut media = row
                    .get::<_, Option<String>>(7)?
                    .and_then(|json| serde_json::from_str(&json).ok());
                match &mut media {
                    Some(MessageMedia::Image {
                        path,
                        thumbnail_path,
                        downloaded,
                        ..
                    }) => {
                        // An empty thumbnail path identifies an ambiguous early
                        // image row whose preview may still occupy `path`.
                        *downloaded = !thumbnail_path.is_empty() && Path::new(path).is_file();
                        if !thumbnail_path.is_empty() && !Path::new(thumbnail_path).is_file() {
                            thumbnail_path.clear();
                        }
                    }
                    Some(MessageMedia::Video {
                        path,
                        thumbnail_path,
                        downloaded,
                        ..
                    }) => {
                        *downloaded = Path::new(path).is_file();
                        if !thumbnail_path.is_empty() && !Path::new(thumbnail_path).is_file() {
                            thumbnail_path.clear();
                        }
                    }
                    Some(MessageMedia::Audio {
                        path, downloaded, ..
                    }) => {
                        *downloaded = Path::new(path).is_file();
                    }
                    _ => {}
                }
                Ok(Message {
                    id: row.get(0)?,
                    chat_jid: row.get(1)?,
                    sender_jid: row.get(2)?,
                    sender_name: row.get(3)?,
                    text: row.get(4)?,
                    timestamp: row.get(5)?,
                    from_me: row.get(6)?,
                    media,
                    reactions: Vec::new(),
                })
            })?;
        let mut messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);

        let mut reactions_by_message: HashMap<String, Vec<Reaction>> = HashMap::new();
        let mut reaction_statement = connection.prepare(
            "SELECT message_id, emoji, COUNT(*), MAX(from_me)
             FROM reactions WHERE chat_jid = ?1
             GROUP BY message_id, emoji
             ORDER BY MIN(timestamp), emoji",
        )?;
        let reaction_rows = reaction_statement.query_map([chat_jid], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Reaction {
                    emoji: row.get(1)?,
                    count: row.get(2)?,
                    from_me: row.get(3)?,
                },
            ))
        })?;
        for row in reaction_rows {
            let (message_id, reaction) = row?;
            reactions_by_message
                .entry(message_id)
                .or_default()
                .push(reaction);
        }
        for message in &mut messages {
            message.reactions = reactions_by_message.remove(&message.id).unwrap_or_default();
        }
        Ok(messages)
    }

    pub fn apply_reaction(
        &self,
        chat_jid: &str,
        message_id: &str,
        reactor_jid: &str,
        emoji: &str,
        from_me: bool,
        timestamp: i64,
    ) -> Result<bool> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        if emoji.is_empty() {
            return Ok(connection.execute(
                "DELETE FROM reactions
                 WHERE chat_jid = ?1 AND message_id = ?2 AND reactor_jid = ?3",
                params![chat_jid, message_id, reactor_jid],
            )? > 0);
        }
        Ok(connection.execute(
            "INSERT INTO reactions
             (chat_jid, message_id, reactor_jid, emoji, from_me, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(chat_jid, message_id, reactor_jid) DO UPDATE SET
                emoji = excluded.emoji,
                from_me = excluded.from_me,
                timestamp = excluded.timestamp
             WHERE reactions.emoji != excluded.emoji
                OR reactions.from_me != excluded.from_me
                OR reactions.timestamp != excluded.timestamp",
            params![chat_jid, message_id, reactor_jid, emoji, from_me, timestamp],
        )? > 0)
    }

    pub fn unread_total(&self) -> Result<u32> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection
            .query_row(
                "SELECT COALESCE(SUM(chats.unread), 0)
                 FROM chats
                 LEFT JOIN chat_settings ON chat_settings.jid = chats.jid
                 WHERE COALESCE(chat_settings.archived, 0) = 0",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn unread_receipts(&self, chat_jid: &str) -> Result<Vec<UnreadReceipt>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let is_group = connection
            .query_row(
                "SELECT is_group FROM chats WHERE jid = ?1",
                [chat_jid],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let mut statement = connection.prepare(
            "SELECT id, sender_jid FROM messages
             WHERE chat_jid = ?1 AND from_me = 0 AND read = 0
             ORDER BY timestamp DESC LIMIT 256",
        )?;
        let rows = statement.query_map([chat_jid], |row| {
            Ok(UnreadReceipt {
                message_id: row.get(0)?,
                sender_jid: row.get(1)?,
                is_group,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn first_unread_message_id(&self, chat_jid: &str) -> Result<Option<String>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection
            .query_row(
                "SELECT id FROM messages
                 WHERE chat_jid = ?1 AND from_me = 0 AND read = 0
                 ORDER BY timestamp ASC, rowid ASC LIMIT 1",
                [chat_jid],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn mark_read(&self, chat_jid: &str) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("UPDATE chats SET unread = 0 WHERE jid = ?1", [chat_jid])?;
        transaction.execute(
            "UPDATE messages SET read = 1 WHERE chat_jid = ?1",
            [chat_jid],
        )?;
        transaction.execute(
            "INSERT INTO chat_settings (jid, read_state) VALUES (?1, 1)
             ON CONFLICT(jid) DO UPDATE SET read_state = 1",
            [chat_jid],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn apply_read_state(&self, chat_jid: &str, read: bool) -> Result<()> {
        if read {
            return self.mark_read(chat_jid);
        }
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO chat_settings (jid, read_state) VALUES (?1, 0)
             ON CONFLICT(jid) DO UPDATE SET read_state = 0",
            [chat_jid],
        )?;
        transaction.execute("UPDATE chats SET unread = 1 WHERE jid = ?1", [chat_jid])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reconcile_unread_after_full_sync(&self) -> Result<u64> {
        if !self.regular_app_state_is_complete()? {
            return Ok(0);
        }
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE chats SET unread = 0
             WHERE unread > 0 AND NOT EXISTS (
                 SELECT 1 FROM chat_settings
                 WHERE chat_settings.jid = chats.jid
                   AND chat_settings.read_state = 0
             )",
            [],
        )?;
        transaction.execute(
            "UPDATE messages SET read = 1
             WHERE chat_jid IN (SELECT jid FROM chats WHERE unread = 0)",
            [],
        )?;
        transaction.commit()?;
        Ok(changed as u64)
    }

    pub fn regular_app_state_is_complete(&self) -> Result<bool> {
        if !self.protocol_db.exists() {
            return Ok(false);
        }
        let connection = Connection::open(&self.protocol_db).with_context(|| {
            format!("opening session database at {}", self.protocol_db.display())
        })?;
        let has_versions = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'app_state_versions'
             )",
            [],
            |row| row.get::<_, bool>(0),
        );
        let has_versions = has_versions?;
        if !has_versions {
            return Ok(false);
        }
        connection
            .query_row(
                "SELECT COUNT(DISTINCT name) = 3 FROM app_state_versions
                 WHERE name IN ('regular', 'regular_low', 'regular_high')",
                [],
                |row| row.get(0),
            )
            .context("checking WhatsApp app-state progress")
    }

    pub fn apply_pin(&self, chat_jid: &str, pinned: bool) -> Result<()> {
        self.update_chat_setting(chat_jid, "pinned", i64::from(pinned))
    }

    pub fn apply_archive(&self, chat_jid: &str, archived: bool) -> Result<()> {
        self.update_chat_setting(chat_jid, "archived", i64::from(archived))
    }

    pub fn apply_mute(&self, chat_jid: &str, muted: bool, mute_end: i64) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection.execute(
            "INSERT INTO chat_settings (jid, muted, mute_end) VALUES (?1, ?2, ?3)
             ON CONFLICT(jid) DO UPDATE SET muted = excluded.muted, mute_end = excluded.mute_end",
            params![chat_jid, muted, mute_end],
        )?;
        Ok(())
    }

    pub fn is_muted(&self, chat_jid: &str, now: i64) -> Result<bool> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection
            .query_row(
                "SELECT COALESCE(muted, 0), COALESCE(mute_end, 0)
                 FROM chat_settings WHERE jid = ?1",
                [chat_jid],
                |row| {
                    let muted: bool = row.get(0)?;
                    let end: i64 = row.get(1)?;
                    Ok(muted && (end <= 0 || end > now))
                },
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(Into::into)
    }

    fn update_chat_setting(&self, chat_jid: &str, column: &str, value: i64) -> Result<()> {
        let sql = match column {
            "pinned" => {
                "INSERT INTO chat_settings (jid, pinned) VALUES (?1, ?2)
                 ON CONFLICT(jid) DO UPDATE SET pinned = excluded.pinned"
            }
            "archived" => {
                "INSERT INTO chat_settings (jid, archived) VALUES (?1, ?2)
                 ON CONFLICT(jid) DO UPDATE SET archived = excluded.archived"
            }
            _ => unreachable!("fixed internal chat-setting column"),
        };
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection.execute(sql, params![chat_jid, value])?;
        Ok(())
    }

    pub fn apply_status_mute(&self, jid: &str, muted: bool) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection.execute(
            "INSERT INTO chat_settings (jid, status_muted) VALUES (?1, ?2)
             ON CONFLICT(jid) DO UPDATE SET status_muted = excluded.status_muted",
            params![jid, muted],
        )?;
        Ok(())
    }

    pub fn apply_disappearing_mode(&self, jid: &str, duration: u32, updated_at: i64) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection.execute(
            "INSERT INTO chat_settings
             (jid, disappearing_duration, disappearing_updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(jid) DO UPDATE SET
                disappearing_duration = excluded.disappearing_duration,
                disappearing_updated_at = excluded.disappearing_updated_at
             WHERE COALESCE(chat_settings.disappearing_updated_at, 0) <= excluded.disappearing_updated_at",
            params![jid, duration, updated_at],
        )?;
        Ok(())
    }

    pub fn star_message(&self, chat_jid: &str, message_id: &str, starred: bool) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection.execute(
            "UPDATE messages SET starred = ?3 WHERE chat_jid = ?1 AND id = ?2",
            params![chat_jid, message_id, starred],
        )?;
        Ok(())
    }

    pub fn update_receipts(&self, message_ids: &[String], receipt: i64) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.transaction()?;
        for id in message_ids {
            transaction.execute(
                "UPDATE messages SET receipt = MAX(receipt, ?2) WHERE id = ?1 AND from_me = 1",
                params![id, receipt],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_chat(&self, chat_jid: &str, timestamp: i64) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM messages WHERE chat_jid = ?1", [chat_jid])?;
        transaction.execute("DELETE FROM reactions WHERE chat_jid = ?1", [chat_jid])?;
        transaction.execute("DELETE FROM chats WHERE jid = ?1", [chat_jid])?;
        transaction.execute("DELETE FROM chat_labels WHERE chat_jid = ?1", [chat_jid])?;
        transaction.execute(
            "INSERT INTO chat_settings (jid, deleted, cleared_at) VALUES (?1, 1, ?2)
             ON CONFLICT(jid) DO UPDATE SET deleted = 1, cleared_at = MAX(cleared_at, ?2)",
            params![chat_jid, timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn clear_chat(&self, chat_jid: &str, timestamp: i64) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM messages WHERE chat_jid = ?1", [chat_jid])?;
        transaction.execute("DELETE FROM reactions WHERE chat_jid = ?1", [chat_jid])?;
        transaction.execute(
            "UPDATE chats SET last_message = '', unread = 0 WHERE jid = ?1",
            [chat_jid],
        )?;
        transaction.execute(
            "INSERT INTO chat_settings (jid, read_state, cleared_at) VALUES (?1, 1, ?2)
             ON CONFLICT(jid) DO UPDATE SET
                read_state = 1, cleared_at = MAX(cleared_at, ?2)",
            params![chat_jid, timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_message(&self, chat_jid: &str, message_id: &str) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM messages WHERE chat_jid = ?1 AND id = ?2",
            params![chat_jid, message_id],
        )?;
        transaction.execute(
            "DELETE FROM reactions WHERE chat_jid = ?1 AND message_id = ?2",
            params![chat_jid, message_id],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO message_tombstones (chat_jid, id) VALUES (?1, ?2)",
            params![chat_jid, message_id],
        )?;
        transaction.execute(
            "UPDATE chats SET
                last_message = COALESCE((SELECT text FROM messages WHERE chat_jid = ?1 ORDER BY timestamp DESC LIMIT 1), ''),
                last_timestamp = COALESCE((SELECT timestamp FROM messages WHERE chat_jid = ?1 ORDER BY timestamp DESC LIMIT 1), last_timestamp)
             WHERE jid = ?1",
            [chat_jid],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_message_media(
        &self,
        chat_jid: &str,
        message_id: &str,
        media: &MessageMedia,
    ) -> Result<bool> {
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let mut merged_media = media.clone();
        if let MessageMedia::Location {
            thumbnail_path,
            duration_seconds,
            ..
        } = &mut merged_media
            && let Some(previous_json) = connection
                .query_row(
                    "SELECT media_json FROM messages WHERE chat_jid = ?1 AND id = ?2",
                    params![chat_jid, message_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
            && let Ok(MessageMedia::Location {
                thumbnail_path: previous_thumbnail,
                duration_seconds: previous_duration,
                ..
            }) = serde_json::from_str(&previous_json)
        {
            if thumbnail_path.is_none() {
                *thumbnail_path = previous_thumbnail;
            }
            if *duration_seconds == 0 {
                *duration_seconds = previous_duration;
            }
        }
        let json = serde_json::to_string(&merged_media)?;
        let gif_placeholder = matches!(
            merged_media,
            MessageMedia::Video {
                gif_playback: true,
                ..
            }
        )
        .then_some("[GIF]");
        let transaction = connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE messages SET
                media_json = ?3,
                text = CASE
                    WHEN ?4 IS NOT NULL AND text = '[Video]' THEN ?4
                    ELSE text END
             WHERE chat_jid = ?1 AND id = ?2
               AND (COALESCE(media_json, '') != ?3
                    OR (?4 IS NOT NULL AND text = '[Video]'))",
            params![chat_jid, message_id, json, gif_placeholder],
        )? > 0;
        if let Some(placeholder) = gif_placeholder {
            transaction.execute(
                "UPDATE chats SET last_message = ?3
                 WHERE jid = ?1 AND last_message = '[Video]'
                   AND ?2 = (SELECT id FROM messages
                             WHERE chat_jid = ?1
                             ORDER BY timestamp DESC LIMIT 1)",
                params![chat_jid, message_id, placeholder],
            )?;
        }
        transaction.commit()?;
        Ok(updated)
    }

    pub fn store_media_download(
        &self,
        chat_jid: &str,
        message_id: &str,
        payload: &[u8],
    ) -> Result<bool> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        Ok(connection.execute(
            "UPDATE messages SET media_download = ?3
             WHERE chat_jid = ?1 AND id = ?2
               AND (media_download IS NULL OR media_download != ?3)",
            params![chat_jid, message_id, payload],
        )? > 0)
    }

    pub fn media_download(&self, chat_jid: &str, message_id: &str) -> Result<Option<Vec<u8>>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let payload = connection
            .query_row(
                "SELECT media_download FROM messages WHERE chat_jid = ?1 AND id = ?2",
                params![chat_jid, message_id],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?;
        Ok(payload.flatten())
    }

    pub fn message_media_kind(&self, chat_jid: &str, message_id: &str) -> Result<Option<String>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let kind = connection
            .query_row(
                "SELECT json_extract(media_json, '$.kind') FROM messages
                 WHERE chat_jid = ?1 AND id = ?2",
                params![chat_jid, message_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(kind.flatten())
    }

    pub fn update_label(
        &self,
        id: &str,
        name: Option<&str>,
        color: Option<i32>,
        deleted: bool,
    ) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO labels (id, name, color, deleted) VALUES (?1, COALESCE(?2, ''), ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                name = CASE WHEN ?2 IS NULL THEN labels.name ELSE excluded.name END,
                color = COALESCE(excluded.color, labels.color),
                deleted = excluded.deleted",
            params![id, name, color, deleted],
        )?;
        if deleted {
            transaction.execute("DELETE FROM chat_labels WHERE label_id = ?1", [id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn associate_label(&self, chat_jid: &str, label_id: &str, labeled: bool) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        if labeled {
            connection.execute(
                "INSERT OR IGNORE INTO chat_labels (chat_jid, label_id) VALUES (?1, ?2)",
                params![chat_jid, label_id],
            )?;
        } else {
            connection.execute(
                "DELETE FROM chat_labels WHERE chat_jid = ?1 AND label_id = ?2",
                params![chat_jid, label_id],
            )?;
        }
        Ok(())
    }

    pub fn migrate_contact_jid(&self, old_jid: &str, new_jid: &str) -> Result<bool> {
        if old_jid == new_jid {
            return Ok(false);
        }
        let mut connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let transaction = connection.transaction()?;
        let source_exists = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM chats WHERE jid = ?1
                 UNION ALL SELECT 1 FROM contacts WHERE jid = ?1
                 UNION ALL SELECT 1 FROM chat_settings WHERE jid = ?1
                 UNION ALL SELECT 1 FROM messages WHERE sender_jid = ?1
                 UNION ALL SELECT 1 FROM reactions WHERE reactor_jid = ?1
             )",
            [old_jid],
            |row| row.get::<_, bool>(0),
        )?;
        if !source_exists {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO contacts (jid, name, source)
             SELECT ?2, name, source FROM contacts WHERE jid = ?1
             ON CONFLICT(jid) DO UPDATE SET
                name = CASE WHEN excluded.source >= contacts.source THEN excluded.name ELSE contacts.name END,
                source = MAX(contacts.source, excluded.source)",
            params![old_jid, new_jid],
        )?;
        transaction.execute(
            "UPDATE messages SET sender_jid = ?2 WHERE sender_jid = ?1",
            params![old_jid, new_jid],
        )?;
        transaction.execute(
            "UPDATE OR IGNORE reactions SET reactor_jid = ?2 WHERE reactor_jid = ?1",
            params![old_jid, new_jid],
        )?;
        transaction.execute("DELETE FROM reactions WHERE reactor_jid = ?1", [old_jid])?;
        transaction.execute(
            "INSERT INTO chats
             (jid, name, name_source, phone_number, last_message, last_timestamp, unread, is_group)
             SELECT ?2, name, name_source, phone_number, last_message, last_timestamp, unread, is_group
             FROM chats WHERE jid = ?1
             ON CONFLICT(jid) DO UPDATE SET
                name = CASE
                    WHEN excluded.name_source > chats.name_source THEN excluded.name
                    ELSE chats.name END,
                name_source = MAX(chats.name_source, excluded.name_source),
                phone_number = COALESCE(chats.phone_number, excluded.phone_number),
                last_message = CASE
                    WHEN excluded.last_timestamp >= chats.last_timestamp
                    THEN excluded.last_message ELSE chats.last_message END,
                last_timestamp = MAX(chats.last_timestamp, excluded.last_timestamp),
                unread = MAX(chats.unread, excluded.unread),
                is_group = MAX(chats.is_group, excluded.is_group)",
            params![old_jid, new_jid],
        )?;
        transaction.execute(
            "INSERT INTO messages
             (chat_jid, id, sender_jid, sender_name, text, timestamp, from_me,
              read, starred, receipt, media_json, media_download)
             SELECT ?2, id, sender_jid, sender_name, text, timestamp, from_me,
                    read, starred, receipt, media_json, media_download
             FROM messages WHERE chat_jid = ?1
             ON CONFLICT(chat_jid, id) DO UPDATE SET
                read = MAX(messages.read, excluded.read),
                starred = MAX(messages.starred, excluded.starred),
                receipt = MAX(messages.receipt, excluded.receipt),
                media_json = COALESCE(messages.media_json, excluded.media_json),
                media_download = COALESCE(messages.media_download, excluded.media_download)",
            params![old_jid, new_jid],
        )?;
        transaction.execute("DELETE FROM messages WHERE chat_jid = ?1", [old_jid])?;
        transaction.execute(
            "DELETE FROM messages WHERE chat_jid = ?1 AND rowid NOT IN
             (SELECT rowid FROM messages WHERE chat_jid = ?1
              ORDER BY timestamp DESC LIMIT 1000)",
            [new_jid],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO reactions
             (chat_jid, message_id, reactor_jid, emoji, from_me, timestamp)
             SELECT ?2, message_id, reactor_jid, emoji, from_me, timestamp
             FROM reactions WHERE chat_jid = ?1",
            params![old_jid, new_jid],
        )?;
        transaction.execute("DELETE FROM reactions WHERE chat_jid = ?1", [old_jid])?;
        transaction.execute(
            "INSERT OR IGNORE INTO message_tombstones (chat_jid, id)
             SELECT ?2, id FROM message_tombstones WHERE chat_jid = ?1",
            params![old_jid, new_jid],
        )?;
        transaction.execute(
            "DELETE FROM message_tombstones WHERE chat_jid = ?1",
            [old_jid],
        )?;
        transaction.execute(
            "UPDATE OR IGNORE chat_labels SET chat_jid = ?2 WHERE chat_jid = ?1",
            params![old_jid, new_jid],
        )?;
        transaction.execute("DELETE FROM chat_labels WHERE chat_jid = ?1", [old_jid])?;
        transaction.execute(
            "INSERT INTO chat_settings
             (jid, pinned, archived, muted, mute_end, read_state, status_muted,
              disappearing_duration, disappearing_updated_at, deleted, cleared_at)
             SELECT ?2, pinned, archived, muted, mute_end, read_state, status_muted,
                    disappearing_duration, disappearing_updated_at, deleted, cleared_at
             FROM chat_settings WHERE jid = ?1
             ON CONFLICT(jid) DO UPDATE SET
                pinned = COALESCE(excluded.pinned, chat_settings.pinned),
                archived = COALESCE(excluded.archived, chat_settings.archived),
                muted = COALESCE(excluded.muted, chat_settings.muted),
                mute_end = COALESCE(excluded.mute_end, chat_settings.mute_end),
                read_state = COALESCE(excluded.read_state, chat_settings.read_state),
                status_muted = COALESCE(excluded.status_muted, chat_settings.status_muted),
                disappearing_duration = CASE
                    WHEN COALESCE(excluded.disappearing_updated_at, 0)
                       >= COALESCE(chat_settings.disappearing_updated_at, 0)
                    THEN excluded.disappearing_duration
                    ELSE chat_settings.disappearing_duration END,
                disappearing_updated_at = MAX(
                    COALESCE(chat_settings.disappearing_updated_at, 0),
                    COALESCE(excluded.disappearing_updated_at, 0)),
                deleted = MAX(chat_settings.deleted, excluded.deleted),
                cleared_at = MAX(chat_settings.cleared_at, excluded.cleared_at)",
            params![old_jid, new_jid],
        )?;
        transaction.execute("DELETE FROM chat_settings WHERE jid = ?1", [old_jid])?;
        transaction.execute(
            "UPDATE chats SET
                name = (SELECT name FROM contacts WHERE jid = ?1),
                name_source = (SELECT CASE source WHEN 1 THEN ?2 ELSE ?3 END
                               FROM contacts WHERE jid = ?1)
             WHERE jid = ?1 AND is_group = 0
               AND EXISTS (SELECT 1 FROM contacts WHERE jid = ?1)",
            params![new_jid, CHAT_NAME_ADDRESS_BOOK, CHAT_NAME_MESSAGE],
        )?;
        transaction.execute(
            "UPDATE messages SET sender_name = (SELECT name FROM contacts WHERE jid = ?1)
             WHERE sender_jid = ?1
               AND EXISTS (SELECT 1 FROM contacts WHERE jid = ?1)",
            [new_jid],
        )?;
        transaction.execute("DELETE FROM chats WHERE jid = ?1", [old_jid])?;
        transaction.execute("DELETE FROM contacts WHERE jid = ?1", [old_jid])?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn chat_name(&self, chat_jid: &str) -> Result<Option<String>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection
            .query_row(
                "SELECT name FROM chats WHERE jid = ?1 AND name_source > ?2 AND name != ''",
                params![chat_jid, CHAT_NAME_UNKNOWN],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn unresolved_chat_jids(&self, is_group: bool, limit: u32) -> Result<Vec<String>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT jid FROM chats
             WHERE is_group = ?1
               AND (name_source = ?2 OR TRIM(name) = '' OR name = jid)
             ORDER BY last_timestamp DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![is_group, CHAT_NAME_UNKNOWN, i64::from(limit.clamp(1, 500))],
            |row| row.get(0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn unresolved_contact_history_cursors(&self, limit: u32) -> Result<Vec<HistoryCursor>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT c.jid, m.id, m.sender_jid, m.from_me, m.timestamp
             FROM chats c
             JOIN messages m ON m.rowid = (
                 SELECT oldest.rowid FROM messages oldest
                 WHERE oldest.chat_jid = c.jid
                 ORDER BY oldest.timestamp ASC LIMIT 1
             )
             WHERE c.is_group = 0
               AND (c.name_source = ?2 OR TRIM(c.name) = '' OR c.name = c.jid)
               AND c.jid != '0@s.whatsapp.net'
             ORDER BY c.last_timestamp DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(
            params![i64::from(limit.clamp(1, 100)), CHAT_NAME_UNKNOWN],
            |row| {
                let timestamp: i64 = row.get(4)?;
                Ok(HistoryCursor {
                    chat_jid: row.get(0)?,
                    message_id: row.get(1)?,
                    sender_jid: row.get(2)?,
                    from_me: row.get(3)?,
                    timestamp_ms: timestamp.saturating_mul(1_000),
                })
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn media_recovery_cursor(&self, chat_jid: &str) -> Result<Option<HistoryCursor>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection
            .query_row(
                "SELECT id, sender_jid, from_me, timestamp FROM messages
                 WHERE chat_jid = ?1 AND (
                   (media_json IS NULL
                    AND text IN ('[Image]', '[Video]', '[Voice message]', '[Document]', '[Location]', '[Live location]'))
                   OR ((media_json LIKE '%\"kind\":\"image\"%'
                        OR media_json LIKE '%\"kind\":\"video\"%')
                       AND (media_download IS NULL
                            OR media_json NOT LIKE '%\"thumbnail_path\"%'))
                   OR (media_json LIKE '%\"kind\":\"location\"%'
                       AND media_json LIKE '%\"live\":true%'
                       AND COALESCE(json_extract(media_json, '$.duration_seconds'), 0) = 0)
                 )
                 ORDER BY timestamp DESC LIMIT 1",
                [chat_jid],
                |row| {
                    let timestamp: i64 = row.get(3)?;
                    Ok(HistoryCursor {
                        chat_jid: chat_jid.to_owned(),
                        message_id: row.get(0)?,
                        sender_jid: row.get(1)?,
                        from_me: row.get(2)?,
                        timestamp_ms: timestamp.saturating_mul(1_000),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn message_history_cursor(
        &self,
        chat_jid: &str,
        message_id: &str,
    ) -> Result<Option<HistoryCursor>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        connection
            .query_row(
                "SELECT id, sender_jid, from_me, timestamp FROM messages
                 WHERE chat_jid = ?1 AND id = ?2",
                params![chat_jid, message_id],
                |row| {
                    let timestamp: i64 = row.get(3)?;
                    Ok(HistoryCursor {
                        chat_jid: chat_jid.to_owned(),
                        message_id: row.get(0)?,
                        sender_jid: row.get(1)?,
                        from_me: row.get(2)?,
                        timestamp_ms: timestamp.saturating_mul(1_000),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn avatar_jids(&self, limit: u32) -> Result<Vec<String>> {
        let connection = self
            .connection
            .lock()
            .expect("history database mutex poisoned");
        let mut statement = connection.prepare(
            "WITH candidates AS (
                 SELECT jid, last_timestamp AS rank, 0 AS priority FROM chats
                 WHERE jid != '0@s.whatsapp.net'
                 UNION ALL
                 SELECT sender_jid AS jid, MAX(timestamp) AS rank, 1 AS priority FROM messages
                 WHERE from_me = 0 AND sender_jid != ''
                 GROUP BY sender_jid
             )
             SELECT jid FROM candidates
             WHERE jid NOT LIKE '%@broadcast' AND jid NOT LIKE '%@newsletter'
             GROUP BY jid
             ORDER BY MIN(priority), MAX(rank) DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(limit.clamp(1, 1000))], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, timestamp: i64) -> Message {
        Message {
            id: id.into(),
            chat_jid: "1@s.whatsapp.net".into(),
            sender_jid: "1@s.whatsapp.net".into(),
            sender_name: "Ada".into(),
            text: format!("message {id}"),
            timestamp,
            from_me: false,
            media: None,
            reactions: Vec::new(),
        }
    }

    #[test]
    fn stores_orders_and_marks_messages_read() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        assert!(
            database
                .insert_message(&message("b", 2), "Ada", false, true)
                .unwrap()
        );
        assert!(
            database
                .insert_message(&message("a", 1), "Ada", false, true)
                .unwrap()
        );
        assert!(
            !database
                .insert_message(&message("a", 1), "Ada", false, true)
                .unwrap()
        );
        assert_eq!(database.unread_total().unwrap(), 2);
        assert_eq!(
            database
                .first_unread_message_id("1@s.whatsapp.net")
                .unwrap()
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            database.unread_receipts("1@s.whatsapp.net").unwrap().len(),
            2
        );
        let stored = database.messages("1@s.whatsapp.net", 50).unwrap();
        assert_eq!(
            stored.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        database.mark_read("1@s.whatsapp.net").unwrap();
        assert_eq!(database.unread_total().unwrap(), 0);
        assert_eq!(
            database
                .first_unread_message_id("1@s.whatsapp.net")
                .unwrap(),
            None
        );
        assert!(
            database
                .unread_receipts("1@s.whatsapp.net")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn avatar_sync_prioritizes_conversations_before_group_senders() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_history_chat(&Chat {
                jid: "1@s.whatsapp.net".into(),
                name: "Ada".into(),
                phone_number: None,
                last_message: "Old chat".into(),
                last_sender_name: "Ada".into(),
                last_timestamp: 1,
                unread: 0,
                is_group: false,
            })
            .unwrap();
        let mut group_message = message("recent", 10);
        group_message.chat_jid = "2@g.us".into();
        group_message.sender_jid = "3@s.whatsapp.net".into();
        database
            .insert_message(&group_message, "Garden Club", true, false)
            .unwrap();

        assert_eq!(
            database.avatar_jids(2).unwrap(),
            vec!["2@g.us".to_owned(), "1@s.whatsapp.net".to_owned()]
        );
    }

    #[test]
    fn chat_list_includes_the_latest_sender_name() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let mut group_message = message("latest", 2);
        group_message.chat_jid = "123-456@g.us".into();
        group_message.sender_jid = "2@s.whatsapp.net".into();
        group_message.sender_name = "Grace".into();
        database
            .insert_message(&group_message, "Friends", true, false)
            .unwrap();

        let chat = database.list_chats(10).unwrap().remove(0);
        assert!(chat.is_group);
        assert_eq!(chat.last_message, "message latest");
        assert_eq!(chat.last_sender_name, "Grace");
    }

    #[test]
    fn nullable_media_download_metadata_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_message(&message("image", 1), "Ada", false, false)
            .unwrap();

        assert_eq!(
            database
                .media_download("1@s.whatsapp.net", "image")
                .unwrap(),
            None
        );
        assert_eq!(
            database
                .message_media_kind("1@s.whatsapp.net", "image")
                .unwrap(),
            None
        );
        assert!(
            database
                .store_media_download("1@s.whatsapp.net", "image", b"download metadata")
                .unwrap()
        );
        assert_eq!(
            database
                .media_download("1@s.whatsapp.net", "image")
                .unwrap(),
            Some(b"download metadata".to_vec())
        );
        let video_path = directory.path().join("clip.video.mp4");
        let missing_thumbnail = directory.path().join("missing-thumbnail.jpg");
        fs::write(&video_path, b"video").unwrap();
        database
            .update_message_media(
                "1@s.whatsapp.net",
                "image",
                &MessageMedia::Video {
                    path: video_path.to_string_lossy().into_owned(),
                    thumbnail_path: missing_thumbnail.to_string_lossy().into_owned(),
                    downloaded: false,
                    mime_type: "video/mp4".into(),
                    width: 640,
                    height: 480,
                    duration_seconds: 10,
                    gif_playback: false,
                },
            )
            .unwrap();
        assert_eq!(
            database
                .message_media_kind("1@s.whatsapp.net", "image")
                .unwrap()
                .as_deref(),
            Some("video")
        );
        let Some(MessageMedia::Video {
            downloaded,
            thumbnail_path,
            ..
        }) = database.messages("1@s.whatsapp.net", 10).unwrap()[0]
            .media
            .clone()
        else {
            panic!("expected stored video")
        };
        assert!(downloaded);
        assert!(thumbnail_path.is_empty());
    }

    #[test]
    fn recovered_gif_relabels_legacy_video_preview() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let mut gif = message("gif", 1);
        gif.text = "[Video]".into();
        database.insert_message(&gif, "Ada", false, false).unwrap();
        database
            .insert_message(&message("newer-text", 2), "Ada", false, false)
            .unwrap();
        assert_eq!(
            database.media_recovery_cursor("1@s.whatsapp.net").unwrap(),
            Some(HistoryCursor {
                chat_jid: "1@s.whatsapp.net".into(),
                message_id: "gif".into(),
                sender_jid: "1@s.whatsapp.net".into(),
                from_me: false,
                timestamp_ms: 1_000,
            })
        );

        database
            .update_message_media(
                "1@s.whatsapp.net",
                "gif",
                &MessageMedia::Video {
                    path: "/cache/clip.video.mp4".into(),
                    thumbnail_path: "/cache/clip.video-thumbnail.jpg".into(),
                    downloaded: false,
                    mime_type: "video/mp4".into(),
                    width: 640,
                    height: 480,
                    duration_seconds: 3,
                    gif_playback: true,
                },
            )
            .unwrap();

        assert_eq!(
            database.messages("1@s.whatsapp.net", 10).unwrap()[0].text,
            "[GIF]"
        );
        assert_eq!(
            database.list_chats(10).unwrap()[0].last_message,
            "message newer-text"
        );
    }

    #[test]
    fn legacy_voice_message_is_selected_for_media_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let mut voice = message("voice", 4);
        voice.text = "[Voice message]".into();
        database
            .insert_message(&voice, "Ada", false, false)
            .unwrap();

        assert_eq!(
            database.media_recovery_cursor(&voice.chat_jid).unwrap(),
            Some(HistoryCursor {
                chat_jid: voice.chat_jid,
                message_id: voice.id,
                sender_jid: voice.sender_jid,
                from_me: voice.from_me,
                timestamp_ms: voice.timestamp * 1_000,
            })
        );
    }

    #[test]
    fn reactions_aggregate_update_and_remove() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_message(&message("target", 1), "Ada", false, false)
            .unwrap();
        assert!(
            database
                .apply_reaction("1@s.whatsapp.net", "target", "me", "👍", true, 2)
                .unwrap()
        );
        database
            .apply_reaction(
                "1@s.whatsapp.net",
                "target",
                "2@s.whatsapp.net",
                "👍",
                false,
                3,
            )
            .unwrap();
        let stored = database.messages("1@s.whatsapp.net", 10).unwrap();
        assert_eq!(
            stored[0].reactions,
            vec![Reaction {
                emoji: "👍".into(),
                count: 2,
                from_me: true,
            }]
        );

        database
            .apply_reaction("1@s.whatsapp.net", "target", "me", "❤️", true, 4)
            .unwrap();
        let stored = database.messages("1@s.whatsapp.net", 10).unwrap();
        assert_eq!(stored[0].reactions.len(), 2);
        assert_eq!(stored[0].reactions[1].emoji, "❤️");
        assert!(stored[0].reactions[1].from_me);

        database
            .apply_reaction("1@s.whatsapp.net", "target", "me", "", true, 5)
            .unwrap();
        let stored = database.messages("1@s.whatsapp.net", 10).unwrap();
        assert_eq!(stored[0].reactions.len(), 1);
        assert_eq!(stored[0].reactions[0].count, 1);
        assert!(!stored[0].reactions[0].from_me);
    }

    #[test]
    fn legacy_unsupported_control_bubbles_are_removed_on_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.db");
        let database = Database::open(&path).unwrap();
        let mut control = message("control", 9);
        control.text = "[Unsupported message]".into();
        database
            .insert_message(&control, "Ada", false, false)
            .unwrap();
        drop(database);

        let database = Database::open(&path).unwrap();
        assert!(
            database
                .messages("1@s.whatsapp.net", 10)
                .unwrap()
                .is_empty()
        );
        let chat = database.list_chats(10).unwrap().remove(0);
        assert_eq!(chat.last_message, "");
        assert_eq!(chat.last_timestamp, 0);
    }

    #[test]
    fn older_history_does_not_replace_a_live_preview() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_message(&message("live", 20), "Ada", false, true)
            .unwrap();
        database
            .insert_message(&message("history", 10), "Ada", false, false)
            .unwrap();
        let chats = database.list_chats(10).unwrap();
        assert_eq!(chats[0].last_message, "message live");
        assert_eq!(chats[0].last_timestamp, 20);
        assert_eq!(chats[0].unread, 1);
    }

    #[test]
    fn push_names_update_direct_chats_and_senders() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_message(&message("one", 1), "1@s.whatsapp.net", false, false)
            .unwrap();
        database
            .update_contact_name("1@s.whatsapp.net", "Ada")
            .unwrap();
        assert_eq!(
            database
                .contact_name("1@s.whatsapp.net")
                .unwrap()
                .as_deref(),
            Some("Ada")
        );
        assert_eq!(database.list_chats(10).unwrap()[0].name, "Ada");
        assert_eq!(
            database.messages("1@s.whatsapp.net", 10).unwrap()[0].sender_name,
            "Ada"
        );
    }

    #[test]
    fn address_book_names_outrank_push_names() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_message(&message("one", 1), "1@s.whatsapp.net", false, false)
            .unwrap();

        database
            .update_contact_name("1@s.whatsapp.net", "Profile name")
            .unwrap();
        database
            .update_address_book_name("1@s.whatsapp.net", "Saved name")
            .unwrap();
        assert!(
            !database
                .update_contact_name("1@s.whatsapp.net", "New profile name")
                .unwrap()
        );
        assert_eq!(database.list_chats(10).unwrap()[0].name, "Saved name");

        let mut newer = message("two", 2);
        newer.sender_name = "New profile name".into();
        database
            .insert_message(&newer, "1@s.whatsapp.net", false, false)
            .unwrap();
        assert!(
            database
                .messages("1@s.whatsapp.net", 10)
                .unwrap()
                .iter()
                .all(|message| message.sender_name == "Saved name")
        );

        database
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET sender_name = 'Stale profile name' WHERE id = 'two'",
                [],
            )
            .unwrap();
        drop(database);
        let reopened = Database::open(&directory.path().join("history.db")).unwrap();
        assert_eq!(
            reopened.messages("1@s.whatsapp.net", 10).unwrap()[1].sender_name,
            "Saved name"
        );
    }

    #[test]
    fn lid_alias_merges_into_phone_conversation_without_losing_state() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let phone_jid = "31612345678@s.whatsapp.net";
        let lid_jid = "100000012345678@lid";

        let mut old = message("old", 10);
        old.chat_jid = phone_jid.into();
        old.sender_jid = phone_jid.into();
        database.insert_message(&old, "Ada", false, false).unwrap();
        database.apply_pin(phone_jid, true).unwrap();

        let mut recent = message("recent", 20);
        recent.chat_jid = lid_jid.into();
        recent.sender_jid = lid_jid.into();
        database
            .insert_message(&recent, "Ada", false, true)
            .unwrap();
        database.apply_archive(lid_jid, true).unwrap();
        database
            .update_chat_phone_number(lid_jid, "31612345678")
            .unwrap();
        database
            .update_address_book_name(lid_jid, "Ada Lovelace")
            .unwrap();

        database.migrate_contact_jid(lid_jid, phone_jid).unwrap();

        let chats = database.list_chats(10).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].jid, phone_jid);
        assert_eq!(chats[0].name, "Ada Lovelace");
        assert_eq!(chats[0].phone_number.as_deref(), Some("31612345678"));
        assert_eq!(chats[0].last_message, "message recent");
        assert_eq!(chats[0].last_timestamp, 20);
        assert_eq!(chats[0].unread, 0);

        let stored = database.messages(phone_jid, 10).unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            ["old", "recent"]
        );
        assert_eq!(stored[1].sender_jid, phone_jid);
        assert!(database.messages(lid_jid, 10).unwrap().is_empty());
        assert_eq!(database.contact_name(lid_jid).unwrap(), None);
        assert_eq!(
            database.contact_name(phone_jid).unwrap().as_deref(),
            Some("Ada Lovelace")
        );

        let connection = database.connection.lock().unwrap();
        let settings = connection
            .query_row(
                "SELECT pinned, archived FROM chat_settings WHERE jid = ?1",
                [phone_jid],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
            )
            .unwrap();
        assert_eq!(settings, (true, true));
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM chats WHERE jid = ?1",
                    [lid_jid],
                    |row| row.get::<_, u32>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn group_metadata_updates_existing_group_subject() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let mut group_message = message("one", 1);
        group_message.chat_jid = "123-456@g.us".into();
        database
            .insert_message(&group_message, "123-456@g.us", true, false)
            .unwrap();

        assert!(
            database
                .update_group_name("123-456@g.us", "Friends")
                .unwrap()
        );
        assert_eq!(database.list_chats(10).unwrap()[0].name, "Friends");
        assert!(database.unresolved_chat_jids(true, 10).unwrap().is_empty());

        let mut replayed = message("history", 2);
        replayed.chat_jid = "123-456@g.us".into();
        database
            .insert_history_chat(&Chat {
                jid: replayed.chat_jid.clone(),
                name: replayed.chat_jid.clone(),
                phone_number: None,
                last_message: replayed.text.clone(),
                last_sender_name: replayed.sender_name.clone(),
                last_timestamp: replayed.timestamp,
                unread: 0,
                is_group: true,
            })
            .unwrap();
        database
            .insert_history_message(&replayed, "123-456@g.us", true)
            .unwrap();
        assert_eq!(database.list_chats(10).unwrap()[0].name, "Friends");

        assert!(
            database
                .update_group_name("123-456@g.us", "Renamed friends")
                .unwrap()
        );
        assert_eq!(database.list_chats(10).unwrap()[0].name, "Renamed friends");
    }

    #[test]
    fn legacy_store_recovers_an_unresolved_name_without_allowing_replay_to_downgrade_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.db");
        {
            let database = Database::open(&path).unwrap();
            let mut group_message = message("one", 1);
            group_message.chat_jid = "123-456@g.us".into();
            database
                .insert_message(&group_message, "123-456@g.us", true, false)
                .unwrap();
            assert_eq!(database.list_chats(10).unwrap()[0].name, "");
        }
        fs::write(
            directory.path().join("store.json"),
            r#"{"chats":[{"jid":"123-456@g.us","name":"Friends"}]}"#,
        )
        .unwrap();

        let database = Database::open(&path).unwrap();
        assert_eq!(database.list_chats(10).unwrap()[0].name, "Friends");
        let mut replayed = message("history", 2);
        replayed.chat_jid = "123-456@g.us".into();
        database
            .insert_history_message(&replayed, "123-456@g.us", true)
            .unwrap();
        assert_eq!(database.list_chats(10).unwrap()[0].name, "Friends");
    }

    #[test]
    fn unresolved_contacts_expose_an_oldest_history_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_message(&message("newer", 20), "1@s.whatsapp.net", false, false)
            .unwrap();
        database
            .insert_message(&message("oldest", 10), "1@s.whatsapp.net", false, false)
            .unwrap();

        assert_eq!(
            database.unresolved_contact_history_cursors(10).unwrap(),
            vec![HistoryCursor {
                chat_jid: "1@s.whatsapp.net".into(),
                message_id: "oldest".into(),
                sender_jid: "1@s.whatsapp.net".into(),
                from_me: false,
                timestamp_ms: 10_000,
            }]
        );

        database
            .update_address_book_name("1@s.whatsapp.net", "Ada")
            .unwrap();
        assert!(
            database
                .unresolved_contact_history_cursors(10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn internal_whatsapp_system_chat_is_not_listed() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        database
            .insert_history_chat(&Chat {
                jid: "0@s.whatsapp.net".into(),
                name: "0@s.whatsapp.net".into(),
                phone_number: None,
                last_message: "internal".into(),
                last_sender_name: "WhatsApp".into(),
                last_timestamp: 2,
                unread: 0,
                is_group: false,
            })
            .unwrap();
        database
            .insert_message(&message("visible", 1), "Ada", false, false)
            .unwrap();
        assert!(
            database
                .update_chat_phone_number("1@s.whatsapp.net", "31612345678")
                .unwrap()
        );

        let chats = database.list_chats(10).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].jid, "1@s.whatsapp.net");
        assert_eq!(chats[0].phone_number.as_deref(), Some("31612345678"));
    }

    #[test]
    fn live_location_without_duration_exposes_a_recovery_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let mut live = message("live", 10);
        live.text = "[Live location]".into();
        live.media = Some(MessageMedia::Location {
            latitude_e7: 523_701_600,
            longitude_e7: 48_953_000,
            accuracy_m: 8,
            name: String::new(),
            address: String::new(),
            thumbnail_path: None,
            live: true,
            updated_at: 10,
            duration_seconds: 0,
        });
        database.insert_message(&live, "Ada", false, false).unwrap();

        assert_eq!(
            database.media_recovery_cursor(&live.chat_jid).unwrap(),
            Some(HistoryCursor {
                chat_jid: live.chat_jid,
                message_id: live.id,
                sender_jid: live.sender_jid,
                from_me: live.from_me,
                timestamp_ms: live.timestamp * 1_000,
            })
        );
    }

    #[test]
    fn event_state_and_media_updates_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let mut rich = message("rich", 10);
        rich.media = Some(MessageMedia::Location {
            latitude_e7: 523_701_600,
            longitude_e7: 48_953_000,
            accuracy_m: 8,
            name: "Amsterdam".into(),
            address: String::new(),
            thumbnail_path: None,
            live: true,
            updated_at: 10,
            duration_seconds: 3_600,
        });
        database.insert_message(&rich, "Ada", false, true).unwrap();
        assert_eq!(database.messages(&rich.chat_jid, 10).unwrap()[0], rich);

        database.apply_read_state(&rich.chat_jid, true).unwrap();
        assert_eq!(database.unread_total().unwrap(), 0);
        database.apply_read_state(&rich.chat_jid, false).unwrap();
        assert_eq!(database.unread_total().unwrap(), 1);
        database.apply_pin(&rich.chat_jid, true).unwrap();
        database.apply_archive(&rich.chat_jid, true).unwrap();
        assert_eq!(database.unread_total().unwrap(), 0);
        assert_eq!(database.list_chats(10).unwrap()[0].unread, 0);
        database.apply_archive(&rich.chat_jid, false).unwrap();
        assert_eq!(database.unread_total().unwrap(), 1);
        assert_eq!(database.list_chats(10).unwrap()[0].unread, 1);
        database.apply_archive(&rich.chat_jid, true).unwrap();
        database.apply_mute(&rich.chat_jid, true, i64::MAX).unwrap();
        assert!(database.is_muted(&rich.chat_jid, 20).unwrap());

        let replacement = MessageMedia::Location {
            latitude_e7: 523_702_000,
            longitude_e7: 48_954_000,
            accuracy_m: 4,
            name: "Amsterdam".into(),
            address: String::new(),
            thumbnail_path: None,
            live: true,
            updated_at: 20,
            duration_seconds: 3_600,
        };
        assert!(
            database
                .update_message_media(&rich.chat_jid, &rich.id, &replacement)
                .unwrap()
        );
        assert_eq!(
            database.messages(&rich.chat_jid, 10).unwrap()[0].media,
            Some(replacement)
        );
        assert!(
            database
                .media_recovery_cursor(&rich.chat_jid)
                .unwrap()
                .is_none()
        );
        database.clear_chat(&rich.chat_jid, 20).unwrap();
        assert!(database.messages(&rich.chat_jid, 10).unwrap().is_empty());
        assert_eq!(database.unread_total().unwrap(), 0);
        let old_history = message("old-history", 15);
        assert!(
            !database
                .insert_history_message(&old_history, "Ada", false)
                .unwrap()
        );
        let deleted = message("deleted", 30);
        database
            .insert_message(&deleted, "Ada", false, false)
            .unwrap();
        database
            .delete_message(&deleted.chat_jid, &deleted.id)
            .unwrap();
        assert!(
            !database
                .insert_history_message(&deleted, "Ada", false)
                .unwrap()
        );
    }

    #[test]
    fn full_state_reconciliation_clears_only_unmarked_history_counts() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        for (jid, unread) in [("stale@g.us", 60), ("explicit@g.us", 7)] {
            database
                .insert_history_chat(&Chat {
                    jid: jid.into(),
                    name: jid.into(),
                    phone_number: None,
                    last_message: "history".into(),
                    last_sender_name: "Ada".into(),
                    last_timestamp: 1,
                    unread,
                    is_group: true,
                })
                .unwrap();
        }
        database.apply_read_state("explicit@g.us", false).unwrap();
        assert_eq!(database.reconcile_unread_after_full_sync().unwrap(), 0);
        assert_eq!(database.unread_total().unwrap(), 61);

        let protocol = Connection::open(directory.path().join("session.db")).unwrap();
        protocol
            .execute_batch(
                "CREATE TABLE app_state_versions (name TEXT NOT NULL);
                 INSERT INTO app_state_versions (name) VALUES
                    ('regular'), ('regular_low'), ('regular_high');",
            )
            .unwrap();
        assert_eq!(database.reconcile_unread_after_full_sync().unwrap(), 1);
        let chats = database.list_chats(10).unwrap();
        let counts = chats
            .into_iter()
            .map(|chat| (chat.jid, chat.unread))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(counts["stale@g.us"], 0);
        assert_eq!(counts["explicit@g.us"], 1);
    }

    #[test]
    fn clearing_account_data_removes_history_and_account_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("history.db")).unwrap();
        let saved = message("saved", 10);
        database.insert_message(&saved, "Ada", false, true).unwrap();
        database
            .update_address_book_name(&saved.sender_jid, "Ada")
            .unwrap();

        database.clear_account_data().unwrap();

        assert!(database.list_chats(10).unwrap().is_empty());
        assert!(database.messages(&saved.chat_jid, 10).unwrap().is_empty());
        assert_eq!(database.unread_total().unwrap(), 0);
        assert_eq!(database.contact_name(&saved.sender_jid).unwrap(), None);
    }
}
