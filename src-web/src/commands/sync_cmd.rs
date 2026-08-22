use serde_json::{json, Value};

use crate::blocking::run_blocking;
use crate::commands::indexing;
use crate::error::ApiError;
use crate::state::AppStateRef;

fn realtime_preference_interval(mode: &str) -> Result<u64, ApiError> {
    match mode {
        "realtime" => Ok(3),
        "balanced" => Ok(15),
        "battery" => Ok(60),
        "manual" => Ok(0),
        other => Err(ApiError::BadRequest(format!(
            "invalid realtime preference: {other}"
        ))),
    }
}
/// 手动触发一次同步（立即执行，不等定时周期）。reason 兼容桌面端 SyncTrigger 语义。
pub async fn trigger_sync(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        #[serde(default)]
        reason: String,
    }
    let a: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid trigger_sync args: {e}")))?;

    let account = state
        .store
        .get_account(&a.account_id)
        .map_err(ApiError::from_store)?
        .ok_or_else(|| ApiError::NotFound(format!("account not found: {}", a.account_id)))?;

    let sync = state.sync_manager.clone();
    tokio::spawn(async move {
        if let Err(e) = sync.sync_account(&account).await {
            tracing::warn!("triggered sync failed for {}: {e}", account.id);
        }
    });
    Ok(json!({ "started": true, "reason": a.reason }))
}

/// start_sync：Web 端由全局调度器管理，命令语义等价于触发一次立即同步。
pub async fn start_sync(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        #[serde(default)]
        poll_interval_secs: Option<u64>,
    }
    let a: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid start_sync args: {e}")))?;
    let _ = a.poll_interval_secs;

    let account = state
        .store
        .get_account(&a.account_id)
        .map_err(ApiError::from_store)?
        .ok_or_else(|| ApiError::NotFound(format!("account not found: {}", a.account_id)))?;

    let sync = state.sync_manager.clone();
    tokio::spawn(async move {
        if let Err(e) = sync.sync_account(&account).await {
            tracing::warn!("start sync failed for {}: {e}", account.id);
        }
    });
    Ok(json!({ "started": true }))
}

/// stop_sync：向当前账户 worker 发送取消信号；空闲账户保持幂等成功。
pub async fn stop_sync(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
    }
    let a: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid stop_sync args: {e}")))?;
    let stopped = state.sync_manager.stop_account(&a.account_id).await;
    Ok(json!({
        "stopped": true,
        "account_id": a.account_id,
        "running_worker_cancelled": stopped
    }))
}

/// Apply the Web scheduler's automatic sync preference.
pub async fn set_realtime_preference(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        mode: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid set_realtime_preference args: {e}")))?;
    let interval = realtime_preference_interval(&args.mode)?;
    state.sync_manager.set_poll_interval_secs(interval);
    state
        .sync_manager
        .publish_realtime_preference_status(interval)
        .await;
    Ok(Value::Null)
}

/// Rebuild the search index from all messages currently in the store.
pub async fn reindex_search(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let search = state.search.clone();
    let count = run_blocking(move || indexing::do_reindex(&store, &search)).await?;
    Ok(json!(count))
}
