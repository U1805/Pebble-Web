use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pebble_core::{Account, PebbleError, ProviderType};
use pebble_crypto::CryptoService;
use pebble_mail::{ImapMailProvider, SyncConfig, SyncWorker};
use pebble_search::TantivySearch;
use pebble_store::Store;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex};
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
    interval: Duration,
    ws_broadcast: broadcast::Sender<String>,
    running: Mutex<HashMap<String, bool>>,
}

/// 事件名与桌面端 Tauri event 对齐（计划书 §22），前端调用层 events 统一订阅。
mod event {
    pub const SYNC_PROGRESS: &str = "mail:sync-progress";
    pub const SYNC_COMPLETE: &str = "mail:sync-complete";
    pub const ERROR: &str = "mail:error";
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
        Self {
            store,
            search,
            crypto,
            attachments_dir,
            interval: Duration::from_secs(sync_interval_secs.max(10)),
            ws_broadcast,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// 启动定时同步循环（main 启动时调用一次）。
    pub fn spawn(self: &Arc<Self>) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(mgr.interval);
            ticker.tick().await; // 首次立即执行一次
            loop {
                mgr.sync_all().await;
                ticker.tick().await;
            }
        });
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
        {
            let mut running = self.running.lock().await;
            if running.get(&account.id).copied().unwrap_or(false) {
                return Ok(());
            }
            running.insert(account.id.clone(), true);
        }

        let result = self.sync_account_inner(account).await;

        let mut running = self.running.lock().await;
        running.remove(&account.id);
        result
    }

    async fn sync_account_inner(&self, account: &Account) -> Result<(), PebbleError> {
        match account.provider {
            ProviderType::Imap => {
                info!("sync start: account {} (imap)", account.id);
                self.emit(event::SYNC_PROGRESS, &account.id, json!({
                    "status": "started",
                    "phase": "initial",
                }));

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
                let search = self.search.clone();
                let store = self.store.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            Some(err) = error_rx.recv() => {
                                let _ = broadcast.send(json!({
                                    "type": event::ERROR,
                                    "account_id": account_id,
                                    "payload": json!({
                                        "message": err.message,
                                        "error_type": err.error_type,
                                    }),
                                }).to_string());
                            }
                            Some(stored) = message_rx.recv() => {
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

                let (stop_tx, stop_rx) = watch::channel(false);
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

                // 事件转发任务随 worker 结束自然终止
                let _ = stop_tx;
                worker.run(config, None).await;

                self.emit(event::SYNC_COMPLETE, &account.id, json!({
                    "status": "completed",
                    "phase": "initial",
                }));
                info!("sync done: account {}", account.id);
                Ok(())
            }
            ProviderType::Pop3 | ProviderType::Gmail | ProviderType::Outlook => {
                warn!(
                    "sync skipped for account {}: provider {:?} not in Web P0 scope",
                    account.id, account.provider
                );
                Ok(())
            }
        }
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
}

/// 从加密 auth_data 解析 IMAP 配置（与桌面端 load_imap_config 同语义）。
fn load_imap_config(
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
    serde_json::from_value(imap_value)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse IMAP config: {e}")))
}

/// 新同步消息入搜索索引（简化版：逐条 add + index + commit）。
async fn index_stored_message(
    search: &Arc<TantivySearch>,
    store: &Arc<Store>,
    stored: &pebble_mail::StoredMessage,
) {
    let search = search.clone();
    let store = store.clone();
    let message = stored.message.clone();
    let folder_ids = stored.folder_ids.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<(), PebbleError> {
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