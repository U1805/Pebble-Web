use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::{AppState, AppStateRef};

use pebble_core::traits::{FolderProvider, LabelProvider};
use pebble_core::{FolderRole, Message, PebbleError, ProviderType};
use pebble_store::pending_ops::PendingMailOp;
use pebble_store::Store;
use tracing::{debug, warn};

use super::compose::{self, LocalOutgoingState};
use super::messages::{
    classify_remote_delete_result, connect_gmail, connect_imap, connect_outlook,
    find_folder_by_role, find_message_folder, refresh_search_document, remove_search_documents,
};

const WORKER_INTERVAL_SECS: u64 = 30;
const WORKER_BATCH_LIMIT: i64 = 20;

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
    store.insert_pending_mail_op(
        &message.account_id,
        &message.id,
        op_type,
        &payload.to_string(),
    )?;
    Ok(())
}

/// 待处理操作统计（失败/重试队列）。
pub async fn get_pending_mail_ops_summary(
    state: AppStateRef,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(default)]
        account_id: Option<String>,
    }
    let args: Args = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid get_pending_mail_ops_summary args: {e}"))
    })?;
    let s = state
        .store
        .pending_mail_ops_summary(args.account_id.as_deref())
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
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(default)]
        account_id: Option<String>,
        #[serde(default)]
        limit: Option<i64>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_pending_mail_ops args: {e}")))?;
    let limit = args.limit.unwrap_or(100).clamp(1, 500);
    let ops = state
        .store
        .list_active_pending_mail_ops(args.account_id.as_deref(), limit)
        .map_err(ApiError::from_store)?;
    let items: Vec<Value> = ops
        .iter()
        .map(|op| {
            json!({
                "id": op.id,
                "account_id": op.account_id,
                "message_id": op.message_id,
                "op_type": op.op_type,
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
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(default)]
        account_id: Option<String>,
    }
    let args: Args = serde_json::from_value(args).map_err(|e| {
        ApiError::BadRequest(format!("invalid dismiss_failed_pending_mail_ops args: {e}"))
    })?;
    let dismissed = state
        .store
        .dismiss_failed_pending_mail_ops(args.account_id.as_deref())
        .map_err(ApiError::from_store)?;
    if dismissed > 0 {
        emit_pending_ops_changed(&state);
    }
    Ok(json!(dismissed))
}

pub async fn run_pending_mail_ops_worker(state: AppStateRef) {
    let mut interval =
        tokio::time::interval(tokio::time::Duration::from_secs(WORKER_INTERVAL_SECS));
    if let Err(error) = state.store.reset_in_progress_pending_mail_ops() {
        warn!("Failed to reset interrupted pending mail ops: {error}");
    }

    loop {
        interval.tick().await;
        if let Err(error) = process_pending_mail_ops(&state).await {
            warn!("Pending mail op worker pass failed: {error}");
        }
    }
}

pub async fn process_pending_mail_ops(
    state: &crate::state::AppState,
) -> std::result::Result<usize, PebbleError> {
    let ops = state
        .store
        .list_retryable_pending_mail_ops(WORKER_BATCH_LIMIT)?;
    let mut changed = false;
    let mut completed = 0usize;

    for op in ops {
        state.store.mark_pending_mail_op_in_progress(&op.id)?;
        changed = true;

        match replay_pending_mail_op(state, &op).await {
            Ok(()) => {
                state.store.mark_pending_mail_op_done(&op.id)?;
                completed += 1;
            }
            Err(ReplayPendingMailOpError::Retryable(ref e)) if is_permanent_error(e) => {
                state.store.mark_pending_mail_op_done(&op.id)?;
                warn!(
                    "Pending mail op {} permanently failed (non-retryable): {e}",
                    op.id
                );
                completed += 1;
            }
            Err(ReplayPendingMailOpError::RemoteSendOutcomeUnknown(e)) => {
                let error = compose::send_outcome_unknown_message(&e);
                // If this write also fails, leave the row in_progress. Startup
                // recovery treats interrupted sends as outcome-unknown instead
                // of ever putting them back on the automatic retry queue.
                state
                    .store
                    .mark_pending_mail_op_outcome_unknown(&op.id, &error)?;
                warn!("Pending mail op {} has an unknown send outcome: {e}", op.id);
            }
            Err(ReplayPendingMailOpError::SenderIdentityBlocked(e)) => {
                state.store.mark_pending_mail_op_stopped(
                    &op.id,
                    &format!("Sender identity requires attention; no message was sent. {e}"),
                )?;
            }
            Err(ReplayPendingMailOpError::Retryable(e)) => {
                state
                    .store
                    .mark_pending_mail_op_failed(&op.id, &e.to_string())?;
                warn!("Pending mail op {} retry failed: {e}", op.id);
            }
        }
    }

    if changed {
        emit_pending_ops_changed(state);
    }
    Ok(completed)
}

#[derive(Debug)]
enum ReplayPendingMailOpError {
    SenderIdentityBlocked(PebbleError),
    Retryable(PebbleError),
    RemoteSendOutcomeUnknown(PebbleError),
}

impl From<PebbleError> for ReplayPendingMailOpError {
    fn from(error: PebbleError) -> Self {
        Self::Retryable(error)
    }
}

fn classify_remote_send_receipt_write(
    result: std::result::Result<(), PebbleError>,
) -> std::result::Result<(), ReplayPendingMailOpError> {
    result.map_err(ReplayPendingMailOpError::RemoteSendOutcomeUnknown)
}

fn classify_remote_send_call(
    result: std::result::Result<(), PebbleError>,
) -> std::result::Result<(), ReplayPendingMailOpError> {
    match result {
        Err(error @ PebbleError::Network(_)) => {
            Err(ReplayPendingMailOpError::RemoteSendOutcomeUnknown(error))
        }
        Err(error) => Err(ReplayPendingMailOpError::Retryable(error)),
        Ok(()) => Ok(()),
    }
}

fn is_permanent_error(e: &PebbleError) -> bool {
    matches!(e, PebbleError::UnsupportedProvider(_))
}

fn emit_pending_ops_changed(state: &crate::state::AppState) {
    let _ = state.ws_broadcast.send(
        serde_json::json!({
            "type": crate::events::MAIL_PENDING_OPS_CHANGED,
            "payload": serde_json::Value::Null,
        })
        .to_string(),
    );
}

async fn replay_pending_mail_op(
    state: &AppState,
    op: &PendingMailOp,
) -> std::result::Result<(), ReplayPendingMailOpError> {
    let Some(message) = state.store.get_message(&op.message_id)? else {
        debug!("Pending mail op {} skipped because message is gone", op.id);
        return Ok(());
    };
    let Some(account) = state.store.get_account(&op.account_id)? else {
        debug!("Pending mail op {} skipped because account is gone", op.id);
        return Ok(());
    };

    let payload = op_payload(op)?;
    match op.op_type.as_str() {
        "update_flags" => {
            let is_read = optional_bool(&payload, "is_read");
            let is_starred = optional_bool(&payload, "is_starred");
            replay_remote_update_flags(
                state,
                account.provider,
                &message,
                &payload,
                is_read,
                is_starred,
            )
            .await?;
        }
        "archive" => {
            replay_remote_archive(state, account.provider, &message, &payload).await?;
        }
        "unarchive" | "restore" => {
            replay_remote_restore(state, account.provider, &message, &payload).await?;
        }
        "delete" => {
            replay_remote_delete(state, account.provider, &message, &payload, false).await?;
        }
        "delete_permanent" => {
            replay_remote_delete(state, account.provider, &message, &payload, true).await?;
        }
        "move_to_folder" => {
            replay_remote_move_to_folder(state, account.provider, &message, &payload).await?;
        }
        "send" => {
            if !remote_side_effect_already_applied(op)? {
                replay_remote_send(state, account.provider.clone(), &account, &message).await?;
                classify_remote_send_receipt_write(
                    state.store.mark_pending_mail_op_remote_succeeded(&op.id),
                )?;
            }
        }
        other => {
            return Err(ReplayPendingMailOpError::Retryable(PebbleError::Internal(
                format!("Unsupported pending mail op type: {other}"),
            )));
        }
    }

    apply_pending_local_commit(&state.store, op)?;
    refresh_after_pending_commit(state, op)?;
    Ok(())
}

async fn replay_remote_update_flags(
    state: &AppState,
    provider_type: ProviderType,
    message: &pebble_core::Message,
    payload: &Value,
    is_read: Option<bool>,
    is_starred: Option<bool>,
) -> std::result::Result<(), PebbleError> {
    match provider_type {
        ProviderType::Gmail => {
            let add = string_array(payload, "add_labels").unwrap_or_else(|| {
                let mut labels = Vec::new();
                if is_read == Some(false) {
                    labels.push("UNREAD".to_string());
                }
                if is_starred == Some(true) {
                    labels.push("STARRED".to_string());
                }
                labels
            });
            let remove = string_array(payload, "remove_labels").unwrap_or_else(|| {
                let mut labels = Vec::new();
                if is_read == Some(true) {
                    labels.push("UNREAD".to_string());
                }
                if is_starred == Some(false) {
                    labels.push("STARRED".to_string());
                }
                labels
            });
            if add.is_empty() && remove.is_empty() {
                return Ok(());
            }
            connect_gmail(state, &message.account_id)
                .await?
                .modify_labels(&message.remote_id, &add, &remove)
                .await
        }
        ProviderType::Outlook => {
            let provider = connect_outlook(state, &message.account_id).await?;
            if let Some(read) = is_read {
                provider
                    .update_read_status(&message.remote_id, read)
                    .await?;
            }
            if let Some(starred) = is_starred {
                provider
                    .update_flag_status(&message.remote_id, starred)
                    .await?;
            }
            Ok(())
        }
        ProviderType::Imap => {
            let folder_remote_id = string_field(payload, "folder_remote_id")
                .or_else(|| {
                    find_message_folder(state, &message.id, &message.account_id)
                        .ok()
                        .map(|folder| folder.remote_id)
                })
                .ok_or_else(|| {
                    PebbleError::Internal("Pending update_flags has no IMAP folder".to_string())
                })?;
            let uid = message
                .remote_id
                .parse::<u32>()
                .map_err(|e| PebbleError::Internal(format!("Invalid IMAP UID: {e}")))?;
            let imap = connect_imap(state, &message.account_id).await?;
            let result = imap
                .set_flags(&folder_remote_id, uid, is_read, is_starred)
                .await;
            let _ = imap.disconnect().await;
            result
        }
        ProviderType::Pop3 => Ok(()),
    }
}

async fn replay_remote_archive(
    state: &AppState,
    provider_type: ProviderType,
    message: &pebble_core::Message,
    payload: &Value,
) -> std::result::Result<(), PebbleError> {
    match provider_type {
        ProviderType::Gmail => {
            let add = string_array(payload, "add_labels").unwrap_or_default();
            let remove =
                string_array(payload, "remove_labels").unwrap_or_else(|| vec!["INBOX".to_string()]);
            connect_gmail(state, &message.account_id)
                .await?
                .modify_labels(&message.remote_id, &add, &remove)
                .await
        }
        ProviderType::Outlook => {
            let target_remote_id =
                target_folder_remote_id(state, message, payload, FolderRole::Archive)?;
            let new_remote_id = connect_outlook(state, &message.account_id)
                .await?
                .move_message(&message.remote_id, &target_remote_id)
                .await?;
            state.store.update_remote_id(&message.id, &new_remote_id)?;
            Ok(())
        }
        ProviderType::Imap => {
            let source_remote_id = source_folder_remote_id(state, message, payload)?;
            let target_remote_id =
                target_folder_remote_id(state, message, payload, FolderRole::Archive)?;
            let _uid = parse_uid(message)?;
            let imap = connect_imap(state, &message.account_id).await?;
            let result = crate::patch::imap_move::move_message_and_update_uid(
                &imap,
                &state.store,
                message,
                &source_remote_id,
                &target_remote_id,
            )
            .await;
            let _ = imap.disconnect().await;
            result
        }
        ProviderType::Pop3 => Ok(()),
    }
}

async fn replay_remote_restore(
    state: &AppState,
    provider_type: ProviderType,
    message: &pebble_core::Message,
    payload: &Value,
) -> std::result::Result<(), PebbleError> {
    match provider_type {
        ProviderType::Gmail => {
            let current_folder = find_message_folder(state, &message.id, &message.account_id).ok();
            let provider = connect_gmail(state, &message.account_id).await?;
            if current_folder
                .as_ref()
                .is_some_and(|folder| folder.role == Some(FolderRole::Trash))
            {
                provider.untrash_message(&message.remote_id).await
            } else {
                provider
                    .modify_labels(&message.remote_id, &["INBOX".to_string()], &[])
                    .await
            }
        }
        ProviderType::Outlook => {
            let new_remote_id =
                if let Some(target_remote_id) = string_field(payload, "target_folder_remote_id") {
                    connect_outlook(state, &message.account_id)
                        .await?
                        .move_message(&message.remote_id, &target_remote_id)
                        .await?
                } else {
                    connect_outlook(state, &message.account_id)
                        .await?
                        .restore_message(&message.remote_id)
                        .await?
                };
            state.store.update_remote_id(&message.id, &new_remote_id)?;
            Ok(())
        }
        ProviderType::Imap => {
            let source_remote_id = source_folder_remote_id(state, message, payload)?;
            let target_remote_id =
                target_folder_remote_id(state, message, payload, FolderRole::Inbox)?;
            let _uid = parse_uid(message)?;
            let imap = connect_imap(state, &message.account_id).await?;
            let result = crate::patch::imap_move::move_message_and_update_uid(
                &imap,
                &state.store,
                message,
                &source_remote_id,
                &target_remote_id,
            )
            .await;
            let _ = imap.disconnect().await;
            result
        }
        ProviderType::Pop3 => Ok(()),
    }
}

async fn replay_remote_delete(
    state: &AppState,
    provider_type: ProviderType,
    message: &pebble_core::Message,
    payload: &Value,
    permanent: bool,
) -> std::result::Result<(), PebbleError> {
    match provider_type {
        ProviderType::Gmail => {
            let provider = connect_gmail(state, &message.account_id).await?;
            let result = if permanent {
                provider
                    .delete_message_permanently(&message.remote_id)
                    .await
            } else {
                provider.trash_message(&message.remote_id).await
            };
            classify_remote_delete_result(result, permanent)
        }
        ProviderType::Outlook => {
            let provider = connect_outlook(state, &message.account_id).await?;
            if permanent {
                classify_remote_delete_result(
                    provider
                        .delete_message_permanently(&message.remote_id)
                        .await,
                    true,
                )
            } else {
                let new_remote_id = provider.trash_message(&message.remote_id).await?;
                state.store.update_remote_id(&message.id, &new_remote_id)?;
                Ok(())
            }
        }
        ProviderType::Imap => {
            let source_remote_id = source_folder_remote_id(state, message, payload)?;
            let uid = parse_uid(message)?;
            let imap = connect_imap(state, &message.account_id).await?;
            let result = if permanent {
                imap.delete_message(&source_remote_id, uid).await
            } else if let Some(trash_remote_id) = string_field(payload, "trash_folder_remote_id")
                .or_else(|| {
                    find_folder_by_role(state, &message.account_id, FolderRole::Trash)
                        .ok()
                        .map(|folder| folder.remote_id)
                })
            {
                if trash_remote_id == source_remote_id {
                    imap.delete_message(&source_remote_id, uid).await
                } else {
                    crate::patch::imap_move::move_message_and_update_uid(
                        &imap,
                        &state.store,
                        message,
                        &source_remote_id,
                        &trash_remote_id,
                    )
                    .await
                }
            } else {
                imap.delete_message(&source_remote_id, uid).await
            };
            let _ = imap.disconnect().await;
            classify_remote_delete_result(result, permanent)
        }
        ProviderType::Pop3 => Ok(()),
    }
}

async fn replay_remote_move_to_folder(
    state: &AppState,
    provider_type: ProviderType,
    message: &pebble_core::Message,
    payload: &Value,
) -> std::result::Result<(), PebbleError> {
    match provider_type {
        ProviderType::Gmail => {
            let add = string_array(payload, "add_labels").unwrap_or_else(|| {
                string_field(payload, "target_folder_remote_id")
                    .into_iter()
                    .collect()
            });
            let remove =
                string_array(payload, "remove_labels").unwrap_or_else(|| vec!["INBOX".to_string()]);
            connect_gmail(state, &message.account_id)
                .await?
                .modify_labels(&message.remote_id, &add, &remove)
                .await
        }
        ProviderType::Outlook => {
            let target_remote_id =
                target_folder_remote_id(state, message, payload, FolderRole::Inbox)?;
            let new_remote_id = connect_outlook(state, &message.account_id)
                .await?
                .move_message(&message.remote_id, &target_remote_id)
                .await?;
            state.store.update_remote_id(&message.id, &new_remote_id)?;
            Ok(())
        }
        ProviderType::Imap => {
            let source_remote_id = source_folder_remote_id(state, message, payload)?;
            let target_remote_id =
                target_folder_remote_id(state, message, payload, FolderRole::Inbox)?;
            let _uid = parse_uid(message)?;
            let imap = connect_imap(state, &message.account_id).await?;
            let result = crate::patch::imap_move::move_message_and_update_uid(
                &imap,
                &state.store,
                message,
                &source_remote_id,
                &target_remote_id,
            )
            .await;
            let _ = imap.disconnect().await;
            result
        }
        ProviderType::Pop3 => Ok(()),
    }
}

async fn replay_remote_send(
    state: &AppState,
    _provider_type: ProviderType,
    account: &pebble_core::Account,
    message: &pebble_core::Message,
) -> std::result::Result<(), ReplayPendingMailOpError> {
    let attachment_paths = state
        .store
        .list_attachments_by_message(&message.id)?
        .into_iter()
        .filter_map(|attachment| attachment.local_path)
        .collect::<Vec<_>>();
    let outgoing = compose::outgoing_message_from_stored(message, attachment_paths);
    pebble_mail::sender::sender_mailbox(&outgoing.from)
        .map_err(ReplayPendingMailOpError::SenderIdentityBlocked)?;
    // Preparation errors are pre-dispatch and must never become outcome-unknown.
    let (transport, verified_from) = compose::prepare_send_transport(state, account)
        .await
        .map_err(|error| {
            if matches!(error, PebbleError::Validation(_)) {
                ReplayPendingMailOpError::SenderIdentityBlocked(error)
            } else {
                ReplayPendingMailOpError::Retryable(error)
            }
        })?;
    replay_prepared_send(&outgoing, &verified_from, || transport.send(&outgoing)).await
}

async fn replay_prepared_send<F, Fut>(
    outgoing: &pebble_core::traits::OutgoingMessage,
    verified_from: &pebble_core::EmailAddress,
    send: F,
) -> std::result::Result<(), ReplayPendingMailOpError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<(), PebbleError>>,
{
    if !outgoing
        .from
        .address
        .eq_ignore_ascii_case(&verified_from.address)
    {
        return Err(ReplayPendingMailOpError::SenderIdentityBlocked(PebbleError::Validation(
            "The queued message belongs to a different mailbox. Review it and compose a new message if needed.".into(),
        )));
    }
    classify_remote_send_call(send().await)
}

fn apply_pending_local_commit(
    store: &Store,
    op: &PendingMailOp,
) -> std::result::Result<(), PebbleError> {
    let payload = op_payload(op)?;
    let placeholder_attachments =
        if op.op_type == "send" && compose::send_finalize_deletes_placeholder(&payload) {
            store.list_attachments_by_message(&op.message_id)?
        } else {
            Vec::new()
        };
    match op.op_type.as_str() {
        "update_flags" => {
            store.update_message_flags(
                &op.message_id,
                optional_bool(&payload, "is_read"),
                optional_bool(&payload, "is_starred"),
            )?;
        }
        "archive" => {
            if let Some(folder_id) = string_field(&payload, "target_folder_id")
                .or_else(|| string_field(&payload, "archive_folder_id"))
                .or_else(|| {
                    crate::patch::folders::find_preferred_folder_by_role(
                        store,
                        &op.account_id,
                        FolderRole::Archive,
                    )
                    .ok()
                    .flatten()
                    .map(|folder| folder.id)
                })
            {
                store.move_message_to_folder(&op.message_id, &folder_id)?;
            } else {
                store.soft_delete_message(&op.message_id)?;
            }
        }
        "unarchive" | "restore" => {
            let folder_id = string_field(&payload, "target_folder_id")
                .or_else(|| {
                    store
                        .find_folder_by_role(&op.account_id, FolderRole::Inbox)
                        .ok()
                        .flatten()
                        .map(|folder| folder.id)
                })
                .ok_or_else(|| PebbleError::Internal("No restore target folder".to_string()))?;
            store.move_message_to_folder(&op.message_id, &folder_id)?;
        }
        "delete" => {
            crate::patch::batch_delete::finalize_pending_trash(
                store,
                &op.account_id,
                &op.message_id,
                string_field(&payload, "trash_folder_id"),
            )?;
        }
        "delete_permanent" => {
            store.hard_delete_messages(std::slice::from_ref(&op.message_id))?;
        }
        "move_to_folder" => {
            let folder_id = string_field(&payload, "target_folder_id")
                .ok_or_else(|| PebbleError::Internal("No move target folder".to_string()))?;
            store.move_message_to_folder(&op.message_id, &folder_id)?;
        }
        "send" => {
            if compose::send_finalize_deletes_placeholder(&payload) {
                store.complete_outgoing_send(&op.message_id, &op.id, None)?;
            } else {
                let sent_folder_id = string_field(&payload, "sent_folder_id")
                    .map(Ok)
                    .unwrap_or_else(|| {
                        compose::ensure_local_outgoing_folder(
                            store,
                            &op.account_id,
                            LocalOutgoingState::Sent,
                        )
                        .map(|folder| folder.id)
                    })?;
                store.complete_outgoing_send(&op.message_id, &op.id, Some(&sent_folder_id))?;
            }
        }
        other => {
            return Err(PebbleError::Internal(format!(
                "Unsupported pending mail op type: {other}"
            )));
        }
    }
    crate::commands::attachments::cleanup_local_attachment_records(&placeholder_attachments);
    Ok(())
}

fn refresh_after_pending_commit(
    state: &AppState,
    op: &PendingMailOp,
) -> std::result::Result<(), PebbleError> {
    if op.op_type == "delete_permanent" {
        remove_search_documents(state, std::slice::from_ref(&op.message_id))
    } else {
        refresh_search_document(state, &op.message_id)
    }
}

fn op_payload(op: &PendingMailOp) -> std::result::Result<Value, PebbleError> {
    let value: Value = serde_json::from_str(&op.payload_json)
        .map_err(|e| PebbleError::Internal(format!("Invalid pending op payload: {e}")))?;
    Ok(value.get("payload").cloned().unwrap_or(value))
}

fn remote_side_effect_already_applied(
    op: &PendingMailOp,
) -> std::result::Result<bool, PebbleError> {
    let value: Value = serde_json::from_str(&op.payload_json)
        .map_err(|e| PebbleError::Internal(format!("Invalid pending op payload: {e}")))?;
    Ok(value
        .get("remote_succeeded")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn optional_bool(payload: &Value, key: &str) -> Option<bool> {
    payload.get(key).and_then(Value::as_bool)
}

fn string_array(payload: &Value, key: &str) -> Option<Vec<String>> {
    payload.get(key).and_then(|value| {
        value.as_array().map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
    })
}

fn source_folder_remote_id(
    state: &AppState,
    message: &pebble_core::Message,
    payload: &Value,
) -> std::result::Result<String, PebbleError> {
    string_field(payload, "source_folder_remote_id")
        .or_else(|| {
            find_message_folder(state, &message.id, &message.account_id)
                .ok()
                .map(|folder| folder.remote_id)
        })
        .ok_or_else(|| PebbleError::Internal("No source folder for pending op".to_string()))
}

fn target_folder_remote_id(
    state: &AppState,
    message: &pebble_core::Message,
    payload: &Value,
    fallback_role: FolderRole,
) -> std::result::Result<String, PebbleError> {
    string_field(payload, "target_folder_remote_id")
        .or_else(|| string_field(payload, "archive_folder_remote_id"))
        .or_else(|| string_field(payload, "trash_folder_remote_id"))
        .or_else(|| {
            string_field(payload, "target_folder_id")
                .or_else(|| string_field(payload, "archive_folder_id"))
                .and_then(|folder_id| {
                    state
                        .store
                        .list_folders(&message.account_id)
                        .ok()?
                        .into_iter()
                        .find(|folder| folder.id == folder_id)
                        .map(|folder| folder.remote_id)
                })
        })
        .or_else(|| {
            find_folder_by_role(state, &message.account_id, fallback_role)
                .ok()
                .map(|folder| folder.remote_id)
        })
        .ok_or_else(|| PebbleError::Internal("No target folder for pending op".to_string()))
}

fn parse_uid(message: &pebble_core::Message) -> std::result::Result<u32, PebbleError> {
    message
        .remote_id
        .parse::<u32>()
        .map_err(|e| PebbleError::Internal(format!("Invalid IMAP UID: {e}")))
}
