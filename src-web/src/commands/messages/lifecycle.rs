use pebble_core::{Account, Folder, FolderRole, Message, PebbleError};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::AppStateRef;

use crate::commands::messages::refresh_search_documents;
use crate::events;

/// 操作后广播（同通道由 realtime/mod.rs 推送给所有订阅连接）。
fn emit(state: &AppStateRef, event_type: &str, payload: Value) {
    let _ = state
        .ws_broadcast
        .send(json!({ "type": event_type, "payload": payload }).to_string());
}

/// 归档/移动/删除后：文件夹变化 + 队列变化通知。
pub(in crate::commands) fn emit_folder_and_queue_changed(state: &AppStateRef, message_id: &str) {
    emit(
        state,
        events::MAIL_FOLDER_CHANGED,
        json!({ "message_id": message_id }),
    );
    emit(state, events::MAIL_PENDING_OPS_CHANGED, json!({}));
}

/// 消息生命周期命令（本地即时提交 + 远端写回排队）。
///
/// 策略（2026-08-19）：Web 端不即时连接远程服务器，本地 DB 变更立即生效，
/// 统一插入 pending_mail_ops 排队。远端重放 worker 尚未与上游 pending_mail_ops.rs 对齐。
/// payload 结构对齐桌面端 queue_pending_remote_op（provider_account_id/remote_id/op/payload），
/// 保证 4.6 的写回循环可按同一格式处理。

pub(in crate::commands) fn find_message_context(
    state: &AppStateRef,
    message_id: &str,
) -> Result<(Message, Account, Folder), PebbleError> {
    let msg = state
        .store
        .get_message(message_id)?
        .ok_or_else(|| PebbleError::Internal(format!("Message not found: {message_id}")))?;
    let account = state
        .store
        .get_account(&msg.account_id)?
        .ok_or_else(|| PebbleError::Internal(format!("Account not found: {}", msg.account_id)))?;
    let source_folder = find_message_folder(state, message_id, &msg.account_id)?;
    Ok((msg, account, source_folder))
}

fn find_message_folder(
    state: &AppStateRef,
    message_id: &str,
    account_id: &str,
) -> Result<Folder, PebbleError> {
    let folder_ids = state.store.get_message_folder_ids(message_id)?;
    let folders = state.store.list_folders(account_id)?;
    folder_ids
        .iter()
        .find_map(|fid| folders.iter().find(|f| &f.id == fid).cloned())
        .ok_or_else(|| PebbleError::Internal(format!("No folder for message {message_id}")))
}

pub(in crate::commands) fn find_folder_by_role(
    state: &AppStateRef,
    account_id: &str,
    role: FolderRole,
) -> Result<Option<Folder>, PebbleError> {
    let folders = state.store.list_folders(account_id)?;
    Ok(folders.into_iter().find(|f| f.role.as_ref() == Some(&role)))
}

pub(in crate::commands) fn queue_pending(
    state: &AppStateRef,
    msg: &Message,
    op_type: &str,
    inner_payload: Value,
) -> Result<(), PebbleError> {
    crate::commands::pending_mail_ops::queue_pending_for_store(
        &state.store,
        msg,
        op_type,
        inner_payload,
    )
}

async fn archive_or_unarchive(state: AppStateRef, message_id: String) -> Result<Value, ApiError> {
    let (msg, _account, source_folder) =
        find_message_context(&state, &message_id).map_err(ApiError::from_pebble)?;

    // 已在归档：恢复回收件箱
    if source_folder.role == Some(FolderRole::Archive) {
        let inbox = find_folder_by_role(&state, &msg.account_id, FolderRole::Inbox)
            .map_err(ApiError::from_pebble)?
            .ok_or_else(|| ApiError::BadRequest("no inbox folder".to_string()))?;
        state
            .store
            .move_message_to_folder(&message_id, &inbox.id)
            .map_err(ApiError::from_store)?;
        queue_pending(
            &state,
            &msg,
            "unarchive",
            json!({
                "source_folder_id": &source_folder.id,
                "source_folder_remote_id": &source_folder.remote_id,
                "target_folder_id": &inbox.id,
                "target_folder_remote_id": &inbox.remote_id,
            }),
        )
        .map_err(ApiError::from_pebble)?;
        refresh_search_documents(&state, &[message_id.clone()]).map_err(ApiError::from_pebble)?;
        emit_folder_and_queue_changed(&state, &message_id);
        return Ok(json!({ "status": "unarchived" }));
    }

    match find_folder_by_role(&state, &msg.account_id, FolderRole::Archive)
        .map_err(ApiError::from_pebble)?
    {
        Some(archive_folder) => {
            state
                .store
                .move_message_to_folder(&message_id, &archive_folder.id)
                .map_err(ApiError::from_store)?;
            queue_pending(
                &state,
                &msg,
                "archive",
                json!({
                    "source_folder_id": &source_folder.id,
                    "source_folder_remote_id": &source_folder.remote_id,
                    "target_folder_id": &archive_folder.id,
                    "target_folder_remote_id": &archive_folder.remote_id,
                }),
            )
            .map_err(ApiError::from_pebble)?;
        }
        // 无归档文件夹：软删除（与桌面端一致）
        None => {
            state
                .store
                .soft_delete_message(&message_id)
                .map_err(ApiError::from_store)?;
            queue_pending(
                &state,
                &msg,
                "archive",
                json!({
                    "source_folder_id": &source_folder.id,
                    "source_folder_remote_id": &source_folder.remote_id,
                    "trash_or_soft_delete": true,
                }),
            )
            .map_err(ApiError::from_pebble)?;
        }
    }
    refresh_search_documents(&state, &[message_id.clone()]).map_err(ApiError::from_pebble)?;
    emit_folder_and_queue_changed(&state, &message_id);
    Ok(json!({ "status": "archived" }))
}

async fn restore_message(state: AppStateRef, message_id: String) -> Result<Value, ApiError> {
    let (msg, _account, source_folder) =
        find_message_context(&state, &message_id).map_err(ApiError::from_pebble)?;
    let inbox = find_folder_by_role(&state, &msg.account_id, FolderRole::Inbox)
        .map_err(ApiError::from_pebble)?
        .ok_or_else(|| ApiError::BadRequest("no inbox folder".to_string()))?;

    state
        .store
        .move_message_to_folder(&message_id, &inbox.id)
        .map_err(ApiError::from_store)?;
    queue_pending(
        &state,
        &msg,
        "restore",
        json!({
            "source_folder_id": &source_folder.id,
            "source_folder_remote_id": &source_folder.remote_id,
            "target_folder_id": &inbox.id,
            "target_folder_remote_id": &inbox.remote_id,
        }),
    )
    .map_err(ApiError::from_pebble)?;
    refresh_search_documents(&state, &[message_id.clone()]).map_err(ApiError::from_pebble)?;
    emit_folder_and_queue_changed(&state, &message_id);
    Ok(Value::Null)
}

async fn delete_message(state: AppStateRef, message_id: String) -> Result<Value, ApiError> {
    let (msg, _account, source_folder) =
        find_message_context(&state, &message_id).map_err(ApiError::from_pebble)?;
    let is_permanent = source_folder.role == Some(FolderRole::Trash)
        || find_folder_by_role(&state, &msg.account_id, FolderRole::Trash)
            .map_err(ApiError::from_pebble)?
            .is_none();

    state
        .store
        .soft_delete_message(&message_id)
        .map_err(ApiError::from_store)?;
    let op_type = if is_permanent {
        "delete_permanent"
    } else {
        "delete"
    };
    queue_pending(
        &state,
        &msg,
        op_type,
        json!({
            "source_folder_id": &source_folder.id,
            "source_folder_remote_id": &source_folder.remote_id,
            "permanent": is_permanent,
        }),
    )
    .map_err(ApiError::from_pebble)?;
    refresh_search_documents(&state, &[message_id.clone()]).map_err(ApiError::from_pebble)?;
    emit_folder_and_queue_changed(&state, &message_id);
    Ok(Value::Null)
}

async fn move_to_folder(
    state: AppStateRef,
    message_id: String,
    target_folder_id: String,
) -> Result<Value, ApiError> {
    let (msg, _account, source_folder) =
        find_message_context(&state, &message_id).map_err(ApiError::from_pebble)?;
    if source_folder.id == target_folder_id {
        return Ok(Value::Null);
    }
    let target_folder = state
        .store
        .list_folders(&msg.account_id)
        .map_err(ApiError::from_store)?
        .into_iter()
        .find(|f| f.id == target_folder_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!("target folder not found: {target_folder_id}"))
        })?;

    state
        .store
        .move_message_to_folder(&message_id, &target_folder_id)
        .map_err(ApiError::from_store)?;
    queue_pending(
        &state,
        &msg,
        "move_to_folder",
        json!({
            "source_folder_id": &source_folder.id,
            "source_folder_remote_id": &source_folder.remote_id,
            "target_folder_id": &target_folder.id,
            "target_folder_remote_id": &target_folder.remote_id,
        }),
    )
    .map_err(ApiError::from_pebble)?;
    refresh_search_documents(&state, &[message_id.clone()]).map_err(ApiError::from_pebble)?;
    emit_folder_and_queue_changed(&state, &message_id);
    Ok(Value::Null)
}

async fn empty_trash(state: AppStateRef, account_id: String) -> Result<Value, ApiError> {
    let trash = find_folder_by_role(&state, &account_id, FolderRole::Trash)
        .map_err(ApiError::from_pebble)?
        .ok_or_else(|| ApiError::BadRequest("no trash folder".to_string()))?;
    let messages = state
        .store
        .list_full_messages_by_folder(&trash.id, u32::MAX, 0)
        .map_err(ApiError::from_store)?;
    let ids: Vec<String> = messages.iter().map(|m| m.id.clone()).collect();
    let count = ids.len() as u32;
    if !ids.is_empty() {
        state
            .store
            .hard_delete_messages(&ids)
            .map_err(ApiError::from_store)?;
    }
    for msg in &messages {
        queue_pending(
            &state,
            msg,
            "delete_permanent",
            json!({ "source_folder_id": &trash.id, "source_folder_remote_id": &trash.remote_id }),
        )
        .map_err(ApiError::from_pebble)?;
    }
    Ok(json!({ "deleted": count }))
}


/// 命令分发入口（参数从 JSON 反序列化）。
pub async fn dispatch_command(
    state: AppStateRef,
    command: &str,
    args: Value,
) -> Result<Value, ApiError> {
    match command {
        "archive_message" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_id: String,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("archive_message"))?;
            archive_or_unarchive(state, a.message_id).await
        }
        "restore_message" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_id: String,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("restore_message"))?;
            restore_message(state, a.message_id).await
        }
        "delete_message" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_id: String,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("delete_message"))?;
            delete_message(state, a.message_id).await
        }
        "move_to_folder" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_id: String,
                target_folder_id: String,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("move_to_folder"))?;
            move_to_folder(state, a.message_id, a.target_folder_id).await
        }
        "empty_trash" => {
            #[derive(serde::Deserialize)]
            struct Args {
                account_id: String,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("empty_trash"))?;
            empty_trash(state, a.account_id).await
        }
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}

fn invalid_args(command: &'static str) -> impl FnOnce(serde_json::Error) -> ApiError + 'static {
    move |e| ApiError::BadRequest(format!("invalid {command} args: {e}"))
}
