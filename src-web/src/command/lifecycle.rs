use pebble_core::{Account, Folder, FolderRole, Message, PebbleError, ProviderType};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::AppStateRef;

/// 事件名与桌面端 Tauri event 对齐（计划书 §22）。
mod event {
    pub const FOLDER_CHANGED: &str = "mail:folder-changed";
    pub const PENDING_OPS_CHANGED: &str = "mail:pending-ops-changed";
}

/// 操作后广播（同通道由 ws.rs 推送给所有订阅连接）。
fn emit(state: &AppStateRef, event_type: &str, payload: Value) {
    let _ = state
        .ws_broadcast
        .send(json!({ "type": event_type, "payload": payload }).to_string());
}

/// 归档/移动/删除后：文件夹变化 + 队列变化通知。
fn emit_folder_and_queue_changed(state: &AppStateRef, message_id: &str) {
    emit(
        state,
        event::FOLDER_CHANGED,
        json!({ "message_id": message_id }),
    );
    emit(state, event::PENDING_OPS_CHANGED, json!({}));
}

fn should_queue_remote_mutation(provider: &ProviderType) -> bool {
    !matches!(provider, ProviderType::Pop3)
}

/// 消息生命周期命令（本地即时提交 + 远端写回排队）。
///
/// 策略（2026-08-19）：Web 端不即时连接远程服务器，本地 DB 变更立即生效，
/// 统一插入 pending_mail_ops 排队；后台同步循环（阶段 4.6）执行远端写回。
/// payload 结构对齐桌面端 queue_pending_remote_op（provider_account_id/remote_id/op/payload），
/// 保证 4.6 的写回循环可按同一格式处理。

fn find_message_context(
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

fn find_folder_by_role(
    state: &AppStateRef,
    account_id: &str,
    role: FolderRole,
) -> Result<Option<Folder>, PebbleError> {
    let folders = state.store.list_folders(account_id)?;
    Ok(folders.into_iter().find(|f| f.role.as_ref() == Some(&role)))
}

/// 排队远端写回操作（外层 payload 与桌面端 queue_pending_remote_op_for_local_commit 一致）。
pub(crate) fn queue_pending_for_store(
    store: &pebble_store::Store,
    msg: &Message,
    op_type: &str,
    inner_payload: Value,
) -> Result<(), PebbleError> {
    // POP3 has no stable remote mutation model in the shared provider (UIDLs
    // are read-only). Keep archive/delete/flag changes local and do not leave
    // permanently failing operations in the retry queue.
    if store
        .get_account(&msg.account_id)?
        .is_some_and(|account| !should_queue_remote_mutation(&account.provider))
    {
        tracing::debug!(account_id = %msg.account_id, op_type, "skipping remote POP3 mutation");
        return Ok(());
    }
    let payload = json!({
        "provider_account_id": msg.account_id,
        "remote_id": msg.remote_id,
        "op": op_type,
        "payload": inner_payload,
    });
    let op_id =
        store.insert_pending_mail_op(&msg.account_id, &msg.id, op_type, &payload.to_string())?;
    // 与桌面端离线分支一致：连接失败时标记 failed 待后台重试
    store.mark_pending_mail_op_failed(&op_id, "queued for background sync")?;
    Ok(())
}

fn queue_pending(
    state: &AppStateRef,
    msg: &Message,
    op_type: &str,
    inner_payload: Value,
) -> Result<(), PebbleError> {
    queue_pending_for_store(&state.store, msg, op_type, inner_payload)
}

/// 批量重建搜索索引文档（与桌面端 refresh_search_documents 等价）。
pub(crate) fn refresh_search_documents(
    state: &AppStateRef,
    ids: &[String],
) -> Result<(), PebbleError> {
    if ids.is_empty() {
        return Ok(());
    }
    state.store.add_search_pending(ids, "index")?;
    for id in ids {
        match state.store.get_message(id)? {
            Some(message) if !message.is_deleted => {
                let folder_ids = state.store.get_message_folder_ids(id)?;
                if folder_ids.is_empty() {
                    state.search.remove_message(id)?;
                } else {
                    state.search.index_message(&message, &folder_ids)?;
                }
            }
            _ => {
                state.search.remove_message(id)?;
            }
        }
    }
    state.search.commit()?;
    state.store.clear_search_pending(ids)?;
    Ok(())
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

async fn update_message_flags(
    state: AppStateRef,
    message_id: String,
    is_read: Option<bool>,
    is_starred: Option<bool>,
) -> Result<Value, ApiError> {
    let exists = state
        .store
        .get_message(&message_id)
        .map_err(ApiError::from_store)?
        .is_some();
    if !exists {
        return Err(ApiError::NotFound(format!(
            "message not found: {message_id}"
        )));
    }
    state
        .store
        .update_message_flags(&message_id, is_read, is_starred)
        .map_err(ApiError::from_store)?;
    if let Some(msg) = state
        .store
        .get_message(&message_id)
        .map_err(ApiError::from_store)?
    {
        queue_pending(
            &state,
            &msg,
            "update_flags",
            json!({ "is_read": is_read, "is_starred": is_starred }),
        )
        .map_err(ApiError::from_pebble)?;
    }
    refresh_search_documents(&state, &[message_id.clone()]).map_err(ApiError::from_pebble)?;
    Ok(Value::Null)
}

async fn batch_archive(state: AppStateRef, message_ids: Vec<String>) -> Result<Value, ApiError> {
    let mut archived = 0u32;
    for id in &message_ids {
        if let Ok((msg, _account, source_folder)) = find_message_context(&state, id) {
            match find_folder_by_role(&state, &msg.account_id, FolderRole::Archive)
                .map_err(ApiError::from_pebble)?
            {
                Some(af) => {
                    state
                        .store
                        .move_message_to_folder(id, &af.id)
                        .map_err(ApiError::from_store)?;
                    queue_pending(
                        &state,
                        &msg,
                        "archive",
                        json!({
                            "source_folder_id": &source_folder.id,
                            "source_folder_remote_id": &source_folder.remote_id,
                            "target_folder_id": &af.id,
                            "target_folder_remote_id": &af.remote_id,
                        }),
                    )
                    .map_err(ApiError::from_pebble)?;
                }
                None => {
                    state
                        .store
                        .soft_delete_message(id)
                        .map_err(ApiError::from_store)?;
                    queue_pending(
                        &state,
                        &msg,
                        "archive",
                        json!({ "source_folder_id": &source_folder.id, "trash_or_soft_delete": true }),
                    )
                    .map_err(ApiError::from_pebble)?;
                }
            }
            archived += 1;
        }
    }
    refresh_search_documents(&state, &message_ids).map_err(ApiError::from_pebble)?;
    for id in &message_ids {
        emit_folder_and_queue_changed(&state, id);
    }
    Ok(json!({ "archived": archived }))
}

async fn batch_delete(state: AppStateRef, message_ids: Vec<String>) -> Result<Value, ApiError> {
    state
        .store
        .bulk_soft_delete(&message_ids)
        .map_err(ApiError::from_store)?;
    for id in &message_ids {
        if let Ok((msg, _account, source_folder)) = find_message_context(&state, id) {
            queue_pending(
                &state,
                &msg,
                "delete",
                json!({
                    "source_folder_id": &source_folder.id,
                    "source_folder_remote_id": &source_folder.remote_id,
                }),
            )
            .map_err(ApiError::from_pebble)?;
        }
    }
    refresh_search_documents(&state, &message_ids).map_err(ApiError::from_pebble)?;
    Ok(json!({ "deleted": message_ids.len() }))
}

async fn batch_mark_read(
    state: AppStateRef,
    message_ids: Vec<String>,
    is_read: bool,
) -> Result<Value, ApiError> {
    let existing_ids: Vec<String> = message_ids
        .iter()
        .filter_map(|id| match state.store.get_message(id) {
            Ok(Some(_)) => Some(Ok(id.clone())),
            Ok(None) => None,
            Err(error) => Some(Err(ApiError::from_store(error))),
        })
        .collect::<Result<_, _>>()?;
    if existing_ids.is_empty() {
        return Ok(json!(0u32));
    }
    let changes: Vec<(String, Option<bool>, Option<bool>)> = existing_ids
        .iter()
        .map(|id| (id.clone(), Some(is_read), None))
        .collect();
    state
        .store
        .bulk_update_flags(&changes)
        .map_err(ApiError::from_store)?;
    for id in &existing_ids {
        if let Ok(msg) = state.store.get_message(id) {
            if let Some(msg) = msg {
                queue_pending(&state, &msg, "update_flags", json!({ "is_read": is_read }))
                    .map_err(ApiError::from_pebble)?;
            }
        }
    }
    refresh_search_documents(&state, &existing_ids).map_err(ApiError::from_pebble)?;
    Ok(json!(existing_ids.len() as u32))
}

async fn batch_star(
    state: AppStateRef,
    message_ids: Vec<String>,
    starred: bool,
) -> Result<Value, ApiError> {
    let existing_ids: Vec<String> = message_ids
        .iter()
        .filter_map(|id| match state.store.get_message(id) {
            Ok(Some(_)) => Some(Ok(id.clone())),
            Ok(None) => None,
            Err(error) => Some(Err(ApiError::from_store(error))),
        })
        .collect::<Result<_, _>>()?;
    if existing_ids.is_empty() {
        return Ok(json!(0u32));
    }
    let changes: Vec<(String, Option<bool>, Option<bool>)> = existing_ids
        .iter()
        .map(|id| (id.clone(), None, Some(starred)))
        .collect();
    state
        .store
        .bulk_update_flags(&changes)
        .map_err(ApiError::from_store)?;
    for id in &existing_ids {
        if let Ok(Some(msg)) = state.store.get_message(id) {
            queue_pending(&state, &msg, "update_flags", json!({"starred": starred}))
                .map_err(ApiError::from_pebble)?;
        }
    }
    refresh_search_documents(&state, &existing_ids).map_err(ApiError::from_pebble)?;
    Ok(json!(existing_ids.len() as u32))
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
        "update_message_flags" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_id: String,
                #[serde(default)]
                is_read: Option<bool>,
                #[serde(default)]
                is_starred: Option<bool>,
            }
            let a: Args =
                serde_json::from_value(args).map_err(invalid_args("update_message_flags"))?;
            update_message_flags(state, a.message_id, a.is_read, a.is_starred).await
        }
        "batch_archive" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("batch_archive"))?;
            batch_archive(state, a.message_ids).await
        }
        "batch_delete" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("batch_delete"))?;
            batch_delete(state, a.message_ids).await
        }
        "batch_mark_read" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
                is_read: bool,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("batch_mark_read"))?;
            batch_mark_read(state, a.message_ids, a.is_read).await
        }
        "batch_star" => {
            // 与上游 Tauri 命令签名一致：batch_star(message_ids, starred)
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
                starred: bool,
            }
            let a: Args = serde_json::from_value(args).map_err(invalid_args("batch_star"))?;
            batch_star(state, a.message_ids, a.starred).await
        }
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}

fn invalid_args(command: &'static str) -> impl FnOnce(serde_json::Error) -> ApiError + 'static {
    move |e| ApiError::BadRequest(format!("invalid {command} args: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{
        archive_or_unarchive, delete_message, restore_message, should_queue_remote_mutation,
    };
    use crate::config::Config;
    use crate::state::{AppState, AppStateRef};
    use pebble_core::{
        now_timestamp, Account, Folder, FolderRole, FolderType, Message, ProviderType,
    };
    use std::path::PathBuf;

    fn test_state() -> AppStateRef {
        let data_dir = std::env::temp_dir().join(format!("pw-lifecycle-{}", uuid::Uuid::new_v4()));
        AppState::init(Config {
            port: 0,
            data_dir,
            password_hash: "unused-in-unit-test".to_string(),
            jwt_secret: "this-is-a-real-secret-with-32-plus-chars".to_string(),
            sync_interval_secs: 3600,
            static_dir: PathBuf::from("./dist"),
        })
        .unwrap()
    }

    fn seed_message(state: &AppStateRef) -> (Message, Folder, Folder, Folder) {
        let now = now_timestamp();
        let account = Account {
            id: "lifecycle-account".to_string(),
            email: "owner@example.com".to_string(),
            display_name: "Owner".to_string(),
            color: None,
            provider: ProviderType::Imap,
            created_at: now,
            updated_at: now,
        };
        state.store.insert_account(&account).unwrap();

        let folder = |id: &str, name: &str, remote_id: &str, role: FolderRole, sort_order| Folder {
            id: id.to_string(),
            account_id: account.id.clone(),
            remote_id: remote_id.to_string(),
            name: name.to_string(),
            folder_type: FolderType::Folder,
            role: Some(role),
            parent_id: None,
            color: None,
            is_system: true,
            sort_order,
        };
        let inbox = folder("lifecycle-inbox", "Inbox", "INBOX", FolderRole::Inbox, 0);
        let archive = folder(
            "lifecycle-archive",
            "Archive",
            "Archive",
            FolderRole::Archive,
            1,
        );
        let trash = folder("lifecycle-trash", "Trash", "Trash", FolderRole::Trash, 2);
        for item in [&inbox, &archive, &trash] {
            state.store.insert_folder(item).unwrap();
        }

        let message = Message {
            id: "lifecycle-message".to_string(),
            account_id: account.id,
            remote_id: "remote-lifecycle-message".to_string(),
            message_id_header: Some("<lifecycle@example.com>".to_string()),
            in_reply_to: None,
            references_header: None,
            thread_id: None,
            subject: "Lifecycle".to_string(),
            snippet: "Lifecycle".to_string(),
            from_address: "sender@example.com".to_string(),
            from_name: "Sender".to_string(),
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
        state
            .store
            .insert_message(&message, std::slice::from_ref(&inbox.id))
            .unwrap();
        (message, inbox, archive, trash)
    }

    #[test]
    fn pop3_mutations_are_local_only() {
        assert!(!should_queue_remote_mutation(&ProviderType::Pop3));
        assert!(should_queue_remote_mutation(&ProviderType::Imap));
        assert!(should_queue_remote_mutation(&ProviderType::Gmail));
        assert!(should_queue_remote_mutation(&ProviderType::Outlook));
    }

    #[tokio::test]
    async fn archive_delete_and_restore_update_local_state() {
        let state = test_state();
        let (message, inbox, archive, _trash) = seed_message(&state);

        let archived = archive_or_unarchive(state.clone(), message.id.clone())
            .await
            .unwrap();
        assert_eq!(archived["status"], "archived");
        assert_eq!(
            state.store.get_message_folder_ids(&message.id).unwrap(),
            vec![archive.id.clone()]
        );
        assert_eq!(state.search.search("Lifecycle", 10).unwrap().len(), 1);

        let unarchived = archive_or_unarchive(state.clone(), message.id.clone())
            .await
            .unwrap();
        assert_eq!(unarchived["status"], "unarchived");
        assert_eq!(
            state.store.get_message_folder_ids(&message.id).unwrap(),
            vec![inbox.id.clone()]
        );

        delete_message(state.clone(), message.id.clone())
            .await
            .unwrap();
        assert!(
            state
                .store
                .get_message(&message.id)
                .unwrap()
                .unwrap()
                .is_deleted
        );
        assert!(state.search.search("Lifecycle", 10).unwrap().is_empty());

        restore_message(state.clone(), message.id.clone())
            .await
            .unwrap();
        let restored = state.store.get_message(&message.id).unwrap().unwrap();
        assert!(!restored.is_deleted);
        assert_eq!(
            state.store.get_message_folder_ids(&message.id).unwrap(),
            vec![inbox.id]
        );
        assert_eq!(state.search.search("Lifecycle", 10).unwrap().len(), 1);
    }
}
