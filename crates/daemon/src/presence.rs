// Presence and active-chat intent reconciliation for the linked device.

use crate::connections;
use crate::state::Shared;
use tracing::warn;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

pub(crate) async fn reconcile_connection_intent(
    shared: &Shared,
    before: &connections::ConnectionState,
    after: &connections::ConnectionState,
) {
    reconcile_connection_intent_inner(shared, before, after, false).await;
}

pub(crate) async fn force_reconcile_connection_intent(
    shared: &Shared,
    before: &connections::ConnectionState,
    after: &connections::ConnectionState,
) {
    reconcile_connection_intent_inner(shared, before, after, true).await;
}

pub(crate) fn effective_presence_available(requested: bool, sync_pending: bool) -> bool {
    requested && !sync_pending
}

async fn reconcile_connection_intent_inner(
    shared: &Shared,
    before: &connections::ConnectionState,
    after: &connections::ConnectionState,
    force_presence: bool,
) {
    let Some(client) = shared.client.read().await.clone() else {
        return;
    };
    if force_presence || before.available != after.available {
        let available =
            effective_presence_available(after.available, shared.presence_sync_pending());
        let result = if available {
            if client.push_name().is_empty() {
                Ok(())
            } else {
                client.set_available().await
            }
        } else {
            client.set_unavailable().await
        };
        if let Err(error) = result {
            warn!(%error, requested = after.available, available, "could not reconcile WhatsApp presence");
        }
    }
    for raw in before.active_chats.difference(&after.active_chats) {
        if let Ok(jid) = raw.parse::<Jid>()
            && !jid.is_group()
            && let Err(error) = client.unsubscribe_presence(&jid).await
        {
            warn!(%error, %jid, "could not unsubscribe inactive-chat presence");
        }
    }
    for raw in after.active_chats.difference(&before.active_chats) {
        if let Ok(jid) = raw.parse::<Jid>()
            && !jid.is_group()
            && let Err(error) = client.subscribe_presence(jid.clone()).await
        {
            warn!(%error, %jid, "could not subscribe active-chat presence");
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::test_support::test_shared;
    use crate::transport::fake::{Call, CallKind, FakeTransport, transport};
    use std::sync::Arc;

    fn intent(available: bool, active_chats: &[&str]) -> connections::ConnectionState {
        connections::ConnectionState {
            available,
            active_chats: active_chats.iter().map(|chat| (*chat).to_owned()).collect(),
        }
    }

    async fn install(shared: &Shared, fake: &Arc<FakeTransport>) {
        *shared.client.write().await = Some(transport(fake));
    }

    #[tokio::test]
    async fn intent_reconciliation_without_a_client_touches_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);

        reconcile_connection_intent(
            &shared,
            &connections::ConnectionState::default(),
            &intent(true, &["1@s.whatsapp.net"]),
        )
        .await;

        assert!(shared.client.read().await.is_none());
    }

    #[tokio::test]
    async fn forced_reconciliation_announces_presence_and_subscribes_direct_chats() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let fake = Arc::new(FakeTransport::new().with_push_name("Ada"));
        install(&shared, &fake).await;

        // A forced pass runs even though `available` did not change, which is
        // what a fresh connection needs to restore its intent.
        force_reconcile_connection_intent(
            &shared,
            &connections::ConnectionState::default(),
            &intent(false, &["1@s.whatsapp.net", "123-456@g.us", "not a jid"]),
        )
        .await;

        assert_eq!(
            fake.calls_of(CallKind::SetUnavailable),
            vec![Call::SetUnavailable]
        );
        assert!(fake.calls_of(CallKind::SetAvailable).is_empty());
        // Groups have no presence subscription and a malformed JID is skipped.
        assert_eq!(
            fake.calls_of(CallKind::SubscribePresence),
            vec![Call::SubscribePresence("1@s.whatsapp.net".into())]
        );

        fake.clear_calls();
        force_reconcile_connection_intent(
            &shared,
            &intent(false, &["1@s.whatsapp.net", "123-456@g.us", "not a jid"]),
            &intent(true, &[]),
        )
        .await;

        assert_eq!(
            fake.calls_of(CallKind::SetAvailable),
            vec![Call::SetAvailable]
        );
        assert_eq!(
            fake.calls_of(CallKind::UnsubscribePresence),
            vec![Call::UnsubscribePresence("1@s.whatsapp.net".into())]
        );
    }

    #[tokio::test]
    async fn an_empty_push_name_and_a_pending_sync_both_suppress_availability() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let fake = Arc::new(FakeTransport::new());
        install(&shared, &fake).await;

        // An unchanged availability is left alone; only the chats reconcile.
        reconcile_connection_intent(
            &shared,
            &intent(true, &[]),
            &intent(true, &["1@s.whatsapp.net"]),
        )
        .await;
        assert!(fake.calls_of(CallKind::SetAvailable).is_empty());
        assert!(fake.calls_of(CallKind::SetUnavailable).is_empty());
        assert_eq!(
            fake.calls_of(CallKind::SubscribePresence),
            vec![Call::SubscribePresence("1@s.whatsapp.net".into())]
        );

        // WhatsApp rejects an available presence before it restored the push
        // name, so the daemon skips the call entirely instead of erroring.
        reconcile_connection_intent(&shared, &intent(false, &[]), &intent(true, &[])).await;
        assert!(fake.calls_of(CallKind::SetAvailable).is_empty());
        assert!(fake.calls_of(CallKind::SetUnavailable).is_empty());

        let generation = shared.clock.begin_generation();
        shared.begin_presence_sync(generation);
        *fake.push_name.lock().unwrap() = "Ada".to_owned();
        reconcile_connection_intent(&shared, &intent(false, &[]), &intent(true, &[])).await;

        assert!(fake.calls_of(CallKind::SetAvailable).is_empty());
        assert_eq!(
            fake.calls_of(CallKind::SetUnavailable),
            vec![Call::SetUnavailable]
        );
    }

    #[tokio::test]
    async fn presence_failures_are_logged_without_aborting_the_pass() {
        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        let fake = Arc::new(
            FakeTransport::new()
                .with_push_name("Ada")
                .failing(CallKind::SetAvailable, "offline")
                .failing(CallKind::SubscribePresence, "offline")
                .failing(CallKind::UnsubscribePresence, "offline"),
        );
        install(&shared, &fake).await;

        reconcile_connection_intent(
            &shared,
            &intent(false, &["1@s.whatsapp.net"]),
            &intent(true, &["2@s.whatsapp.net"]),
        )
        .await;

        assert_eq!(
            fake.calls_of(CallKind::SetAvailable),
            vec![Call::SetAvailable]
        );
        assert_eq!(
            fake.calls_of(CallKind::UnsubscribePresence),
            vec![Call::UnsubscribePresence("1@s.whatsapp.net".into())]
        );
        assert_eq!(
            fake.calls_of(CallKind::SubscribePresence),
            vec![Call::SubscribePresence("2@s.whatsapp.net".into())]
        );

        fake.succeed(CallKind::SetAvailable);
        fake.fail(CallKind::SetUnavailable, "offline");
        reconcile_connection_intent(&shared, &intent(true, &[]), &intent(false, &[])).await;
        assert_eq!(
            fake.calls_of(CallKind::SetUnavailable),
            vec![Call::SetUnavailable]
        );
    }

    #[test]
    fn presence_stays_unavailable_until_the_current_generation_finishes_syncing() {
        assert!(!effective_presence_available(false, false));
        assert!(!effective_presence_available(false, true));
        assert!(effective_presence_available(true, false));
        assert!(!effective_presence_available(true, true));

        let directory = tempfile::tempdir().unwrap();
        let shared = test_shared(&directory);
        assert!(!shared.presence_sync_pending());

        let first = shared.clock.begin_generation();
        shared.begin_presence_sync(first);
        assert!(shared.presence_sync_pending());
        assert!(!shared.finish_presence_sync(first.saturating_add(1)));
        assert!(shared.presence_sync_pending());

        let second = shared.clock.begin_generation();
        assert!(!shared.presence_sync_pending());
        shared.begin_presence_sync(second);
        assert!(!shared.finish_presence_sync(first));
        assert!(shared.presence_sync_pending());
        assert!(shared.finish_presence_sync(second));
        assert!(!shared.presence_sync_pending());
    }
}
