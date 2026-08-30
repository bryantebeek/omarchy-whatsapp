use std::collections::{HashMap, HashSet};

pub const LEASE_TTL_SECONDS: i64 = 65;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Lease {
    active_chat: Option<String>,
    available: bool,
    touched_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeaseState {
    pub available: bool,
    pub active_chats: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct LeaseBook {
    next_id: u64,
    leases: HashMap<u64, Lease>,
}

impl LeaseBook {
    #[must_use]
    pub fn connect(&mut self, now: i64) -> u64 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        while self.leases.contains_key(&self.next_id) {
            self.next_id = self.next_id.wrapping_add(1).max(1);
        }
        self.leases.insert(
            self.next_id,
            Lease {
                touched_at: now,
                ..Lease::default()
            },
        );
        self.next_id
    }

    pub fn touch(&mut self, id: u64, now: i64) -> bool {
        let Some(lease) = self.leases.get_mut(&id) else {
            return false;
        };
        lease.touched_at = now;
        true
    }

    pub fn set_active_chat(&mut self, id: u64, chat: Option<String>, now: i64) -> bool {
        let Some(lease) = self.leases.get_mut(&id) else {
            return false;
        };
        lease.active_chat = chat;
        lease.touched_at = now;
        true
    }

    pub fn set_available(&mut self, id: u64, available: bool, now: i64) -> bool {
        let Some(lease) = self.leases.get_mut(&id) else {
            return false;
        };
        lease.available = available;
        lease.touched_at = now;
        true
    }

    pub fn disconnect(&mut self, id: u64) -> bool {
        self.leases.remove(&id).is_some()
    }

    pub fn clear(&mut self) {
        self.leases.clear();
    }

    pub fn expire(&mut self, now: i64) -> Vec<u64> {
        let mut expired = self
            .leases
            .iter()
            .filter_map(|(id, lease)| {
                (now.saturating_sub(lease.touched_at) > LEASE_TTL_SECONDS).then_some(*id)
            })
            .collect::<Vec<_>>();
        expired.sort_unstable();
        self.leases
            .retain(|_, lease| now.saturating_sub(lease.touched_at) <= LEASE_TTL_SECONDS);
        expired
    }

    #[must_use]
    pub fn state(&self, now: i64) -> LeaseState {
        let current = self
            .leases
            .values()
            .filter(|lease| now.saturating_sub(lease.touched_at) <= LEASE_TTL_SECONDS);
        let mut state = LeaseState::default();
        for lease in current {
            state.available |= lease.available;
            if let Some(chat) = &lease.active_chat {
                state.active_chats.insert(chat.clone());
            }
        }
        state
    }

    #[must_use]
    pub fn is_focused(&self, chat: &str, now: i64) -> bool {
        self.leases.values().any(|lease| {
            now.saturating_sub(lease.touched_at) <= LEASE_TTL_SECONDS
                && lease.active_chat.as_deref() == Some(chat)
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn leases_are_connection_scoped_and_aggregate_intent() {
        let mut book = LeaseBook::default();
        let first = book.connect(10);
        let second = book.connect(10);
        assert_ne!(first, second);
        assert!(book.set_active_chat(first, Some("a".into()), 11));
        assert!(book.set_active_chat(second, Some("b".into()), 12));
        assert!(book.set_available(second, true, 12));
        assert!(book.is_focused("a", 12));
        assert!(book.is_focused("b", 12));
        assert_eq!(
            book.state(12),
            LeaseState {
                available: true,
                active_chats: HashSet::from(["a".into(), "b".into()]),
            }
        );
        assert!(book.disconnect(second));
        assert!(!book.disconnect(second));
        assert!(!book.state(12).available);
        assert!(!book.is_focused("b", 12));
        book.clear();
        assert_eq!(book.state(12), LeaseState::default());
    }

    #[test]
    fn stale_and_unknown_leases_cannot_keep_focus_or_presence() {
        let mut book = LeaseBook::default();
        let id = book.connect(0);
        assert!(book.set_active_chat(id, Some("a".into()), 1));
        assert!(book.set_available(id, true, 1));
        assert!(!book.touch(999, 2));
        assert!(!book.set_active_chat(999, None, 2));
        assert!(!book.set_available(999, false, 2));
        assert!(book.state(LEASE_TTL_SECONDS + 1).available);
        assert!(!book.state(LEASE_TTL_SECONDS + 2).available);
        assert_eq!(book.expire(LEASE_TTL_SECONDS + 2), vec![id]);
        assert!(book.expire(LEASE_TTL_SECONDS + 2).is_empty());
    }

    #[test]
    fn touch_renews_a_lease_and_ids_do_not_wrap_to_zero() {
        let mut book = LeaseBook {
            next_id: u64::MAX,
            ..LeaseBook::default()
        };
        let id = book.connect(5);
        assert_eq!(id, 1);
        assert!(book.touch(id, LEASE_TTL_SECONDS + 10));
        assert!(book.expire(LEASE_TTL_SECONDS + 11).is_empty());

        let mut collision = LeaseBook {
            next_id: u64::MAX,
            ..LeaseBook::default()
        };
        collision.leases.insert(1, Lease::default());
        assert_eq!(collision.connect(7), 2);
    }
}
