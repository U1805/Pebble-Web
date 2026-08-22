use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pebble_core::{Account, FolderRole, KanbanCard, Message, PebbleError, ProviderType};
use pebble_crypto::CryptoService;
use pebble_mail::{
    GmailProvider, GmailSyncWorker, ImapMailProvider, OutlookProvider, OutlookSyncWorker,
    Pop3Provider, Pop3SyncWorker, SyncConfig, SyncWorker,
};
use pebble_rules::{types::RuleAction, RuleEngine};
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
    attachments_dir: PathBuf,
    poll_interval_secs: watch::Sender<u64>,
    ws_broadcast: broadcast::Sender<String>,
    running: Mutex<HashMap<String, bool>>,
    stop_signals: Mutex<HashMap<String, watch::Sender<bool>>>,
}

/// 事件名与桌面端 Tauri event 对齐（计划书 §22），前端调用层 events 统一订阅。
mod event {
    pub const SYNC_PROGRESS: &str = "mail:sync-progress";
    pub const SYNC_COMPLETE: &str = "mail:sync-complete";
    pub const ERROR: &str = "mail:error";
    pub const MAIL_NEW: &str = "mail:new";
    pub const REALTIME_STATUS: &str = "mail:realtime-status";
}

impl SyncManager {
    pub fn new(
        store: Arc<Store>,
        search: Arc<TantivySearch>,
        crypto: Arc<CryptoService>,
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

        let snooze_mgr = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                snooze_mgr.process_due_snoozes().await;
            }
        });
    }

    /// Expire snoozed messages on the same cadence as the desktop watcher and
    /// broadcast the shared `mail:unsnoozed` event to WebSocket subscribers.
    async fn process_due_snoozes(&self) {
        let store = self.store.clone();
        let due = match tokio::task::spawn_blocking(move || {
            store.get_due_snoozed(pebble_core::now_timestamp())
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
            let store = self.store.clone();
            let message_id = snoozed.message_id.clone();
            match tokio::task::spawn_blocking(move || store.unsnooze_message(&message_id)).await {
                Ok(Ok(())) => {
                    let _ = self.ws_broadcast.send(
                        json!({
                            "type": "mail:unsnoozed",
                            "payload": {
                                "message_id": snoozed.message_id,
                                "return_to": snoozed.return_to,
                            },
                        })
                        .to_string(),
                    );
                }
                Ok(Err(error)) => warn!("Failed to unsnooze message: {error}"),
                Err(error) => warn!("Snooze unsnooze task error: {error}"),
            }
        }
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
                    event::SYNC_PROGRESS,
                    &account.id,
                    json!({
                        "status": "started",
                        "phase": "initial",
                    }),
                );

                let imap_config = load_imap_config(&self.store, &self.crypto, &account.id)?;
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
                                    "type": event::ERROR,
                                    "account_id": account_id,
                                    "payload": json!({
                                        "message": err.message,
                                        "error_type": err.error_type,
                                    }),
                                }).to_string());
                                let _ = broadcast.send(json!({
                                    "type": event::REALTIME_STATUS,
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
                                        "type": event::MAIL_NEW,
                                        "account_id": stored.message.account_id,
                                        "payload": payload,
                                    }).to_string());
                                }
                                index_stored_message(&search, &store, &stored).await;
                            }
                            Some(progress) = progress_rx.recv() => {
                                let _ = broadcast.send(json!({
                                    "type": event::SYNC_PROGRESS,
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
                    event::SYNC_COMPLETE,
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
                            "type": event::ERROR,
                            "account_id": account_id,
                            "payload": {"message": err.message, "error_type": err.error_type},
                        }).to_string());
                        let _ = broadcast.send(json!({
                            "type": event::REALTIME_STATUS,
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
                                "type": event::MAIL_NEW,
                                "account_id": stored.message.account_id,
                                "payload": payload,
                            }).to_string());
                        }
                        index_stored_message(&search, &store, &stored).await;
                    }
                    Some(progress) = progress_rx.recv() => {
                        let _ = broadcast.send(json!({
                            "type": event::SYNC_PROGRESS,
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
            event::SYNC_COMPLETE,
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
        let config = load_pop3_config(&self.store, &self.crypto, &account.id)?;
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
                            "type": event::ERROR,
                            "account_id": account_id,
                            "payload": {"message": err.message, "error_type": err.error_type},
                        }).to_string());
                        let _ = broadcast.send(json!({
                            "type": event::REALTIME_STATUS,
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
                                "type": event::MAIL_NEW,
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
                        index_stored_message(&search, &store, &stored).await;
                    }
                    Some(progress) = progress_rx.recv() => {
                        let _ = broadcast.send(json!({
                            "type": event::SYNC_PROGRESS,
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
            event::SYNC_COMPLETE,
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
            event::REALTIME_STATUS,
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

/// 从加密 auth_data 解析 IMAP 配置（与桌面端 load_imap_config 同语义）。
pub(crate) fn load_imap_config(
    store: &Store,
    crypto: &CryptoService,
    account_id: &str,
) -> Result<pebble_mail::ImapConfig, PebbleError> {
    let Some(encrypted) = store
        .get_auth_data(account_id)
        .map_err(|e| PebbleError::Internal(e.to_string()))?
    else {
        return Err(PebbleError::Internal(format!(
            "No auth_data for account {account_id}"
        )));
    };
    let plaintext = crypto
        .decrypt_for(
            crate::credentials::ACCOUNT_AUTH_DATA_PURPOSE,
            account_id,
            &encrypted,
        )
        .map_err(|e| PebbleError::Internal(e.to_string()))?;
    let value: Value = serde_json::from_slice(&plaintext)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse auth_data: {e}")))?;
    let imap_value = value.get("imap").cloned().unwrap_or(value.clone());
    let mut config: pebble_mail::ImapConfig = serde_json::from_value(imap_value)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse IMAP config: {e}")))?;
    // 全局代理语义（Inherit→账户未设代理时用全局）：与桌面端消息装配点一致
    let mode = crate::command::network::account_proxy_mode_from_auth_value(&value);
    config.proxy =
        crate::command::network::resolve_mail_proxy_from_mode(store, crypto, mode, config.proxy)?;
    Ok(config)
}

/// 从加密 auth_data 解析 POP3 配置。Web 账户表单复用 incoming 字段，
/// 因此 POP3 配置与桌面端旧数据一样存储在 `imap` 对象中。
pub(crate) fn load_pop3_config(
    store: &Store,
    crypto: &CryptoService,
    account_id: &str,
) -> Result<pebble_mail::Pop3Config, PebbleError> {
    let Some(encrypted) = store
        .get_auth_data(account_id)
        .map_err(|e| PebbleError::Internal(e.to_string()))?
    else {
        return Err(PebbleError::Internal(format!(
            "No auth_data for account {account_id}"
        )));
    };
    let plaintext = crypto
        .decrypt_for(
            crate::credentials::ACCOUNT_AUTH_DATA_PURPOSE,
            account_id,
            &encrypted,
        )
        .map_err(|e| PebbleError::Internal(e.to_string()))?;
    let value: Value = serde_json::from_slice(&plaintext)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse auth_data: {e}")))?;
    let imap_value = value.get("imap").cloned().unwrap_or(value.clone());
    let mut config: pebble_mail::Pop3Config = serde_json::from_value(imap_value)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse POP3 config: {e}")))?;
    let mode = crate::command::network::account_proxy_mode_from_auth_value(&value);
    config.proxy =
        crate::command::network::resolve_mail_proxy_from_mode(store, crypto, mode, config.proxy)?;
    Ok(config)
}

fn apply_web_rule_action(
    store: &Store,
    message: &Message,
    action: &RuleAction,
) -> Result<(), PebbleError> {
    let source_folder = || -> Result<Option<pebble_core::Folder>, PebbleError> {
        let folder_ids = store.get_message_folder_ids(&message.id)?;
        let folders = store.list_folders(&message.account_id)?;
        Ok(folder_ids
            .iter()
            .find_map(|id| folders.iter().find(|folder| &folder.id == id).cloned()))
    };

    match action {
        RuleAction::MarkRead => {
            store.update_message_flags(&message.id, Some(true), None)?;
            crate::command::lifecycle::queue_pending_for_store(
                store,
                message,
                "update_flags",
                json!({ "is_read": true, "is_starred": null }),
            )?;
        }
        RuleAction::Archive => {
            let source = source_folder()?;
            if let Some(archive) =
                store.find_folder_by_role(&message.account_id, FolderRole::Archive)?
            {
                store.move_message_to_folder(&message.id, &archive.id)?;
                crate::command::lifecycle::queue_pending_for_store(
                    store,
                    message,
                    "archive",
                    json!({
                        "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                        "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                        "target_folder_id": archive.id,
                        "target_folder_remote_id": archive.remote_id,
                    }),
                )?;
            } else {
                store.soft_delete_message(&message.id)?;
                crate::command::lifecycle::queue_pending_for_store(
                    store,
                    message,
                    "archive",
                    json!({
                        "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                        "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                        "trash_or_soft_delete": true,
                    }),
                )?;
            }
        }
        RuleAction::AddLabel(label) => {
            store.add_label(&message.id, label)?;
        }
        RuleAction::MoveToFolder(folder_name) => {
            let Some(target) = store.find_folder_by_name(&message.account_id, folder_name)? else {
                warn!(
                    message_id = %message.id,
                    account_id = %message.account_id,
                    folder = %folder_name,
                    "Rule target folder not found"
                );
                return Ok(());
            };
            let source = source_folder()?;
            store.move_message_to_folder(&message.id, &target.id)?;
            crate::command::lifecycle::queue_pending_for_store(
                store,
                message,
                "move_to_folder",
                json!({
                    "source_folder_id": source.as_ref().map(|folder| folder.id.as_str()),
                    "source_folder_remote_id": source.as_ref().map(|folder| folder.remote_id.as_str()),
                    "target_folder_id": target.id,
                    "target_folder_remote_id": target.remote_id,
                }),
            )?;
        }
        RuleAction::SetKanbanColumn(column) => {
            let now = pebble_core::now_timestamp();
            store.upsert_kanban_card(&KanbanCard {
                message_id: message.id.clone(),
                column: column.clone(),
                position: 0,
                created_at: now,
                updated_at: now,
            })?;
        }
    }
    Ok(())
}

fn apply_web_rules(store: &Store, message: &Message) -> Result<(), PebbleError> {
    let rules = store.list_rules()?;
    let engine = RuleEngine::new(&rules);
    for action in engine.evaluate(message) {
        apply_web_rule_action(store, message, &action)?;
    }
    Ok(())
}

/// 新同步消息执行规则并入搜索索引（逐条 add + index + commit）。
async fn index_stored_message(
    search: &Arc<TantivySearch>,
    store: &Arc<Store>,
    stored: &pebble_mail::StoredMessage,
) {
    let search = search.clone();
    let store = store.clone();
    let mut message = stored.message.clone();
    let mut folder_ids = stored.folder_ids.clone();
    let apply_rules = !stored.reconciliation;
    let outcome = tokio::task::spawn_blocking(move || -> Result<(), PebbleError> {
        if apply_rules {
            if let Err(error) = apply_web_rules(&store, &message) {
                warn!(message_id = %message.id, "Failed to apply Web rules: {error}");
            }
            if let Some(latest) = store.get_message(&message.id)? {
                message = latest;
                folder_ids = store.get_message_folder_ids(&message.id)?;
            }
        }
        let ids = vec![message.id.clone()];
        store.add_search_pending(&ids, "index")?;
        search.index_message(&message, &folder_ids)?;
        search.commit()?;
        store.clear_search_pending(&ids)?;
        Ok(())
    })
    .await;
    if let Err(e) = outcome {
        warn!("Failed to index synced message: {:?}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_status_payload_matches_frontend_contract() {
        let payload = realtime_status_payload(
            "account-1",
            "imap",
            "polling",
            None,
            None,
            Some("Polling every 15s".to_string()),
        );

        assert_eq!(payload["account_id"], "account-1");
        assert_eq!(payload["provider"], "imap");
        assert_eq!(payload["mode"], "polling");
        assert!(payload["last_success_at"].is_null());
        assert_eq!(payload["message"], "Polling every 15s");
    }

    #[test]
    fn realtime_status_payload_records_success_timestamp() {
        let payload = realtime_status_payload(
            "account-1",
            "gmail",
            "polling",
            Some(1_725_000_000),
            None,
            Some("Polling every 15s".to_string()),
        );

        assert_eq!(payload["last_success_at"], 1_725_000_000);
        assert!(payload["next_retry_at"].is_null());
    }

    #[test]
    fn realtime_preference_status_uses_manual_or_polling_mode() {
        assert_eq!(realtime_mode_for_interval(0), "manual");
        assert_eq!(realtime_status_message(0), "Manual only");
        assert_eq!(realtime_mode_for_interval(60), "polling");
        assert_eq!(realtime_status_message(60), "Polling every 60s");
    }

    #[test]
    fn realtime_error_mode_classifies_auth_and_network_failures() {
        let auth = pebble_mail::SyncError {
            error_type: "Auth".to_string(),
            message: "token expired".to_string(),
            timestamp: 0,
        };
        let offline = pebble_mail::SyncError {
            error_type: "Network".to_string(),
            message: "offline".to_string(),
            timestamp: 0,
        };
        let generic = pebble_mail::SyncError {
            error_type: "Protocol".to_string(),
            message: "bad response".to_string(),
            timestamp: 0,
        };

        assert_eq!(realtime_error_mode(&auth), "auth_required");
        assert_eq!(realtime_error_mode(&offline), "offline");
        assert_eq!(realtime_error_mode(&generic), "error");
    }

    #[tokio::test]
    async fn sync_failure_broadcasts_error_and_releases_retry_slot() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let search = Arc::new(TantivySearch::open_in_memory().unwrap());
        let crypto = Arc::new(CryptoService::from_key([7_u8; 32]));
        let (broadcast, mut events) = broadcast::channel(16);
        let manager = SyncManager::new(
            store.clone(),
            search,
            crypto,
            PathBuf::from("/tmp/pebble-web-sync-test-attachments"),
            15,
            broadcast,
        );
        let account = Account {
            id: "sync-failure-account".to_string(),
            email: "sync-failure@example.com".to_string(),
            display_name: "Sync Failure".to_string(),
            color: None,
            provider: ProviderType::Imap,
            created_at: 0,
            updated_at: 0,
        };
        store.insert_account(&account).unwrap();

        let first_error = manager.sync_account(&account).await.unwrap_err();
        assert!(first_error.to_string().contains("No auth_data"));
        let started: Value = serde_json::from_str(&events.recv().await.unwrap()).unwrap();
        let progress: Value = serde_json::from_str(&events.recv().await.unwrap()).unwrap();
        let failed: Value = serde_json::from_str(&events.recv().await.unwrap()).unwrap();
        assert_eq!(started["type"], event::REALTIME_STATUS);
        assert_eq!(started["payload"]["mode"], "polling");
        assert_eq!(progress["type"], event::SYNC_PROGRESS);
        assert_eq!(progress["payload"]["status"], "started");
        assert_eq!(failed["type"], event::REALTIME_STATUS);
        assert_eq!(failed["payload"]["mode"], "error");
        assert!(failed["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("No auth_data"));

        // The failed worker must release its per-account running slot so a
        // later manual trigger is not silently treated as a duplicate.
        let retry_error = manager.sync_account(&account).await.unwrap_err();
        assert!(retry_error.to_string().contains("No auth_data"));
        assert_eq!(
            serde_json::from_str::<Value>(&events.recv().await.unwrap()).unwrap()["type"],
            event::REALTIME_STATUS
        );
        assert_eq!(
            serde_json::from_str::<Value>(&events.recv().await.unwrap()).unwrap()["type"],
            event::SYNC_PROGRESS
        );
        assert_eq!(
            serde_json::from_str::<Value>(&events.recv().await.unwrap()).unwrap()["payload"]
                ["mode"],
            "error"
        );
    }

    #[tokio::test]
    async fn web_rules_apply_local_actions_before_indexing() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let search = Arc::new(TantivySearch::open_in_memory().unwrap());
        let now = pebble_core::now_timestamp();
        let account = Account {
            id: "rule-account".to_string(),
            email: "rules@example.com".to_string(),
            display_name: "Rules".to_string(),
            color: None,
            provider: ProviderType::Imap,
            created_at: now,
            updated_at: now,
        };
        store.insert_account(&account).unwrap();
        let folder =
            |id: &str, name: &str, remote_id: &str, role: FolderRole| pebble_core::Folder {
                id: id.to_string(),
                account_id: account.id.clone(),
                remote_id: remote_id.to_string(),
                name: name.to_string(),
                folder_type: pebble_core::FolderType::Folder,
                role: Some(role),
                parent_id: None,
                color: None,
                is_system: true,
                sort_order: 0,
            };
        let inbox = folder("rule-inbox", "Inbox", "INBOX", FolderRole::Inbox);
        let archive = folder("rule-archive", "Archive", "Archive", FolderRole::Archive);
        store.insert_folder(&inbox).unwrap();
        store.insert_folder(&archive).unwrap();
        let message = Message {
            id: "rule-message".to_string(),
            account_id: account.id.clone(),
            remote_id: "remote-rule-message".to_string(),
            message_id_header: None,
            in_reply_to: None,
            references_header: None,
            thread_id: None,
            subject: "News update".to_string(),
            snippet: "News update".to_string(),
            from_address: "newsletter@example.com".to_string(),
            from_name: "Newsletter".to_string(),
            to_list: vec![],
            cc_list: vec![],
            bcc_list: vec![],
            body_text: "body".to_string(),
            body_html_raw: String::new(),
            has_attachments: false,
            is_read: false,
            is_starred: false,
            is_draft: false,
            date: now,
            remote_version: None,
            is_deleted: false,
            deleted_at: None,
            created_at: now,
            updated_at: now,
        };
        store
            .insert_message(&message, std::slice::from_ref(&inbox.id))
            .unwrap();
        store
            .insert_rule(&pebble_core::Rule {
                id: "rule-1".to_string(),
                name: "Archive newsletters".to_string(),
                priority: 1,
                conditions: r#"{"operator":"and","conditions":[{"field":"subject","op":"contains","value":"news"}]}"#.to_string(),
                actions: r#"[{"type":"MarkRead"},{"type":"AddLabel","value":"newsletters"},{"type":"Archive"}]"#.to_string(),
                is_enabled: true,
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        index_stored_message(
            &search,
            &store,
            &pebble_mail::StoredMessage {
                message: message.clone(),
                folder_ids: vec![inbox.id.clone()],
                notify: true,
                reconciliation: false,
            },
        )
        .await;

        let updated = store.get_message(&message.id).unwrap().unwrap();
        assert!(updated.is_read);
        assert!(!updated.is_deleted);
        assert_eq!(
            store.get_message_folder_ids(&message.id).unwrap(),
            vec![archive.id]
        );
        assert_eq!(
            store.get_message_labels(&message.id).unwrap()[0].name,
            "newsletters"
        );
        assert_eq!(search.search("News", 10).unwrap().len(), 1);
        let pending = store.list_pending_mail_ops(&account.id).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].op_type, "update_flags");
        assert_eq!(pending[1].op_type, "archive");
        assert!(pending.iter().all(|op| op.status.as_str() == "failed"));
    }
}
