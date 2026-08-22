use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::AppStateRef;

use pebble_core::{Message, PebbleError, ProviderType};

pub(crate) fn queue_pending_for_store(
    store: &pebble_store::Store,
    message: &Message,
    op_type: &str,
    payload: Value,
) -> Result<(), PebbleError> {
    if store
        .get_account(&message.account_id)?
        .is_some_and(|account| matches!(account.provider, ProviderType::Pop3))
    {
        tracing::debug!(
            account_id = %message.account_id,
            op_type,
            "skipping remote POP3 mutation"
        );
        return Ok(());
    }

    let payload = serde_json::json!({
        "provider_account_id": message.account_id,
        "remote_id": message.remote_id,
        "op": op_type,
        "payload": payload,
    });
    let op_id = store.insert_pending_mail_op(
        &message.account_id,
        &message.id,
        op_type,
        &payload.to_string(),
    )?;
    store.mark_pending_mail_op_failed(&op_id, "queued for background sync")?;
    Ok(())
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
