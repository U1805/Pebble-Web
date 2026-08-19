use pebble_crypto::CryptoService;
use pebble_store::Store;
use pebble_search::TantivySearch;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::config::Config;
use crate::crypto;
use crate::sync::SyncManager;

/// Web 服务共享状态。
///
/// 挂载方式与桌面端 src-tauri 的 AppState 保持一致：
/// store/search/crypto 由启动逻辑创建后注入，各命令从 State 取用。
/// sync_manager 驱动后台定时同步，ws_broadcast 推送同步/消息事件（阶段 4.7）。
pub struct AppState {
    pub config: Config,
    pub store: Arc<Store>,
    pub search: Arc<TantivySearch>,
    pub crypto: Arc<CryptoService>,
    pub attachments_dir: PathBuf,
    pub sync_manager: Arc<SyncManager>,
    pub ws_broadcast: broadcast::Sender<String>,
}

pub type AppStateRef = Arc<AppState>;

impl AppState {
    pub fn init(config: Config) -> Result<AppStateRef, String> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|e| format!("Failed to create data dir: {e}"))?;
        std::fs::create_dir_all(config.attachments_dir())
            .map_err(|e| format!("Failed to create attachments dir: {e}"))?;

        let store = Store::open(&config.db_path())
            .map_err(|e| format!("Failed to open store: {e}"))?;
        let search = TantivySearch::open(&config.index_dir())
            .map_err(|e| format!("Failed to open search index: {e}"))?;
        let crypto = crypto::load_or_create_crypto(&config.data_dir)
            .map_err(|e| format!("Failed to init crypto: {e}"))?;

        // 先取路径再 move config，避免借用已移动值
        let attachments_dir = config.attachments_dir();
        let sync_interval = config.sync_interval_secs;

        let store = Arc::new(store);
        let search = Arc::new(search);
        let crypto = Arc::new(crypto);

        let (ws_broadcast, _) = broadcast::channel(100);
        let sync_manager = Arc::new(SyncManager::new(
            store.clone(),
            search.clone(),
            crypto.clone(),
            attachments_dir.clone(),
            sync_interval,
            ws_broadcast.clone(),
        ));

        Ok(Arc::new(Self {
            config,
            store,
            search,
            crypto,
            attachments_dir,
            sync_manager,
            ws_broadcast,
        }))
    }
}