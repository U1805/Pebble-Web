use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::AppStateRef;
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

/// stop_sync：Web 端无长驻连接（每轮同步即断开），命令保持兼容返回提示。
pub async fn stop_sync(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
    }
    let a: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid stop_sync args: {e}")))?;
    let _ = state;
    Ok(json!({
        "stopped": true,
        "account_id": a.account_id,
        "note": "web sync is scheduler-managed; no persistent connection to stop"
    }))
}

/// 待处理操作统计（失败/重试队列）。
pub async fn get_pending_mail_ops_summary(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let account_id: String = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid args: {e}")))?;
    let s = state
        .store
        .pending_mail_ops_summary(Some(&account_id))
        .map_err(ApiError::from_store)?;
    Ok(json!({
        "pending_count": s.pending_count,
        "in_progress_count": s.in_progress_count,
        "failed_count": s.failed_count,
        "total_active_count": s.total_active_count,
        "last_error": s.last_error,
        "updated_at": s.updated_at,
    }))
}

/// 列出账户的待处理操作。
pub async fn list_pending_mail_ops(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let account_id: String = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid args: {e}")))?;
    let ops = state
        .store
        .list_pending_mail_ops(&account_id)
        .map_err(ApiError::from_store)?;
    let items: Vec<Value> = ops
        .iter()
        .map(|op| {
            json!({
                "id": op.id,
                "account_id": op.account_id,
                "message_id": op.message_id,
                "op_type": op.op_type,
                "payload_json": op.payload_json,
                "status": op.status.as_str(),
                "attempts": op.attempts,
                "last_error": op.last_error,
                "created_at": op.created_at,
                "updated_at": op.updated_at,
                "next_retry_at": op.next_retry_at,
            })
        })
        .collect();
    Ok(Value::Array(items))
}

/// 清理失败状态的操作（用户认可后）。
pub async fn dismiss_failed_pending_mail_ops(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    let account_id: Option<String> = serde_json::from_value(args).ok();
    let dismissed = state
        .store
        .dismiss_failed_pending_mail_ops(account_id.as_deref())
        .map_err(ApiError::from_store)?;
    Ok(json!({ "dismissed": dismissed }))
}