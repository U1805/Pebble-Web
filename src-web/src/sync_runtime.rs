use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pebble_core::{Account, PebbleError, ProviderType};
use pebble_crypto::CryptoService;
use pebble_mail::{
    GmailProvider, GmailSyncWorker, ImapMailProvider, OutlookProvider, OutlookSyncWorker,
    Pop3Provider, Pop3SyncWorker, SyncConfig, SyncWorker,
};
use pebble_search::TantivySearch;
use pebble_store::Store;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::time::Instant;
use tracing::{error, info, warn};

/// Web 端同步管理器。
///
/// 设计（2026-08-19）：服务端定时对全部账户做单轮同步（manual_only 语义，
/// 每轮同步完即断开连接，适合长驻服务器进程）；手动触发走同一路径立即执行。
/// 每账户防重入（running 标记），同步进度/结果通过 ws_broadcast 广播 JSON 事件
/// （阶段 4.7 统一事件命名，此处先发结构化文本）。新同步的消息注入搜索索引。
pub struct SyncManager {
    store: Arc<Store>,
    search: Arc<TantivySearch>,
    crypto: Arc<CryptoService>,
    oauth_account_locks: OAuthAccountLockRegistry,
    attachments_dir: PathBuf,
    poll_interval_secs: watch::Sender<u64>,
    ws_broadcast: broadcast::Sender<String>,
    running: Mutex<HashMap<String, bool>>,
    stop_signals: Mutex<HashMap<String, watch::Sender<bool>>>,
}

use crate::events;
use crate::state::OAuthAccountLockRegistry;

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
        let initial_interval = sync_interval_secs.max(10);
        let (poll_interval_secs, _) = watch::channel(initial_interval);
        Self {
            store,
            search,
            crypto,
            oauth_account_locks,
            attachments_dir,
            poll_interval_secs,
            ws_broadcast,
            running: Mutex::new(HashMap::new()),
            stop_signals: Mutex::new(HashMap::new()),
        }
    }

    /// Change the scheduler interval without restarting the Web service.
    /// A value of zero disables automatic synchronization (manual mode).
    pub fn set_poll_interval_secs(&self, seconds: u64) {
        let _ = self.poll_interval_secs.send(seconds);
    }

    /// Publish the account status snapshot after a preference change without
    /// blocking the async command thread on SQLite I/O.
    pub async fn publish_realtime_preference_status(&self, seconds: u64) {
        // Keep the account-status event in sync with the preference change.
        // This is deliberately best-effort: changing the scheduler must not
        // fail merely because the status snapshot could not be read.
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || store.list_accounts()).await {
            Ok(Ok(accounts)) => {
                let mode = realtime_mode_for_interval(seconds);
                let message = realtime_status_message(seconds);
                for account in accounts {
                    self.emit_realtime_status(&account, mode, None, None, Some(message.clone()));
                }
            }
            Ok(Err(error)) => warn!("Failed to publish realtime preference status: {error}"),
            Err(error) => warn!("Realtime preference status task failed: {error}"),
        }
    }

    fn poll_interval_secs(&self) -> u64 {
        *self.poll_interval_secs.borrow()
    }

    /// Request cancellation of an in-flight account sync. Workers all share
    /// this watch channel and observe it between network operations.
    pub async fn stop_account(&self, account_id: &str) -> bool {
        let sender = self.stop_signals.lock().await.get(account_id).cloned();
        sender.is_some_and(|sender| sender.send(true).is_ok())
    }

    /// Stop an account and wait until its worker has released the running
    /// slot. A bounded wait prevents account deletion from hanging forever if
    /// an upstream provider ignores cancellation while a socket is blocked.
    pub async fn stop_account_and_wait(&self, account_id: &str, timeout: Duration) -> bool {
        let _ = self.stop_account(account_id).await;
        tokio::time::timeout(timeout, async {
            loop {
                if !self
                    .running
                    .lock()
                    .await
                    .get(account_id)
                    .copied()
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok()
    }

    /// 启动定时同步循环（main 启动时调用一次）。
    pub fn spawn(self: &Arc<Self>) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut poll_rx = mgr.poll_interval_secs.subscribe();
            if *poll_rx.borrow() > 0 {
                mgr.sync_all().await;
            }
            let mut next_sync =
                Box::pin(tokio::time::sleep(sync_sleep_duration(*poll_rx.borrow())));
            loop {
                tokio::select! {
                    _ = &mut next_sync => {
                        if *poll_rx.borrow() > 0 {
                            mgr.sync_all().await;
                        }
                        next_sync.as_mut().reset(Instant::now() + sync_sleep_duration(*poll_rx.borrow()));
                    }
                    changed = poll_rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        next_sync.as_mut().reset(Instant::now() + sync_sleep_duration(*poll_rx.borrow()));
                    }
                }
            }
        });

        tokio::spawn(crate::snooze_watcher::run_snooze_watcher(
            self.store.clone(),
            self.ws_broadcast.clone(),
        ));
    }


    /// 同步全部账户（间隔触发）。
    pub async fn sync_all(&self) {
        let accounts = match self.store.list_accounts() {
            Ok(a) => a,
            Err(e) => {
                error!("SyncManager: failed to list accounts: {e}");
                return;
            }
        };
        for account in accounts {
            if let Err(e) = self.sync_account(&account).await {
                warn!("Sync failed for account {}: {e}", account.id);
            }
        }
    }

    /// 同步单个账户（防重入）。
    pub async fn sync_account(&self, account: &Account) -> Result<(), PebbleError> {
        let (stop_tx, stop_rx) = watch::channel(false);
        {
            let mut running = self.running.lock().await;
            if running.get(&account.id).copied().unwrap_or(false) {
                return Ok(());
            }
            self.stop_signals
                .lock()
                .await
                .insert(account.id.clone(), stop_tx);
            running.insert(account.id.clone(), true);
        }

        let interval = self.poll_interval_secs();
        self.emit_realtime_status(
            account,
            realtime_mode_for_interval(interval),
            None,
            None,
            Some(realtime_status_message(interval)),
        );

        let result = self.sync_account_inner(account, stop_rx).await;

        match &result {
            Ok(()) => self.emit_realtime_status(
                account,
                realtime_mode_for_interval(interval),
                Some(pebble_core::now_timestamp()),
                None,
                Some(realtime_status_message(interval)),
            ),
            Err(error) => {
                self.emit_realtime_status(account, "error", None, None, Some(error.to_string()))
            }
        }

        self.stop_signals.lock().await.remove(&account.id);
        let mut running = self.running.lock().await;
        running.remove(&account.id);
        result
    }

    async fn sync_account_inner(
        &self,
        account: &Account,
        stop_rx: watch::Receiver<bool>,
    ) -> Result<(), PebbleError> {
        match account.provider {
            ProviderType::Imap => {
                info!("sync start: account {} (imap)", account.id);
                self.emit(
                    events::MAIL_SYNC_PROGRESS,
                    &account.id,
                    json!({
                        "status": "started",
                        "phase": "initial",
                    }),
                );

                let imap_config = crate::commands::messages::load_imap_config(&self.store, &self.crypto, &account.id)?;
                let provider = Arc::new(ImapMailProvider::new(imap_config));

                let (error_tx, mut error_rx) =
                    tokio::sync::mpsc::unbounded_channel::<pebble_mail::SyncError>();
                let (message_tx, mut message_rx) =
                    tokio::sync::mpsc::unbounded_channel::<pebble_mail::StoredMessage>();
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::unbounded_channel::<pebble_mail::SyncProgress>();

                let broadcast = self.ws_broadcast.clone();
                let account_id = account.id.clone();
                let provider_slug = provider_slug(&account.provider).to_string();
                let search = self.search.clone();
                let store = self.store.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            Some(err) = error_rx.recv() => {
                                let error_mode = realtime_error_mode(&err);
                                let error_message = err.message.clone();
                                let _ = broadcast.send(json!({
                                    "type": events::MAIL_ERROR,
                                    "account_id": account_id,
                                    "payload": json!({
                                        "message": err.message,
                                        "error_type": err.error_type,
                                    }),
                                }).to_string());
                                let _ = broadcast.send(json!({
                                    "type": events::MAIL_REALTIME_STATUS,
                                    "account_id": account_id,
                                    "payload": realtime_status_payload(
                                        &account_id,
                                        &provider_slug,
                                        error_mode,
                                        None,
                                        None,
                                        Some(error_message),
                                    ),
                                }).to_string());
                            }
                            Some(stored) = message_rx.recv() => {
                                // 非 reconciliation 的新消息 → 发 mail:new 事件（对齐桌面端 indexing 语义，
                                // payload 结构一致：account_id/message_id/folder_ids）
                                if !stored.reconciliation {
                                    let payload = json!({
                                        "account_id": stored.message.account_id,
                                        "message_id": stored.message.id,
                                        "folder_ids": stored.folder_ids,
                                        "thread_id": stored.message.thread_id,
                                        "subject": stored.message.subject,
                                        "from": stored.message.from_address,
                                        "received_at": stored.message.date,
                                    });
                                    let _ = broadcast.send(json!({
                                        "type": events::MAIL_NEW,
                                        "account_id": stored.message.account_id,
                                        "payload": payload,
                                    }).to_string());
                                }
                                crate::commands::indexing::index_stored_message(&search, &store, &stored).await;
                            }
                            Some(progress) = progress_rx.recv() => {
                                let _ = broadcast.send(json!({
                                    "type": events::MAIL_SYNC_PROGRESS,
                                    "account_id": progress.account_id,
                                    "payload": json!({
                                        "status": progress.status,
                                        "phase": progress.phase,
                                        "message": progress.message,
                                    }),
                                }).to_string());
                            }
                            else => break,
                        }
                    }
                });

                let mut config = SyncConfig::default();
                config.poll_interval_secs = 0; // manual_only：单轮同步后断开
                config.reconcile_interval_secs = 86400;

                let worker = SyncWorker::new(
                    account.id.clone(),
                    provider,
                    self.store.clone(),
                    stop_rx,
                    &self.attachments_dir,
                )
                .with_error_tx(error_tx)
                .with_message_tx(message_tx)
                .with_progress_tx(progress_tx);

                worker.run(config, None).await;

                self.emit(
                    events::MAIL_SYNC_COMPLETE,
                    &account.id,
                    json!({
                        "status": "completed",
                        "phase": "initial",
                    }),
                );
                info!("sync done: account {}", account.id);
                Ok(())
            }
            ProviderType::Gmail | ProviderType::Outlook => {
                self.sync_oauth_account(account, stop_rx).await
            }
            ProviderType::Pop3 => self.sync_pop3_account(account, stop_rx).await,
        }
    }

    /// Run one manual Gmail/Outlook worker pass. The workers are shared with
    /// the desktop transport; Web only supplies server-side token loading,
    /// refresh persistence, and event forwarding.
    async fn sync_oauth_account(
        &self,
        account: &Account,
        stop_rx: watch::Receiver<bool>,
    ) -> Result<(), PebbleError> {
        let access = crate::oauth::load_oauth_access(&self.crypto, &self.store, &account.id)?;
        let refresher = crate::oauth::build_oauth_token_refresher(
            self.crypto.clone(),
            self.store.clone(),
            match account.provider {
                ProviderType::Gmail => "gmail",
                ProviderType::Outlook => "outlook",
                _ => unreachable!(),
            },
            &access,
            self.oauth_account_locks.clone(),
            &account.id,
        )?;
        let (error_tx, mut error_rx) = mpsc::unbounded_channel::<pebble_mail::SyncError>();
        let (message_tx, mut message_rx) = mpsc::unbounded_channel::<pebble_mail::StoredMessage>();
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<pebble_mail::SyncProgress>();
        let broadcast = self.ws_broadcast.clone();
        let account_id = account.id.clone();
        let provider_slug = provider_slug(&account.provider).to_string();
        let search = self.search.clone();
        let store = self.store.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(err) = error_rx.recv() => {
                        let error_mode = realtime_error_mode(&err);
                        let error_message = err.message.clone();
                        let _ = broadcast.send(json!({
                            "type": events::MAIL_ERROR,
                            "account_id": account_id,
                            "payload": {"message": err.message, "error_type": err.error_type},
                        }).to_string());
                        let _ = broadcast.send(json!({
                            "type": events::MAIL_REALTIME_STATUS,
                            "account_id": account_id,
                            "payload": realtime_status_payload(
                                &account_id,
                                &provider_slug,
                                error_mode,
                                None,
                                None,
                                Some(error_message),
                            ),
                        }).to_string());
                    }
                    Some(stored) = message_rx.recv() => {
                        if !stored.reconciliation {
                            let payload = json!({
                                "account_id": stored.message.account_id,
                                "message_id": stored.message.id,
                                "folder_ids": stored.folder_ids,
                                "thread_id": stored.message.thread_id,
                                "subject": stored.message.subject,
                                "from": stored.message.from_address,
                                "received_at": stored.message.date,
                            });
                            let _ = broadcast.send(json!({
                                "type": events::MAIL_NEW,
                                "account_id": stored.message.account_id,
                                "payload": payload,
                            }).to_string());
                        }
                        crate::commands::indexing::index_stored_message(&search, &store, &stored).await;
                    }
                    Some(progress) = progress_rx.recv() => {
                        let _ = broadcast.send(json!({
                            "type": events::MAIL_SYNC_PROGRESS,
                            "account_id": progress.account_id,
                            "payload": {
                                "status": progress.status,
                                "phase": progress.phase,
                                "message": progress.message,
                            },
                        }).to_string());
                    }
                    else => break,
                }
            }
        });

        let mut config = SyncConfig::default();
        config.poll_interval_secs = 0;
        config.reconcile_interval_secs = 86400;
        match account.provider {
            ProviderType::Gmail => {
                let provider = Arc::new(GmailProvider::new_with_proxy(
                    access.access_token.clone(),
                    access.proxy.clone(),
                )?);
                let mut worker = GmailSyncWorker::new(
                    account.id.clone(),
                    provider,
                    self.store.clone(),
                    stop_rx,
                    &self.attachments_dir,
                )
                .with_error_tx(error_tx)
                .with_message_tx(message_tx)
                .with_progress_tx(progress_tx);
                if let Some(refresher) = refresher {
                    worker = worker.with_token_refresher(refresher, access.expires_at);
                }
                worker.run(config, None).await;
            }
            ProviderType::Outlook => {
                let provider = Arc::new(OutlookProvider::new_with_proxy(
                    access.access_token.clone(),
                    account.id.clone(),
                    access.proxy.clone(),
                )?);
                let mut worker = OutlookSyncWorker::new(
                    account.id.clone(),
                    provider,
                    self.store.clone(),
                    &self.attachments_dir,
                )
                .with_error_tx(error_tx)
                .with_message_tx(message_tx)
                .with_progress_tx(progress_tx);
                if let Some(refresher) = refresher {
                    worker = worker.with_token_refresher(refresher, access.expires_at);
                }
                worker.run(config, stop_rx, None).await;
            }
            _ => unreachable!(),
        }
        self.emit(
            events::MAIL_SYNC_COMPLETE,
            &account.id,
            json!({"status": "completed", "phase": "oauth"}),
        );
        Ok(())
    }

    async fn sync_pop3_account(
        &self,
        account: &Account,
        stop_rx: watch::Receiver<bool>,
    ) -> Result<(), PebbleError> {
        let config = crate::commands::messages::load_pop3_config(&self.store, &self.crypto, &account.id)?;
        let provider = Arc::new(Pop3Provider::new(config));
        let (error_tx, mut error_rx) = mpsc::unbounded_channel::<pebble_mail::SyncError>();
        let (message_tx, mut message_rx) = mpsc::unbounded_channel::<pebble_mail::StoredMessage>();
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<pebble_mail::SyncProgress>();
        let broadcast = self.ws_broadcast.clone();
        let account_id = account.id.clone();
        let provider_slug = provider_slug(&account.provider).to_string();
        let search = self.search.clone();
        let store = self.store.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(err) = error_rx.recv() => {
                        let error_mode = realtime_error_mode(&err);
                        let error_message = err.message.clone();
                        let _ = broadcast.send(json!({
                            "type": events::MAIL_ERROR,
                            "account_id": account_id,
                            "payload": {"message": err.message, "error_type": err.error_type},
                        }).to_string());
                        let _ = broadcast.send(json!({
                            "type": events::MAIL_REALTIME_STATUS,
                            "account_id": account_id,
                            "payload": realtime_status_payload(
                                &account_id,
                                &provider_slug,
                                error_mode,
                                None,
                                None,
                                Some(error_message),
                            ),
                        }).to_string());
                    }
                    Some(stored) = message_rx.recv() => {
                        if !stored.reconciliation {
                            let _ = broadcast.send(json!({
                                "type": events::MAIL_NEW,
                                "account_id": stored.message.account_id,
                                "payload": {
                                    "account_id": stored.message.account_id,
                                    "message_id": stored.message.id,
                                    "folder_ids": stored.folder_ids,
                                    "thread_id": stored.message.thread_id,
                                    "subject": stored.message.subject,
                                    "from": stored.message.from_address,
                                    "received_at": stored.message.date,
                                },
                            }).to_string());
                        }
                        crate::commands::indexing::index_stored_message(&search, &store, &stored).await;
                    }
                    Some(progress) = progress_rx.recv() => {
                        let _ = broadcast.send(json!({
                            "type": events::MAIL_SYNC_PROGRESS,
                            "account_id": progress.account_id,
                            "payload": {
                                "status": progress.status,
                                "phase": progress.phase,
                                "message": progress.message,
                            },
                        }).to_string());
                    }
                    else => break,
                }
            }
        });
        let mut sync_config = SyncConfig::default();
        sync_config.poll_interval_secs = 0;
        sync_config.reconcile_interval_secs = 86400;
        let worker = Pop3SyncWorker::new(
            account.id.clone(),
            provider,
            self.store.clone(),
            stop_rx,
            self.attachments_dir.clone(),
        )
        .with_error_tx(error_tx)
        .with_message_tx(message_tx)
        .with_progress_tx(progress_tx);
        worker.run(sync_config, None).await;
        self.emit(
            events::MAIL_SYNC_COMPLETE,
            &account.id,
            json!({"status": "completed", "phase": "pop3"}),
        );
        Ok(())
    }

    fn emit(&self, event_type: &str, account_id: &str, detail: Value) {
        // 事件名直接作为 type（与桌面端 Tauri event 名一致），不再拼前缀
        let _ = self.ws_broadcast.send(
            json!({
                "type": event_type,
                "account_id": account_id,
                "payload": detail,
            })
            .to_string(),
        );
    }

    fn emit_realtime_status(
        &self,
        account: &Account,
        mode: &str,
        last_success_at: Option<i64>,
        next_retry_at: Option<i64>,
        message: Option<String>,
    ) {
        self.emit(
            events::MAIL_REALTIME_STATUS,
            &account.id,
            realtime_status_payload(
                &account.id,
                provider_slug(&account.provider),
                mode,
                last_success_at,
                next_retry_at,
                message,
            ),
        );
    }
}

fn provider_slug(provider: &ProviderType) -> &'static str {
    match provider {
        ProviderType::Imap => "imap",
        ProviderType::Pop3 => "pop3",
        ProviderType::Gmail => "gmail",
        ProviderType::Outlook => "outlook",
    }
}

fn realtime_mode_for_interval(seconds: u64) -> &'static str {
    if seconds == 0 {
        "manual"
    } else {
        "polling"
    }
}

fn realtime_status_message(seconds: u64) -> String {
    if seconds == 0 {
        "Manual only".to_string()
    } else {
        format!("Polling every {seconds}s")
    }
}

fn realtime_status_payload(
    account_id: &str,
    provider: &str,
    mode: &str,
    last_success_at: Option<i64>,
    next_retry_at: Option<i64>,
    message: Option<String>,
) -> Value {
    json!({
        "account_id": account_id,
        "mode": mode,
        "provider": provider,
        "last_success_at": last_success_at,
        "next_retry_at": next_retry_at,
        "message": message,
    })
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

fn sync_sleep_duration(seconds: u64) -> Duration {
    if seconds == 0 {
        // Manual mode has no automatic wake-up; the watch channel interrupts
        // this sleep immediately when a new preference is selected.
        Duration::from_secs(24 * 60 * 60)
    } else {
        Duration::from_secs(seconds)
    }
}
