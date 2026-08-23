use pebble_core::PebbleError;

use crate::error::ApiError;

/// store 阻塞调用包装：SQLite/磁盘 I/O 不得阻塞 async 主线程（与桌面端 spawn_blocking 一致）。
pub(crate) async fn run_blocking<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PebbleError> + Send + 'static,
{
    run_blocking_core(f).await.map_err(ApiError::from_pebble)
}

/// Run blocking core work without converting its business error into an HTTP error.
pub(crate) async fn run_blocking_core<T, F>(f: F) -> Result<T, PebbleError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PebbleError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| PebbleError::Internal(format!("Task join error: {e}")))?
}
