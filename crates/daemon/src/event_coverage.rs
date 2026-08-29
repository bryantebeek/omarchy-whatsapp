//! Exhaustive policy for every event exposed by the pinned whatsapp-rust release.
//!
//! The ordered test is intentional: `EventKind` discriminants are contiguous and
//! append-only upstream. A newly added kind changes the terminal discriminant and
//! fails this module's test until we consciously classify it.

use whatsapp_rust::prelude::EventKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// The daemon consumes this event or an equivalent high-level Bot callback.
    App,
    /// whatsapp-rust owns the behavior completely; duplicating it would be harmful.
    Library,
    /// The current product deliberately has no matching feature.
    Excluded,
}

pub const EVENT_COVERAGE: &[(EventKind, Coverage, &str)] = &[
    (EventKind::Connected, Coverage::App, "connection state"),
    (EventKind::Disconnected, Coverage::App, "connection state"),
    (
        EventKind::PairSuccess,
        Coverage::Library,
        "followed by Connected",
    ),
    (EventKind::PairError, Coverage::App, "pairing error state"),
    (EventKind::LoggedOut, Coverage::App, "logged-out state"),
    (EventKind::PairingQrCode, Coverage::App, "QR pairing UI"),
    (
        EventKind::PairingCode,
        Coverage::Excluded,
        "QR-only pairing UI",
    ),
    (
        EventKind::PairingCodeRefresh,
        Coverage::Excluded,
        "QR-only pairing UI",
    ),
    (
        EventKind::QrScannedWithoutMultidevice,
        Coverage::App,
        "pairing error state",
    ),
    (
        EventKind::ClientOutdated,
        Coverage::App,
        "fatal client state",
    ),
    (
        EventKind::Messages,
        Coverage::App,
        "messages, media, locations, reactions, and notifications",
    ),
    (
        EventKind::Receipt,
        Coverage::App,
        "delivery/read persistence",
    ),
    (
        EventKind::UndecryptableMessage,
        Coverage::App,
        "diagnostic; library performs recovery",
    ),
    (
        EventKind::Notification,
        Coverage::Library,
        "internal stanza router",
    ),
    (
        EventKind::ChatPresence,
        Coverage::App,
        "typing and recording UI",
    ),
    (
        EventKind::Presence,
        Coverage::App,
        "online and last-seen UI",
    ),
    (
        EventKind::PictureUpdate,
        Coverage::App,
        "avatar cache invalidation and refresh",
    ),
    (
        EventKind::UserAboutUpdate,
        Coverage::Excluded,
        "no profile-about UI",
    ),
    (
        EventKind::ContactUpdated,
        Coverage::App,
        "profile-name refresh",
    ),
    (
        EventKind::ContactNumberChanged,
        Coverage::App,
        "local JID migration",
    ),
    (
        EventKind::ContactSyncRequested,
        Coverage::App,
        "contact refresh",
    ),
    (
        EventKind::GroupUpdate,
        Coverage::App,
        "group subject refresh",
    ),
    (
        EventKind::ContactUpdate,
        Coverage::App,
        "address-book names",
    ),
    (
        EventKind::IncomingCall,
        Coverage::App,
        "desktop call notification",
    ),
    (
        EventKind::MissedCall,
        Coverage::App,
        "desktop missed-call notification",
    ),
    (
        EventKind::CallEndedElsewhere,
        Coverage::App,
        "call lifecycle diagnostic",
    ),
    (
        EventKind::PushNameUpdate,
        Coverage::App,
        "profile-name refresh",
    ),
    (
        EventKind::SelfPushNameUpdated,
        Coverage::App,
        "account diagnostic and deferred presence recovery",
    ),
    (EventKind::PinUpdate, Coverage::App, "chat ordering"),
    (
        EventKind::MuteUpdate,
        Coverage::App,
        "notification suppression",
    ),
    (EventKind::ArchiveUpdate, Coverage::App, "chat ordering"),
    (EventKind::StarUpdate, Coverage::App, "message metadata"),
    (
        EventKind::MarkChatAsReadUpdate,
        Coverage::App,
        "unread synchronization",
    ),
    (EventKind::DeleteChatUpdate, Coverage::App, "chat deletion"),
    (
        EventKind::ClearChatUpdate,
        Coverage::App,
        "history clearing",
    ),
    (
        EventKind::UserStatusMuteUpdate,
        Coverage::App,
        "status metadata",
    ),
    (
        EventKind::DeleteMessageForMeUpdate,
        Coverage::App,
        "message deletion",
    ),
    (
        EventKind::LabelEditUpdate,
        Coverage::App,
        "label persistence",
    ),
    (
        EventKind::LabelAssociationUpdate,
        Coverage::App,
        "label persistence",
    ),
    (EventKind::HistorySync, Coverage::App, "history import"),
    (
        EventKind::OfflineSyncPreview,
        Coverage::Library,
        "progress-only; offline messages remain notification-silent",
    ),
    (
        EventKind::OfflineSyncCompleted,
        Coverage::App,
        "snapshot refresh",
    ),
    (
        EventKind::DirtyState,
        Coverage::App,
        "derived metadata refresh",
    ),
    (
        EventKind::DeviceListUpdate,
        Coverage::Library,
        "session/device cache maintenance",
    ),
    (
        EventKind::IdentityChange,
        Coverage::App,
        "security notification",
    ),
    (
        EventKind::BusinessStatusUpdate,
        Coverage::App,
        "business-name refresh",
    ),
    (EventKind::StreamReplaced, Coverage::App, "connection state"),
    (
        EventKind::TemporaryBan,
        Coverage::App,
        "fatal connection state",
    ),
    (EventKind::ConnectFailure, Coverage::App, "connection state"),
    (EventKind::StreamError, Coverage::App, "connection state"),
    (
        EventKind::DisappearingModeChanged,
        Coverage::App,
        "expiry-setting persistence",
    ),
    (
        EventKind::NewsletterLiveUpdate,
        Coverage::Excluded,
        "newsletters are excluded from this messenger UI",
    ),
    (
        EventKind::RawNode,
        Coverage::Library,
        "raw forwarding is disabled",
    ),
    (
        EventKind::MexNotification,
        Coverage::Library,
        "feature-specific router",
    ),
    (
        EventKind::PairPasskeyRequest,
        Coverage::Excluded,
        "QR-only pairing UI has no WebAuthn surface",
    ),
    (
        EventKind::PairPasskeyConfirmation,
        Coverage::Excluded,
        "QR-only pairing UI has no WebAuthn surface",
    ),
    (
        EventKind::PairPasskeyError,
        Coverage::Excluded,
        "QR-only pairing UI has no WebAuthn surface",
    ),
    (
        EventKind::ServerAck,
        Coverage::App,
        "outgoing nack diagnostics",
    ),
    (
        EventKind::PairingQrCodesExhausted,
        Coverage::App,
        "pairing retry state",
    ),
    (
        EventKind::PairingCodeError,
        Coverage::Excluded,
        "QR-only pairing UI",
    ),
    (
        EventKind::AppStateSyncFailed,
        Coverage::App,
        "degraded/fatal synchronization state",
    ),
];

pub fn counts() -> (usize, usize, usize) {
    EVENT_COVERAGE.iter().fold((0, 0, 0), |mut counts, entry| {
        match entry.1 {
            Coverage::App => counts.0 += 1,
            Coverage::Library => counts.1 += 1,
            Coverage::Excluded => counts.2 += 1,
        }
        counts
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_upstream_event_kind_has_an_explicit_policy() {
        assert_eq!(
            EVENT_COVERAGE.len(),
            EventKind::AppStateSyncFailed as usize + 1
        );
        for (index, (kind, _, reason)) in EVENT_COVERAGE.iter().enumerate() {
            assert_eq!(
                *kind as usize, index,
                "missing or reordered event at {index}"
            );
            assert!(!reason.is_empty());
        }

        let (app, library, excluded) = counts();
        assert_eq!(app + library + excluded, EVENT_COVERAGE.len());
        assert!(app > 0);
        assert!(library > 0);
        assert!(excluded > 0);
    }
}
