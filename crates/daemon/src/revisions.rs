use omarchy_whatsapp_protocol::{ServerEvent, ServerFrame};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct RevisionClock {
    generation: AtomicU64,
    revision: AtomicU64,
}

impl RevisionClock {
    #[must_use]
    pub fn begin_generation(&self) -> u64 {
        self.generation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                Some(value.saturating_add(1))
            })
            .unwrap_or_else(|value| value)
            .saturating_add(1)
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn is_current(&self, generation: u64) -> bool {
        generation != 0 && self.generation() == generation
    }

    pub fn retire_generation(&self, generation: u64) {
        let _ = self
            .generation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                (current == generation).then(|| current.saturating_add(1))
            });
    }

    #[must_use]
    pub fn stamp_event(&self, event: ServerEvent) -> ServerFrame {
        self.stamp(ServerFrame::event(event))
    }

    #[must_use]
    pub fn stamp_response(&self, id: Option<u64>, event: ServerEvent) -> ServerFrame {
        self.stamp(ServerFrame::response(id, event))
    }

    fn stamp(&self, frame: ServerFrame) -> ServerFrame {
        let revision = self
            .revision
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                Some(value.saturating_add(1))
            })
            .unwrap_or_else(|value| value)
            .saturating_add(1);
        frame.with_metadata(self.generation(), revision)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn frames_are_stamped_with_current_generation_and_monotonic_revisions() {
        let clock = RevisionClock::default();
        assert!(!clock.is_current(0));
        let first_generation = clock.begin_generation();
        assert!(clock.is_current(first_generation));
        let first = clock.stamp_event(ServerEvent::Unread { total: 1 });
        let second = clock.stamp_response(Some(7), ServerEvent::Pong);
        assert_eq!(first.generation, first_generation);
        assert_eq!(first.sequence, 1);
        assert_eq!(second.id, Some(7));
        assert_eq!(second.sequence, 2);

        clock.retire_generation(first_generation.saturating_add(1));
        assert!(clock.is_current(first_generation));
        clock.retire_generation(first_generation);
        assert!(!clock.is_current(first_generation));

        let second_generation = clock.begin_generation();
        assert!(!clock.is_current(first_generation));
        assert!(clock.is_current(second_generation));
        let third = clock.stamp_event(ServerEvent::Unread { total: 2 });
        assert_eq!(third.generation, second_generation);
        assert_eq!(third.sequence, 3);
    }

    #[test]
    fn counters_saturate_without_wrapping() {
        let clock = RevisionClock {
            generation: AtomicU64::new(u64::MAX),
            revision: AtomicU64::new(u64::MAX),
        };
        assert_eq!(clock.begin_generation(), u64::MAX);
        clock.retire_generation(u64::MAX);
        assert_eq!(clock.generation(), u64::MAX);
        let frame = clock.stamp_event(ServerEvent::Pong);
        assert_eq!(frame.generation, u64::MAX);
        assert_eq!(frame.sequence, u64::MAX);
    }
}
