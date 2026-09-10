use std::sync::Arc;
use std::time::{Duration, Instant};

use pebble_store::Store;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::events;

/// Run the Web snooze watcher on the same cadence and housekeeping schedule
/// as the desktop watcher. WebSocket delivery replaces the desktop OS event.
pub async fn run_snooze_watcher(store: Arc<Store>, ws_broadcast: broadcast::Sender<String>) {
    const INTERVAL: Duration = Duration::from_secs(30);
    const PURGE_INTERVAL: Duration = Duration::from_secs(3600);
    const TOMBSTONE_MAX_AGE_SECS: i64 = 30 * 24 * 3600;
    const VACUUM_INTERVAL: Duration = Duration::from_secs(7 * 24 * 3600);

    let mut ticker = tokio::time::interval(INTERVAL);
    // Tokio intervals fire once immediately. Consume that tick so the first
    // due-snooze pass matches the desktop watcher after one 30-second delay.
    ticker.tick().await;
    let mut last_purge = Instant::now();
    let mut last_vacuum = Instant::now();

    loop {
        ticker.tick().await;
        process_due_snoozes(store.clone(), ws_broadcast.clone()).await;

        if last_purge.elapsed() >= PURGE_INTERVAL {
            let purge_store = store.clone();
            match tokio::task::spawn_blocking(move || {
                purge_store.purge_old_tombstones(TOMBSTONE_MAX_AGE_SECS)
            })
            .await
            {
                Ok(Ok(count)) if count > 0 => info!("Purged {count} old tombstone messages"),
                Ok(Ok(_)) => {}
                Ok(Err(error)) => warn!("Tombstone purge error: {error}"),
                Err(error) => warn!("Tombstone purge task error: {error}"),
            }
            last_purge = Instant::now();
        }

        if last_vacuum.elapsed() >= VACUUM_INTERVAL {
            let vacuum_store = store.clone();
            match tokio::task::spawn_blocking(move || vacuum_store.vacuum()).await {
                Ok(Ok(())) => info!("Database VACUUM completed"),
                Ok(Err(error)) => warn!("Database VACUUM failed: {error}"),
                Err(error) => warn!("Database VACUUM task error: {error}"),
            }
            last_vacuum = Instant::now();
        }
    }
}

/// Expire due snoozes and broadcast the shared `mail:unsnoozed` event.
pub async fn process_due_snoozes(store: Arc<Store>, ws_broadcast: broadcast::Sender<String>) {
    let query_store = store.clone();
    let due = match tokio::task::spawn_blocking(move || {
        query_store.get_due_snoozed(pebble_core::now_timestamp())
    })
    .await
    {
        Ok(Ok(due)) => due,
        Ok(Err(error)) => {
            warn!("Snooze watcher error: {error}");
            return;
        }
        Err(error) => {
            warn!("Snooze watcher task error: {error}");
            return;
        }
    };

    for snoozed in due {
        let item_store = store.clone();
        let message_id = snoozed.message_id.clone();
        match tokio::task::spawn_blocking(move || item_store.unsnooze_message(&message_id)).await {
            Ok(Ok(())) => {
                let notification_body = {
                    let lookup_store = store.clone();
                    let lookup_id = snoozed.message_id.clone();
                    match tokio::task::spawn_blocking(move || lookup_store.get_message(&lookup_id))
                        .await
                    {
                        Ok(Ok(Some(message))) if message.from_name.is_empty() => message.subject,
                        Ok(Ok(Some(message))) => {
                            format!("{}: {}", message.from_name, message.subject)
                        }
                        _ => snoozed.message_id.clone(),
                    }
                };
                let _ = ws_broadcast.send(
                    json!({
                        "type": events::MAIL_UNSNOOZED,
                        "payload": {
                            "message_id": snoozed.message_id,
                            "return_to": snoozed.return_to,
                        },
                    })
                    .to_string(),
                );
                crate::browser_notifications::emit_browser_notification(
                    &ws_broadcast,
                    "Pebble - Snoozed Message",
                    &notification_body,
                    None,
                    None,
                );
            }
            Ok(Err(error)) => warn!("Failed to unsnooze message: {error}"),
            Err(error) => warn!("Snooze unsnooze task error: {error}"),
        }
    }
}
