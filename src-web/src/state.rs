use std::sync::Arc;

use crate::config::Config;

/// Web 服务共享状态。
///
/// 阶段三：仅持配置。后续阶段按需挂载：
/// - store / search / crypto（阶段四）
/// - sync_manager / ws_broadcast（阶段六）
/// 参考桌面端 src-tauri 的 AppState 注入模式保持一致。
pub struct AppState {
    pub config: Config,
}

pub type AppStateRef = Arc<AppState>;

impl AppState {
    pub fn init(config: Config) -> AppStateRef {
        Arc::new(Self { config })
    }
}