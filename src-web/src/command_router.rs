use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde_json::{json, Value};

use crate::auth;
use crate::commands::{
    accounts, advanced_search, appearance, attachments, batch, cloud_sync, compose, contacts,
    diagnostics, drafts, folder_counts, folders, health, kanban, labels, messages, network, oauth,
    pending_mail_ops, rules, search, snooze, sync_cmd, threads, translate, trusted_senders,
    user_data,
};
use crate::error::ApiError;
use crate::state::AppStateRef;

/// Web 命令契约的唯一公开清单。
pub const SUPPORTED_COMMANDS: &[&str] = &[
    "login",
    "health_check",
    "get_profile_storage_namespace",
    "list_accounts",
    "add_account",
    "update_account",
    "delete_account",
    "get_account_proxy",
    "get_account_proxy_setting",
    "update_account_proxy",
    "update_account_proxy_setting",
    "complete_oauth_flow",
    "preview_oauth_identity",
    "apply_oauth_identity",
    "import_background_image",
    "delete_background_image",
    "export_backup_file",
    "preview_backup_file",
    "import_backup_file",
    "test_webdav_connection",
    "backup_to_webdav",
    "preview_webdav_backup",
    "restore_from_webdav",
    "save_auto_backup_config",
    "load_auto_backup_config",
    "delete_auto_backup_config",
    "get_global_proxy",
    "update_global_proxy",
    "list_attachments",
    "list_folders",
    "get_folder_unread_counts",
    "get_imap_sync_folders",
    "update_imap_sync_folders",
    "list_threads",
    "list_thread_messages",
    "list_messages",
    "list_starred_messages",
    "get_message",
    "get_messages_batch",
    "get_rendered_html",
    "get_message_with_html",
    "is_trusted_sender",
    "get_message_labels",
    "get_message_labels_batch",
    "add_message_label",
    "remove_message_label",
    "list_labels",
    "list_trusted_senders",
    "trust_sender",
    "remove_trusted_sender",
    "search_messages",
    "advanced_search",
    "send_email",
    "list_rules",
    "create_rule",
    "update_rule",
    "delete_rule",
    "list_contacts",
    "get_contact_by_email",
    "save_contact",
    "delete_contact",
    "set_contact_favorite",
    "search_contact_suggestions",
    "suppress_contact_suggestion",
    "import_contacts_vcard",
    "export_contacts_vcard",
    "search_contacts",
    "test_imap_connection",
    "test_pop3_connection",
    "test_account_connection",
    "read_app_log",
    "check_for_update",
    "archive_message",
    "restore_message",
    "delete_message",
    "move_to_folder",
    "empty_trash",
    "update_message_flags",
    "batch_archive",
    "batch_delete",
    "batch_mark_read",
    "batch_star",
    "trigger_sync",
    "start_sync",
    "stop_sync",
    "set_realtime_preference",
    "reindex_search",
    "get_pending_mail_ops_summary",
    "list_pending_mail_ops",
    "dismiss_failed_pending_mail_ops",
    "snooze_message",
    "unsnooze_message",
    "list_snoozed",
    "move_to_kanban",
    "list_kanban_cards",
    "remove_from_kanban",
    "list_kanban_context_notes",
    "set_kanban_context_note",
    "merge_kanban_context_notes",
    "get_translate_config",
    "save_translate_config",
    "test_translate_connection",
    "translate_text",
    "stage_compose_attachment",
    "cleanup_staged_compose_attachment",
    "get_attachment_path",
    "save_draft",
    "delete_draft",
    "list_email_templates",
    "save_email_template",
    "delete_email_template",
    "get_email_signature",
    "set_email_signature",
    "migrate_email_signature_if_absent",
    "get_oauth_account_proxy",
    "get_oauth_account_proxy_setting",
    "update_oauth_account_proxy",
    "update_oauth_account_proxy_setting",
];

fn is_supported_command(command: &str) -> bool {
    SUPPORTED_COMMANDS.contains(&command)
}

/// 命令注册表：命令名 → 处理函数。
///
/// 计划书 §17/§19 命令风格：POST /api/v1/command/{command}，
/// 命令名与桌面端 Tauri command 保持一致，减少前端分支代码。
/// 白名单机制：不允许的命令一律 404，不做动态分发。
/// login/health_check 为公开命令；其余命令需 Bearer token（auth::require_auth）。
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
        "health_check" => Ok(Json(health::health_check_command(state).await?)),
        _ => {
            auth::require_auth(&state, &headers)?;
            if !is_supported_command(&command) {
                return Err(ApiError::NotFound(format!("unknown command: {command}")));
            }
            dispatch(state, &command, args).await
        }
    }
}

/// 受保护命令分发。阶段四起按顺序注册：
/// list_accounts → add_account → list_folders → list_threads → ...
async fn dispatch(state: AppStateRef, command: &str, args: Value) -> Result<Json<Value>, ApiError> {
    match command {
        "get_profile_storage_namespace" => Ok(Json(Value::String("web".to_string()))),
        "list_accounts" => Ok(Json(accounts::list_accounts(state.clone(), args).await?)),
        "add_account" => Ok(Json(accounts::add_account(state.clone(), args).await?)),
        "update_account" => Ok(Json(accounts::update_account(state.clone(), args).await?)),
        "get_account_proxy" => Ok(Json(
            accounts::get_account_proxy(state.clone(), args).await?,
        )),
        "get_account_proxy_setting" => Ok(Json(
            accounts::get_account_proxy_setting(state.clone(), args).await?,
        )),
        "update_account_proxy" => Ok(Json(
            accounts::update_account_proxy(state.clone(), args).await?,
        )),
        "update_account_proxy_setting" => Ok(Json(
            accounts::update_account_proxy_setting(state.clone(), args).await?,
        )),
        "preview_oauth_identity" => Ok(Json(oauth::preview_oauth_identity(state, args).await?)),
        "apply_oauth_identity" => Ok(Json(oauth::apply_oauth_identity(state, args).await?)),
        // Web OAuth is a two-step browser flow: this command creates the
        // server-side PKCE transaction and returns the authorization URL;
        // the callback completes it and posts the account to the opener.
        "complete_oauth_flow" => Ok(Json(crate::oauth::start_oauth_flow(state, args).await?)),
        "delete_account" => Ok(Json(accounts::delete_account(state, args).await?)),
        "import_background_image" => Ok(Json(
            appearance::import_background_image(state, args).await?,
        )),
        "delete_background_image" => Ok(Json(
            appearance::delete_background_image(state, args).await?,
        )),
        "get_oauth_account_proxy" => Ok(Json(oauth::get_oauth_account_proxy(state, args).await?)),
        "get_oauth_account_proxy_setting" => Ok(Json(
            oauth::get_oauth_account_proxy_setting(state, args).await?,
        )),
        "update_oauth_account_proxy" => {
            Ok(Json(oauth::update_oauth_account_proxy(state, args).await?))
        }
        "update_oauth_account_proxy_setting" => Ok(Json(
            oauth::update_oauth_account_proxy_setting(state, args).await?,
        )),
        "export_backup_file" => Ok(Json(cloud_sync::export_backup_file(state, args).await?)),
        "preview_backup_file" => Ok(Json(cloud_sync::preview_backup_file(state, args).await?)),
        "import_backup_file" => Ok(Json(cloud_sync::import_backup_file(state, args).await?)),
        // WebDAV 云端同步（Web 后端真实实现，能力对齐桌面端）
        "test_webdav_connection" => {
            Ok(Json(cloud_sync::test_webdav_connection(state, args).await?))
        }
        "backup_to_webdav" => Ok(Json(cloud_sync::backup_to_webdav(state, args).await?)),
        "preview_webdav_backup" => Ok(Json(cloud_sync::preview_webdav_backup(state, args).await?)),
        "restore_from_webdav" => Ok(Json(cloud_sync::restore_from_webdav(state, args).await?)),
        "save_auto_backup_config" => Ok(Json(cloud_sync::save_auto_backup_config(state, args)?)),
        "load_auto_backup_config" => Ok(Json(cloud_sync::load_auto_backup_config(state, args)?)),
        "delete_auto_backup_config" => {
            Ok(Json(cloud_sync::delete_auto_backup_config(state, args)?))
        }
        // 全局网络代理（Web 端真实实现，作用于后端 IMAP/SMTP/连接测试装配）
        "get_global_proxy" => Ok(Json(network::get_global_proxy(state, args).await?)),
        "update_global_proxy" => Ok(Json(network::update_global_proxy(state, args).await?)),
        "list_attachments" => Ok(Json(attachments::list_attachments(state, args).await?)),
        "stage_compose_attachment" => Ok(Json(
            attachments::stage_compose_attachment(state, args).await?,
        )),
        "cleanup_staged_compose_attachment" => Ok(Json(
            attachments::cleanup_staged_compose_attachment(state, args).await?,
        )),
        "get_attachment_path" => Ok(Json(attachments::get_attachment_path(state, args).await?)),
        "save_draft" | "delete_draft" => {
            Ok(Json(drafts::dispatch_command(state, command, args).await?))
        }
        "list_email_templates" => Ok(Json(user_data::list_email_templates(state, args).await?)),
        "save_email_template" => Ok(Json(user_data::save_email_template(state, args).await?)),
        "delete_email_template" => Ok(Json(user_data::delete_email_template(state, args).await?)),
        "get_email_signature" => Ok(Json(user_data::get_email_signature(state, args).await?)),
        "set_email_signature" => Ok(Json(user_data::set_email_signature(state, args).await?)),
        "migrate_email_signature_if_absent" => Ok(Json(
            user_data::migrate_email_signature_if_absent(state, args).await?,
        )),
        "list_folders" => Ok(Json(folders::list_folders(state, args).await?)),
        "get_folder_unread_counts" => Ok(Json(
            folder_counts::get_folder_unread_counts(state, args).await?,
        )),
        "get_imap_sync_folders" => Ok(Json(folders::get_imap_sync_folders(state, args).await?)),
        "update_imap_sync_folders" => {
            Ok(Json(folders::update_imap_sync_folders(state, args).await?))
        }
        "list_threads" => Ok(Json(threads::list_threads(state, args).await?)),
        "list_thread_messages" => Ok(Json(threads::list_thread_messages(state, args).await?)),
        "list_messages" => Ok(Json(messages::list_messages(state, args).await?)),
        "list_starred_messages" => Ok(Json(messages::list_starred_messages(state, args).await?)),
        "get_message" => Ok(Json(messages::get_message(state, args).await?)),
        "get_messages_batch" => Ok(Json(messages::get_messages_batch(state, args).await?)),
        "get_rendered_html" => Ok(Json(messages::get_rendered_html(state, args).await?)),
        "get_message_with_html" => Ok(Json(messages::get_message_with_html(state, args).await?)),
        "is_trusted_sender" => Ok(Json(messages::is_trusted_sender(state, args).await?)),
        "get_message_labels" => Ok(Json(labels::get_message_labels(state, args).await?)),
        "get_message_labels_batch" => {
            Ok(Json(labels::get_message_labels_batch(state, args).await?))
        }
        "add_message_label" => Ok(Json(labels::add_message_label(state, args).await?)),
        "remove_message_label" => Ok(Json(labels::remove_message_label(state, args).await?)),
        "list_labels" => Ok(Json(labels::list_labels(state, args).await?)),
        "list_trusted_senders" => Ok(Json(
            trusted_senders::list_trusted_senders(state, args).await?,
        )),
        "trust_sender" => Ok(Json(trusted_senders::trust_sender(state, args).await?)),
        "remove_trusted_sender" => Ok(Json(
            trusted_senders::remove_trusted_sender(state, args).await?,
        )),
        "snooze_message" => Ok(Json(snooze::snooze_message(state, args).await?)),
        "unsnooze_message" => Ok(Json(snooze::unsnooze_message(state, args).await?)),
        "list_snoozed" => Ok(Json(snooze::list_snoozed(state, args).await?)),
        "move_to_kanban" => Ok(Json(kanban::move_to_kanban(state, args).await?)),
        "list_kanban_cards" => Ok(Json(kanban::list_kanban_cards(state, args).await?)),
        "remove_from_kanban" => Ok(Json(kanban::remove_from_kanban(state, args).await?)),
        "list_kanban_context_notes" => {
            Ok(Json(kanban::list_kanban_context_notes(state, args).await?))
        }
        "set_kanban_context_note" => Ok(Json(kanban::set_kanban_context_note(state, args).await?)),
        "merge_kanban_context_notes" => {
            Ok(Json(kanban::merge_kanban_context_notes(state, args).await?))
        }
        "translate_text" => Ok(Json(translate::translate_text(state, args).await?)),
        "get_translate_config" => Ok(Json(translate::get_translate_config(state, args).await?)),
        "save_translate_config" => Ok(Json(translate::save_translate_config(state, args).await?)),
        "test_translate_connection" => Ok(Json(
            translate::test_translate_connection(state, args).await?,
        )),
        "search_messages" => Ok(Json(search::search_messages(state, args).await?)),
        "advanced_search" => Ok(Json(advanced_search::advanced_search(state, args).await?)),
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
        "search_contact_suggestions" => Ok(Json(
            contacts::search_contact_suggestions(state, args).await?,
        )),
        "suppress_contact_suggestion" => Ok(Json(
            contacts::suppress_contact_suggestion(state, args).await?,
        )),
        "import_contacts_vcard" => Ok(Json(contacts::import_contacts_vcard(state, args).await?)),
        "export_contacts_vcard" => Ok(Json(contacts::export_contacts_vcard(state, args).await?)),
        "search_contacts" => Ok(Json(contacts::search_contacts(state, args).await?)),
        // 连接测试（A 方案：Web 端真实可用，UI 与桌面同步）
        "test_imap_connection" => Ok(Json(accounts::test_imap_connection(state, args).await?)),
        "test_pop3_connection" => Ok(Json(accounts::test_pop3_connection(state, args).await?)),
        "test_account_connection" => {
            Ok(Json(accounts::test_account_connection(state, args).await?))
        }
        // 诊断日志（A 方案：日志文件化，Web 端可读服务端日志）
        "read_app_log" => Ok(Json(diagnostics::read_app_log(state, args).await?)),
        // 更新检查（Web 端指向上游 fork Release，真实可用）
        "check_for_update" => Ok(Json(health::check_for_update(state, args).await?)),
        // 消息生命周期（本地提交 + 远端排队）
        "archive_message" | "restore_message" | "delete_message" | "move_to_folder"
        | "empty_trash" => Ok(Json(
            messages::lifecycle::dispatch_command(state, command, args).await?,
        )),
        "update_message_flags" => Ok(Json(
            messages::flags::dispatch_command(state, command, args).await?,
        )),
        "batch_archive" | "batch_delete" | "batch_mark_read" | "batch_star" => {
            Ok(Json(batch::dispatch_command(state, command, args).await?))
        }
        // 同步（4.6）与待处理队列
        "trigger_sync" => Ok(Json(sync_cmd::trigger_sync(state, args).await?)),
        "start_sync" => Ok(Json(sync_cmd::start_sync(state, args).await?)),
        "stop_sync" => Ok(Json(sync_cmd::stop_sync(state, args).await?)),
        "set_realtime_preference" => {
            Ok(Json(sync_cmd::set_realtime_preference(state, args).await?))
        }
        "reindex_search" => Ok(Json(sync_cmd::reindex_search(state, args).await?)),
        "get_pending_mail_ops_summary" => Ok(Json(
            pending_mail_ops::get_pending_mail_ops_summary(state, args).await?,
        )),
        "list_pending_mail_ops" => Ok(Json(
            pending_mail_ops::list_pending_mail_ops(state, args).await?,
        )),
        "dismiss_failed_pending_mail_ops" => Ok(Json(
            pending_mail_ops::dismiss_failed_pending_mail_ops(state, args).await?,
        )),
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}

/// 健康检查（无需登录）。
pub async fn health(State(_state): State<AppStateRef>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "pebble-web",
        "version": env!("PEBBLE_APP_VERSION"),
    }))
}

/// 未匹配路由的统一 404。
pub async fn not_found() -> ApiError {
    ApiError::NotFound("not found".to_string())
}
