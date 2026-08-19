use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use pebble_core::PebbleError;
use serde_json::{json, Value};

use crate::auth as auth_mod;
use crate::error::ApiError;
use crate::state::AppStateRef;

mod accounts;
pub mod attachments;
mod auth;
mod compose;
mod contacts;
mod folders;
mod lifecycle;
mod messages;
mod rules;
mod search;
mod sync_cmd;
mod threads;

/// 命令注册表：命令名 → 处理函数。
///
/// 计划书 §17/§19 命令风格：POST /api/v1/command/{command}，
/// 命令名与桌面端 Tauri command 保持一致，减少前端分支代码。
/// 白名单机制：不允许的命令一律 404，不做动态分发。
/// login 为公开命令；其余命令需 Bearer token（auth::require_auth）。
pub async fn handle_command(
    State(state): State<AppStateRef>,
    Path(command): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    // 手动解析 JSON body：统一走结构化错误（替代 axum Json extractor 的默认纯文本拒绝）
    let args: Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::BadRequest(format!("invalid JSON body: {e}")))?;
    match command.as_str() {
        "login" => Ok(Json(auth::login(state, args)?)),
        _ => {
            auth_mod::require_auth(&state, &headers)?;
            dispatch(state, &command, args).await
        }
    }
}

/// store 阻塞调用包装：SQLite/磁盘 I/O 不得阻塞 async 主线程（与桌面端 spawn_blocking 一致）。
pub(crate) async fn run_blocking<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PebbleError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(format!("task join error: {e}")))?
        .map_err(ApiError::from_pebble)
}

/// 受保护命令分发。阶段四起按顺序注册：
/// list_accounts → add_account → list_folders → list_threads → ...
async fn dispatch(
    state: AppStateRef,
    command: &str,
    args: Value,
) -> Result<Json<Value>, ApiError> {
    match command {
        "list_accounts" => Ok(Json(accounts::list_accounts(state.clone(), args).await?)),
        "add_account" => Ok(Json(accounts::add_account(state.clone(), args).await?)),
        "update_account" => Ok(Json(accounts::update_account(state.clone(), args).await?)),
        "delete_account" => Ok(Json(accounts::delete_account(state, args).await?)),
        "list_attachments" => Ok(Json(attachments::list_attachments(state, args).await?)),
        "list_folders" => Ok(Json(folders::list_folders(state, args).await?)),
        "list_threads" => Ok(Json(threads::list_threads(state, args).await?)),
        "list_thread_messages" => Ok(Json(threads::list_thread_messages(state, args).await?)),
        "list_messages" => Ok(Json(messages::list_messages(state, args).await?)),
        "list_starred_messages" => Ok(Json(messages::list_starred_messages(state, args).await?)),
        "get_message" => Ok(Json(messages::get_message(state, args).await?)),
        "get_messages_batch" => Ok(Json(messages::get_messages_batch(state, args).await?)),
        "get_rendered_html" => Ok(Json(messages::get_rendered_html(state, args).await?)),
        "get_message_with_html" => Ok(Json(messages::get_message_with_html(state, args).await?)),
        "is_trusted_sender" => Ok(Json(messages::is_trusted_sender(state, args).await?)),
        "search_messages" => Ok(Json(search::search_messages(state, args).await?)),
        "advanced_search" => Ok(Json(search::advanced_search(state, args).await?)),
        "send_email" => Ok(Json(compose::send_email(state, args).await?)),
        "list_rules" => Ok(Json(rules::list_rules(state, args).await?)),
        "create_rule" => Ok(Json(rules::create_rule(state, args).await?)),
        "update_rule" => Ok(Json(rules::update_rule(state, args).await?)),
        "delete_rule" => Ok(Json(rules::delete_rule(state, args).await?)),
        "list_contacts" => Ok(Json(contacts::list_contacts(state, args).await?)),
        "get_contact_by_email" => Ok(Json(contacts::get_contact_by_email(state, args).await?)),
        "save_contact" => Ok(Json(contacts::save_contact(state, args).await?)),
        "delete_contact" => Ok(Json(contacts::delete_contact(state, args).await?)),
        "set_contact_favorite" => Ok(Json(contacts::set_contact_favorite(state, args).await?)),
        "search_contact_suggestions" => {
            Ok(Json(contacts::search_contact_suggestions(state, args).await?))
        }
        "suppress_contact_suggestion" => {
            Ok(Json(contacts::suppress_contact_suggestion(state, args).await?))
        }
        "import_contacts_vcard" => Ok(Json(contacts::import_contacts_vcard(state, args).await?)),
        "export_contacts_vcard" => Ok(Json(contacts::export_contacts_vcard(state, args).await?)),
        "search_contacts" => Ok(Json(contacts::search_contacts(state, args).await?)),
        // 消息生命周期（本地提交 + 远端排队）
        "archive_message" | "restore_message" | "delete_message" | "move_to_folder"
        | "empty_trash" | "update_message_flags" | "batch_archive" | "batch_delete"
        | "batch_mark_read" | "batch_star" => {
            Ok(Json(lifecycle::dispatch_command(state, command, args).await?))
        }
        // 同步（4.6）与待处理队列
        "trigger_sync" => Ok(Json(sync_cmd::trigger_sync(state, args).await?)),
        "start_sync" => Ok(Json(sync_cmd::start_sync(state, args).await?)),
        "stop_sync" => Ok(Json(sync_cmd::stop_sync(state, args).await?)),
        "get_pending_mail_ops_summary" => {
            Ok(Json(sync_cmd::get_pending_mail_ops_summary(state, args).await?))
        }
        "list_pending_mail_ops" => Ok(Json(sync_cmd::list_pending_mail_ops(state, args).await?)),
        "dismiss_failed_pending_mail_ops" => {
            Ok(Json(sync_cmd::dismiss_failed_pending_mail_ops(state, args).await?))
        }
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}

/// 健康检查（无需登录）。
pub async fn health(State(_state): State<AppStateRef>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "pebble-web",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// 未匹配路由的统一 404。
pub async fn not_found() -> ApiError {
    ApiError::NotFound("not found".to_string())
}