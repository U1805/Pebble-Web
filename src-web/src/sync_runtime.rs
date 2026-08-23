use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use pebble_core::{Account, PebbleError, ProviderType};
use pebble_crypto::CryptoService;
use pebble_mail::{
    GmailProvider, GmailSyncWorker, ImapMailProvider, OutlookProvider, OutlookSyncWorker,
    Pop3Provider, Pop3SyncWorker, SyncConfig, SyncRuntimeStatus, SyncTrigger, SyncWorker,
};
use pebble_search::TantivySearch;
use pebble_store::Store;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tracing::{info, warn};

use crate::events;
use crate::state::OAuthAccountLockRegistry;

const WEB_REALTIME_INTERVAL_KEY: &str = "web_realtime_interval_secs";

fn initial_poll_interval(
    crypto: &pebble_crypto::CryptoService,
    store: &Store,
    fallback: u64,
) -> u64 {
    let fallback = if fallback == 0 { 0 } else { fallback.max(10) };
    match crate::commands::encrypted_store::load_secure_user_data(
        crypto,
        store,
        WEB_REALTIME_INTERVAL_KEY,
    ) {
        Ok(Some(bytes)) => match serde_json::from_slice::<u64>(&bytes) {
            Ok(interval) => interval,
            Err(error) => {
                warn!("Ignoring invalid persisted Web realtime preference: {error}");
                fallback
            }
        },
        Ok(None) => fallback,
        Err(error) => {
            warn!("Failed to load persisted Web realtime preference: {error}");
            fallback
        }
    }
}

fn persist_poll_interval(
    crypto: &pebble_crypto::CryptoService,
    store: &Store,
    interval: u64,
) -> Result<(), PebbleError> {
    let bytes = serde_json::to_vec(&interval)
        .map_err(|error| PebbleError::Internal(format!("Failed to serialize realtime preference: {error}")))?;
    crate::commands::encrypted_store::store_secure_user_data(
        crypto,
        store,
        WEB_REALTIME_INTERVAL_KEY,
        &bytes,
    )
}

struct SyncHandle {
    stop_tx: watch::Sender<bool>,
    trigger_tx: mpsc::UnboundedSender<SyncTrigger>,
    task: tokio::task::JoinHandle<()>,
}

fn spawn_sync_start_placeholder(
    stop_rx: watch::Receiver<bool>,
    trigger_rx: mpsc::UnboundedReceiver<SyncTrigger>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _keepalive = (stop_rx, trigger_rx);
        std::future::pending::<()>().await;
    })
}

/// Long-lived Web sync runtime.
///
/// The lifecycle mirrors the desktop adapter: each account owns one worker
/// handle with independent stop/trigger channels. Shared `pebble-mail`
/// workers retain their provider-specific polling/backoff behavior and IMAP
/// can promote itself to IDLE when the server advertises that capability.
pub struct SyncManager {
    store: Arc<Store>,
    search: Arc<TantivySearch>,
    crypto: Arc<CryptoService>,
    oauth_account_locks: OAuthAccountLockRegistry,
    attachments_dir: PathBuf,
    preferred_poll_interval_secs: AtomicU64,
    ws_broadcast: broadcast::Sender<String>,
    handles: Mutex<HashMap<String, SyncHandle>>,
}

impl SyncManager {
    pub fn new(
        store: Arc<Store>,
        search: Arc<TantivySearch>,
        crypto: Arc<CryptoService>,
        oauth_account_locks: OAuthAccountLockRegistry,
        attachments_dir: PathBuf,
        sync_interval_secs: u64,
        ws_broadcast: broadcast::Sender<String>,
    ) -> Self {
        let preferred_poll_interval_secs =
            initial_poll_interval(&crypto, &store, sync_interval_secs);
        Self {
            store,
            search,
            crypto,
            oauth_account_locks,
            attachments_dir,
            preferred_poll_interval_secs: AtomicU64::new(preferred_poll_interval_secs),
            ws_broadcast,
            handles: Mutex::new(HashMap::new()),
        }
    }

    pub fn preferred_poll_interval_secs(&self) -> u64 {
        self.preferred_poll_interval_secs.load(Ordering::Relaxed)
    }

    /// Auto-resume one long-lived worker for every account after startup
    /// recovery has completed.
    pub fn spawn(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let accounts = match manager.store.list_accounts() {
                Ok(accounts) => accounts,
                Err(error) => {
                    warn!("Failed to list accounts for auto-sync: {error}");
                    return;
                }
            };
            let interval = manager.preferred_poll_interval_secs();
            if interval == 0 {
                for account in accounts {
                    manager.emit_realtime_status(
                        &account,
                        "manual",
                        None,
                        None,
                        Some("Manual only".to_string()),
                    );
                }
                return;
            }
            for account in accounts {
                if let Err(error) = manager
                    .start_account(account.id.clone(), Some(interval))
                    .await
                {
                    warn!(
                        account_id = %account.id,
                        "Failed to auto-resume sync: {error}"
                    );
                }
            }
        });
    }

    pub async fn start_account(
        self: &Arc<Self>,
        account_id: String,
        poll_interval_secs: Option<u64>,
    ) -> Result<(), PebbleError> {
        {
            let mut handles = self.handles.lock().await;
            if let Some(existing) = handles.get(&account_id) {
                if !existing.task.is_finished() {
                    return Ok(());
                }
                handles.remove(&account_id);
            }

            let (placeholder_stop_tx, placeholder_stop_rx) = watch::channel(false);
            let (placeholder_trigger_tx, placeholder_trigger_rx) = mpsc::unbounded_channel();
            let placeholder_task =
                spawn_sync_start_placeholder(placeholder_stop_rx, placeholder_trigger_rx);
            handles.insert(
                account_id.clone(),
                SyncHandle {
                    stop_tx: placeholder_stop_tx,
                    trigger_tx: placeholder_trigger_tx,
                    task: placeholder_task,
                },
            );
        }

        let account = match self.store.get_account(&account_id) {
            Ok(Some(account)) => account,
            Ok(None) => {
                self.remove_placeholder(&account_id).await;
                return Err(PebbleError::Internal(format!(
                    "Account not found: {account_id}"
                )));
            }
            Err(error) => {
                self.remove_placeholder(&account_id).await;
                return Err(error);
            }
        };

        let interval = poll_interval_secs.unwrap_or_else(|| self.preferred_poll_interval_secs());
        let (stop_tx, stop_rx) = watch::channel(false);
        let (trigger_tx, trigger_rx) = mpsc::unbounded_channel();

        let task = match self.build_sync_task(account, stop_rx, trigger_rx, interval) {
            Ok(task) => task,
            Err(error) => {
                self.remove_placeholder(&account_id).await;
                return Err(error);
            }
        };

        let mut handles = self.handles.lock().await;
        if let Some(previous) = handles.insert(
            account_id,
            SyncHandle {
                stop_tx,
                trigger_tx,
                task,
            },
        ) {
            previous.task.abort();
        }
        Ok(())
    }

    async fn remove_placeholder(&self, account_id: &str) {
        let mut handles = self.handles.lock().await;
        if let Some(handle) = handles.remove(account_id) {
            handle.task.abort();
        }
    }

    pub async fn trigger_account(
        self: &Arc<Self>,
        account_id: &str,
        reason: &str,
    ) -> Result<(), PebbleError> {
        let trigger = SyncTrigger::from_reason(reason);
        let should_start_one_shot = {
            let mut handles = self.handles.lock().await;
            match handles.get(account_id) {
                Some(handle) if handle.task.is_finished() => {
                    handles.remove(account_id);
                    true
                }
                Some(handle) => {
                    let send_failed = handle.trigger_tx.send(trigger).is_err();
                    if send_failed {
                        warn!(
                            "Sync trigger channel was already closed for account {}",
                            account_id
                        );
                        handles.remove(account_id);
                    }
                    send_failed
                }
                None => true,
            }
        };

        if should_start_one_shot {
            self.start_account(account_id.to_string(), Some(0)).await?;
        }
        Ok(())
    }

    pub async fn stop_account(&self, account_id: &str) -> bool {
        let mut handles = self.handles.lock().await;
        let Some(handle) = handles.remove(account_id) else {
            return false;
        };
        if !handle.task.is_finished() {
            let _ = handle.stop_tx.send(true);
            handle.task.abort();
        }
        true
    }

    pub async fn apply_realtime_preference(
        self: &Arc<Self>,
        interval: u64,
    ) -> Result<(), PebbleError> {
        let accounts = self.store.list_accounts()?;
        persist_poll_interval(&self.crypto, &self.store, interval)?;
        self.preferred_poll_interval_secs
            .store(interval, Ordering::Relaxed);
        let running_ids = {
            let handles = self.handles.lock().await;
            handles.keys().cloned().collect::<Vec<_>>()
        };
        for account_id in running_ids {
            let _ = self.stop_account(&account_id).await;
        }

        if interval == 0 {
            for account in accounts {
                self.emit_realtime_status(
                    &account,
                    "manual",
                    None,
                    None,
                    Some("Manual only".to_string()),
                );
            }
            return Ok(());
        }

        let mut started_count = 0usize;
        let mut failures = Vec::new();
        for account in accounts {
            match self
                .start_account(account.id.clone(), Some(interval))
                .await
            {
                Ok(()) => started_count += 1,
                Err(error) => failures.push((account.id, error.to_string())),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(PebbleError::Internal(format!(
                "Realtime preference applied with {} account start failure(s); {} account(s) started; failures: {}",
                failures.len(),
                started_count,
                failures
                    .into_iter()
                    .map(|(id, error)| format!("{id}: {error}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            )))
        }
    }

    fn build_sync_task(
        self: &Arc<Self>,
        account: Account,
        stop_rx: watch::Receiver<bool>,
        trigger_rx: mpsc::UnboundedReceiver<SyncTrigger>,
        poll_interval_secs: u64,
    ) -> Result<tokio::task::JoinHandle<()>, PebbleError> {
        let account_id = account.id.clone();
        let (error_tx, progress_tx, message_tx) = self.spawn_event_forwarders(&account);
        let store = Arc::clone(&self.store);
        let attachments_dir = self.attachments_dir.clone();
        let broadcast = self.ws_broadcast.clone();

        let task = match account.provider {
            ProviderType::Gmail => {
                let tokens = crate::commands::oauth::decode_oauth_account_tokens_raw(&self.crypto, &self.store, &account_id)
                    .map_err(|error| {
                        emit_realtime_status_to(
                            &broadcast,
                            &account_id,
                            &ProviderType::Gmail,
                            "auth_required",
                            None,
                            None,
                            Some(error.to_string()),
                        );
                        error
                    })?;
                let expires_at = tokens.expires_at;
                let provider = Arc::new(GmailProvider::new_with_proxy(
                    tokens.access_token.clone(),
                    tokens.proxy.clone(),
                )?);
                let refresher = crate::commands::oauth::build_oauth_token_refresher(
                    crate::commands::oauth::gmail_oauth_config(),
                    tokens.refresh_token,
                    tokens.access_token,
                    Arc::clone(&self.crypto),
                    Arc::clone(&self.store),
                    Arc::clone(&self.oauth_account_locks),
                    account_id.clone(),
                );
                tokio::spawn(async move {
                    let mut config = SyncConfig::default();
                    config.poll_interval_secs = poll_interval_secs;
                    emit_realtime_status_to(
                        &broadcast,
                        &account_id,
                        &ProviderType::Gmail,
                        "polling",
                        Some(now_timestamp_secs()),
                        None,
                        Some(polling_status_message(&config)),
                    );
                    let mut worker = GmailSyncWorker::new(
                        account_id.clone(),
                        provider,
                        store,
                        stop_rx,
                        attachments_dir,
                    )
                    .with_error_tx(error_tx)
                    .with_message_tx(message_tx)
                    .with_progress_tx(progress_tx);
                    worker = worker.with_token_refresher(refresher, expires_at);
                    worker.run(config, Some(trigger_rx)).await;
                    emit_sync_complete_to(&broadcast, &account_id);
                    info!("Gmail sync task completed for account {}", account_id);
                })
            }
            ProviderType::Outlook => {
                let tokens = crate::commands::oauth::decode_oauth_account_tokens_raw(&self.crypto, &self.store, &account_id)
                    .map_err(|error| {
                        emit_realtime_status_to(
                            &broadcast,
                            &account_id,
                            &ProviderType::Outlook,
                            "auth_required",
                            None,
                            None,
                            Some(error.to_string()),
                        );
                        error
                    })?;
                let expires_at = tokens.expires_at;
                let provider = Arc::new(OutlookProvider::new_with_proxy(
                    tokens.access_token.clone(),
                    account_id.clone(),
                    tokens.proxy.clone(),
                )?);
                let refresher = crate::commands::oauth::build_oauth_token_refresher(
                    crate::commands::oauth::outlook_oauth_config(),
                    tokens.refresh_token,
                    tokens.access_token,
                    Arc::clone(&self.crypto),
                    Arc::clone(&self.store),
                    Arc::clone(&self.oauth_account_locks),
                    account_id.clone(),
                );
                tokio::spawn(async move {
                    let mut config = SyncConfig::default();
                    config.poll_interval_secs = poll_interval_secs;
                    emit_realtime_status_to(
                        &broadcast,
                        &account_id,
                        &ProviderType::Outlook,
                        "polling",
                        Some(now_timestamp_secs()),
                        None,
                        Some(polling_status_message(&config)),
                    );
                    let mut worker = OutlookSyncWorker::new(
                        account_id.clone(),
                        provider,
                        store,
                        attachments_dir,
                    )
                    .with_error_tx(error_tx)
                    .with_message_tx(message_tx)
                    .with_progress_tx(progress_tx);
                    worker = worker.with_token_refresher(refresher, expires_at);
                    worker.run(config, stop_rx, Some(trigger_rx)).await;
                    emit_sync_complete_to(&broadcast, &account_id);
                    info!("Outlook sync task completed for account {}", account_id);
                })
            }
            ProviderType::Pop3 => {
                let pop3_config = crate::commands::messages::load_pop3_config(
                    &self.store,
                    &self.crypto,
                    &account_id,
                )
                .map_err(|error| {
                    emit_realtime_status_to(
                        &broadcast,
                        &account_id,
                        &ProviderType::Pop3,
                        "error",
                        None,
                        None,
                        Some(error.to_string()),
                    );
                    error
                })?;
                let provider = Arc::new(Pop3Provider::new(pop3_config));
                tokio::spawn(async move {
                    let mut config = SyncConfig::default();
                    config.poll_interval_secs = poll_interval_secs;
                    emit_realtime_status_to(
                        &broadcast,
                        &account_id,
                        &ProviderType::Pop3,
                        if config.manual_only() { "manual" } else { "polling" },
                        Some(now_timestamp_secs()),
                        None,
                        Some(polling_status_message(&config)),
                    );
                    let worker = Pop3SyncWorker::new(
                        account_id.clone(),
                        provider,
                        store,
                        stop_rx,
                        attachments_dir,
                    )
                    .with_error_tx(error_tx)
                    .with_message_tx(message_tx)
                    .with_progress_tx(progress_tx);
                    worker.run(config, Some(trigger_rx)).await;
                    emit_sync_complete_to(&broadcast, &account_id);
                    info!("POP3 sync task completed for account {}", account_id);
                })
            }
            ProviderType::Imap => {
                let imap_config = crate::commands::messages::load_imap_config(
                    &self.store,
                    &self.crypto,
                    &account_id,
                )
                .map_err(|error| {
                    emit_realtime_status_to(
                        &broadcast,
                        &account_id,
                        &ProviderType::Imap,
                        "error",
                        None,
                        None,
                        Some(error.to_string()),
                    );
                    error
                })?;
                let provider = Arc::new(ImapMailProvider::new(imap_config));
                tokio::spawn(async move {
                    let mut config = SyncConfig::default();
                    config.poll_interval_secs = poll_interval_secs;
                    emit_realtime_status_to(
                        &broadcast,
                        &account_id,
                        &ProviderType::Imap,
                        if config.manual_only() { "manual" } else { "polling" },
                        Some(now_timestamp_secs()),
                        None,
                        Some(polling_status_message(&config)),
                    );
                    let (runtime_status_tx, mut runtime_status_rx) = mpsc::unbounded_channel();
                    let status_broadcast = broadcast.clone();
                    let status_account_id = account_id.clone();
                    let status_config = config.clone();
                    tokio::spawn(async move {
                        while let Some(status) = runtime_status_rx.recv().await {
                            let supports_idle =
                                matches!(status, SyncRuntimeStatus::ImapIdleAvailable);
                            emit_realtime_status_to(
                                &status_broadcast,
                                &status_account_id,
                                &ProviderType::Imap,
                                if status_config.manual_only() {
                                    "manual"
                                } else if supports_idle {
                                    "realtime"
                                } else {
                                    "polling"
                                },
                                Some(now_timestamp_secs()),
                                None,
                                if supports_idle {
                                    None
                                } else {
                                    Some(polling_status_message(&status_config))
                                },
                            );
                        }
                    });
                    let worker = SyncWorker::new(
                        account_id.clone(),
                        provider,
                        store,
                        stop_rx,
                        attachments_dir,
                    )
                    .with_error_tx(error_tx)
                    .with_message_tx(message_tx)
                    .with_progress_tx(progress_tx)
                    .with_runtime_status_tx(runtime_status_tx);
                    worker.run(config, Some(trigger_rx)).await;
                    emit_sync_complete_to(&broadcast, &account_id);
                    info!("IMAP sync task completed for account {}", account_id);
                })
            }
        };
        Ok(task)
    }

    fn spawn_event_forwarders(
        &self,
        account: &Account,
    ) -> (
        mpsc::UnboundedSender<pebble_mail::SyncError>,
        mpsc::UnboundedSender<pebble_mail::SyncProgress>,
        mpsc::UnboundedSender<pebble_mail::StoredMessage>,
    ) {
        let (error_tx, mut error_rx) = mpsc::unbounded_channel::<pebble_mail::SyncError>();
        let (progress_tx, mut progress_rx) =
            mpsc::unbounded_channel::<pebble_mail::SyncProgress>();
        let (message_tx, mut message_rx) =
            mpsc::unbounded_channel::<pebble_mail::StoredMessage>();
        let broadcast = self.ws_broadcast.clone();
        let account_id = account.id.clone();
        let provider = account.provider.clone();
        let search = Arc::clone(&self.search);
        let store = Arc::clone(&self.store);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(error) = error_rx.recv() => {
                        let mode = realtime_error_mode(&error);
                        let error_message = error.message.clone();
                        let payload = serde_json::to_value(&error).unwrap_or_else(|_| json!({
                            "error_type": error.error_type,
                            "message": error.message,
                        }));
                        emit_event_to(&broadcast, events::MAIL_ERROR, &account_id, payload);
                        emit_realtime_status_to(
                            &broadcast,
                            &account_id,
                            &provider,
                            mode,
                            None,
                            None,
                            Some(error_message),
                        );
                    }
                    Some(progress) = progress_rx.recv() => {
                        let payload = serde_json::to_value(&progress).unwrap_or_else(|_| json!({
                            "account_id": progress.account_id,
                            "status": progress.status,
                            "phase": progress.phase,
                            "message": progress.message,
                        }));
                        emit_event_to(
                            &broadcast,
                            events::MAIL_SYNC_PROGRESS,
                            &progress.account_id,
                            payload,
                        );
                    }
                    Some(stored) = message_rx.recv() => {
                        if !stored.reconciliation {
                            let notify = crate::commands::indexing::should_notify_new_mail(&store, &stored)
                                .unwrap_or_else(|error| {
                                    warn!(
                                        message_id = %stored.message.id,
                                        "Failed to evaluate new-mail notification eligibility: {error}"
                                    );
                                    false
                                });
                            let account_id = stored.message.account_id.clone();
                            let message_id = stored.message.id.clone();
                            emit_event_to(
                                &broadcast,
                                events::MAIL_NEW,
                                &account_id,
                                crate::commands::indexing::new_mail_event_payload(&stored),
                            );
                            if notify {
                                let notification_body =
                                    crate::commands::indexing::new_mail_notification_body(&stored);
                                crate::browser_notifications::emit_browser_notification(
                                    &broadcast,
                                    "Pebble - New Mail",
                                    &notification_body,
                                    Some(&account_id),
                                    Some(&message_id),
                                );
                            }
                        }
                        crate::commands::indexing::index_stored_message(&search, &store, &stored).await;
                    }
                    else => break,
                }
            }
        });

        (error_tx, progress_tx, message_tx)
    }

    fn emit_realtime_status(
        &self,
        account: &Account,
        mode: &str,
        last_success_at: Option<i64>,
        next_retry_at: Option<i64>,
        message: Option<String>,
    ) {
        emit_realtime_status_to(
            &self.ws_broadcast,
            &account.id,
            &account.provider,
            mode,
            last_success_at,
            next_retry_at,
            message,
        );
    }
}

fn emit_event_to(
    broadcast: &broadcast::Sender<String>,
    event_type: &str,
    account_id: &str,
    payload: Value,
) {
    let _ = broadcast.send(
        json!({
            "type": event_type,
            "account_id": account_id,
            "payload": payload,
        })
        .to_string(),
    );
}

fn emit_sync_complete_to(broadcast: &broadcast::Sender<String>, account_id: &str) {
    emit_event_to(
        broadcast,
        events::MAIL_SYNC_COMPLETE,
        account_id,
        json!({ "account_id": account_id }),
    );
}

fn emit_realtime_status_to(
    broadcast: &broadcast::Sender<String>,
    account_id: &str,
    provider: &ProviderType,
    mode: &str,
    last_success_at: Option<i64>,
    next_retry_at: Option<i64>,
    message: Option<String>,
) {
    emit_event_to(
        broadcast,
        events::MAIL_REALTIME_STATUS,
        account_id,
        json!({
            "account_id": account_id,
            "mode": mode,
            "provider": provider_slug(provider),
            "last_success_at": last_success_at,
            "next_retry_at": next_retry_at,
            "message": message,
        }),
    );
}

fn provider_slug(provider: &ProviderType) -> &'static str {
    match provider {
        ProviderType::Imap => "imap",
        ProviderType::Pop3 => "pop3",
        ProviderType::Gmail => "gmail",
        ProviderType::Outlook => "outlook",
    }
}

fn now_timestamp_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn polling_status_message(config: &SyncConfig) -> String {
    if config.manual_only() {
        "Manual only".to_string()
    } else {
        format!("Polling every {}s", config.poll_interval_secs)
    }
}

fn realtime_error_mode(error: &pebble_mail::SyncError) -> &'static str {
    let text = format!(
        "{} {}",
        error.error_type.to_ascii_lowercase(),
        error.message.to_ascii_lowercase()
    );
    if text.contains("auth")
        || text.contains("token")
        || text.contains("unauthorized")
        || text.contains("401")
    {
        "auth_required"
    } else if text.contains("offline") || text.contains("network") {
        "offline"
    } else if text.contains("circuit") || text.contains("backoff") {
        "backoff"
    } else {
        "error"
    }
}
