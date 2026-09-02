use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ConnectionIntent {
    active_chat: Option<String>,
    available: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionState {
    pub available: bool,
    pub active_chats: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct ConnectionIntents {
    next_id: u64,
    connections: HashMap<u64, ConnectionIntent>,
}

impl ConnectionIntents {
    #[must_use]
    pub fn connect(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        while self.connections.contains_key(&self.next_id) {
            self.next_id = self.next_id.wrapping_add(1).max(1);
        }
        self.connections
            .insert(self.next_id, ConnectionIntent::default());
        self.next_id
    }

    pub fn set_active_chat(&mut self, id: u64, chat: Option<String>) -> bool {
        let Some(connection) = self.connections.get_mut(&id) else {
            return false;
        };
        connection.active_chat = chat;
        true
    }

    pub fn set_available(&mut self, id: u64, available: bool) -> bool {
        let Some(connection) = self.connections.get_mut(&id) else {
            return false;
        };
        connection.available = available;
        true
    }

    pub fn disconnect(&mut self, id: u64) -> bool {
        self.connections.remove(&id).is_some()
    }

    #[must_use]
    pub fn state(&self) -> ConnectionState {
        let mut state = ConnectionState::default();
        for connection in self.connections.values() {
            state.available |= connection.available;
            if let Some(chat) = &connection.active_chat {
                state.active_chats.insert(chat.clone());
            }
        }
        state
    }

    #[must_use]
    pub fn is_focused(&self, chat: &str) -> bool {
        self.connections
            .values()
            .any(|connection| connection.active_chat.as_deref() == Some(chat))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn intent_is_scoped_to_the_connection_and_aggregated() {
        let mut intents = ConnectionIntents::default();
        let first = intents.connect();
        let second = intents.connect();
        assert_ne!(first, second);
        assert!(intents.set_active_chat(first, Some("a".into())));
        assert!(intents.set_active_chat(second, Some("b".into())));
        assert!(intents.set_available(second, true));
        assert!(intents.is_focused("a"));
        assert!(intents.is_focused("b"));
        assert_eq!(
            intents.state(),
            ConnectionState {
                available: true,
                active_chats: HashSet::from(["a".into(), "b".into()]),
            }
        );
        assert!(intents.disconnect(second));
        assert!(!intents.disconnect(second));
        assert!(!intents.state().available);
        assert!(!intents.is_focused("b"));
    }

    #[test]
    fn unknown_connections_are_rejected_and_ids_do_not_wrap_to_zero() {
        let mut intents = ConnectionIntents {
            next_id: u64::MAX,
            ..ConnectionIntents::default()
        };
        let id = intents.connect();
        assert_eq!(id, 1);
        assert!(!intents.set_active_chat(999, None));
        assert!(!intents.set_available(999, false));

        let mut collision = ConnectionIntents {
            next_id: u64::MAX,
            ..ConnectionIntents::default()
        };
        collision.connections.insert(1, ConnectionIntent::default());
        assert_eq!(collision.connect(), 2);
    }
}
