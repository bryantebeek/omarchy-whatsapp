// Presence and active-chat intent reconciliation for the linked device.

use crate::connections;
use crate::state::Shared;
use tracing::warn;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore_binary::JidExt;

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn reconcile_connection_intent(
    shared: &Shared,
    before: &connections::ConnectionState,
    after: &connections::ConnectionState,
) {
    reconcile_connection_intent_inner(shared, before, after, false).await;
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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
