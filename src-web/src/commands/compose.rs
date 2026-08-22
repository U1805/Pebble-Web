use pebble_core::traits::OutgoingMessage;
use pebble_core::{
    new_id, now_timestamp, Account, EmailAddress, Folder, FolderRole, FolderType, Message,
    PebbleError, ProviderType,
};
use pebble_mail::smtp::SmtpSender;
use pebble_mail::SmtpConfig;
use serde_json::Value;
use std::time::Duration;

use crate::commands::accounts::StoredMailConfig;
use crate::commands::attachments::{
    cleanup_local_attachment_records, stage_local_attachment_records,
    validate_staged_attachment_paths,
};
use crate::blocking::run_blocking;
use crate::commands::encrypted_store;
use crate::error::ApiError;
use crate::events;
use crate::state::AppStateRef;

// 与桌面端 compose.rs 对齐的本地外发文件夹状态。
enum LocalOutgoingState {
    Sent,
    Queued,
}

fn local_outgoing_folder_spec(
    state: &LocalOutgoingState,
) -> (&'static str, &'static str, Option<FolderRole>, i32) {
    match state {
        LocalOutgoingState::Sent => ("__local_sent__", "Sent", Some(FolderRole::Sent), 2),
        LocalOutgoingState::Queued => ("__local_outbox__", "Outbox", None, 3),
    }
}

fn ensure_local_outgoing_folder(
    store: &pebble_store::Store,
    account_id: &str,
    state: LocalOutgoingState,
) -> Result<Folder, PebbleError> {
    if matches!(state, LocalOutgoingState::Sent) {
        if let Some(folder) = store.find_folder_by_role(account_id, FolderRole::Sent)? {
            return Ok(folder);
        }
    }
    let (remote_id, name, role, sort_order) = local_outgoing_folder_spec(&state);
    if let Some(folder) = store.find_folder_by_name(account_id, name)? {
        return Ok(folder);
    }
    let folder = Folder {
        id: new_id(),
        account_id: account_id.to_string(),
        remote_id: remote_id.to_string(),
        name: name.to_string(),
        folder_type: FolderType::Folder,
        role,
        parent_id: None,
        color: None,
        is_system: true,
        sort_order,
    };
    let id = store.insert_folder(&folder)?;
    Ok(Folder { id, ..folder })
}

fn parse_recipients(addresses: Vec<String>) -> Vec<EmailAddress> {
    addresses
        .into_iter()
        .map(|address| EmailAddress {
            name: None,
            address,
        })
        .collect()
}

/// 读取账户 SMTP 配置（auth_data 解密）。
/// 加载账户 SMTP 配置并按代理模式解析生效代理（Inherit→账户或全局代理）。
fn load_smtp_config(state: &AppStateRef, account_id: &str) -> Result<SmtpConfig, PebbleError> {
    let decrypted = encrypted_store::load_account_auth_data(&state.crypto, &state.store, account_id)?
        .ok_or_else(|| {
            PebbleError::Internal(format!("No auth data found for account {account_id}"))
        })?;
    let config: serde_json::Value = serde_json::from_slice(&decrypted)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse decrypted config: {e}")))?;
    let stored: StoredMailConfig = serde_json::from_value(
        config
            .get("smtp")
            .cloned()
            .ok_or_else(|| PebbleError::Internal("No SMTP config in auth data".to_string()))?,
    )
    .map_err(|e| PebbleError::Internal(format!("Failed to deserialize SMTP config: {e}")))?;
    let mut smtp = stored.into_smtp();
    let mode = crate::commands::network::account_proxy_mode_from_auth_value(&config);
    smtp.proxy = crate::commands::network::resolve_mail_proxy_from_mode(
        &state.crypto,
        &state.store,
        mode,
        smtp.proxy,
    )?;
    Ok(smtp)
}

/// 将已准备的外发消息写入本地 Outbox 并登记 pending op。
fn prepare_outgoing_send_locally(
    state: &AppStateRef,
    account: &Account,
    to: Vec<EmailAddress>,
    cc: Vec<EmailAddress>,
    bcc: Vec<EmailAddress>,
    subject: &str,
    body_text: &str,
    body_html: Option<String>,
    in_reply_to: Option<String>,
    attachment_paths: &[String],
) -> Result<PreparedOutgoingSend, PebbleError> {
    let outbox =
        ensure_local_outgoing_folder(&state.store, &account.id, LocalOutgoingState::Queued)?;
    let sent_folder_id =
        Some(ensure_local_outgoing_folder(&state.store, &account.id, LocalOutgoingState::Sent)?.id);

    let now = now_timestamp();
    let id = new_id();
    let attachment_paths =
        validate_staged_attachment_paths(&state.attachments_dir, attachment_paths)?;
    let attachment_records =
        stage_local_attachment_records(&state.attachments_dir, &id, &attachment_paths)?;
    let message = Message {
        id: id.clone(),
        account_id: account.id.clone(),
        remote_id: format!("local-outbox-{id}"),
        message_id_header: Some(format!("<{id}@pebble.local>")),
        in_reply_to: in_reply_to.clone(),
        references_header: in_reply_to,
        thread_id: None,
        subject: subject.to_string(),
        snippet: body_text.chars().take(200).collect(),
        from_address: account.email.clone(),
        from_name: account.display_name.clone(),
        to_list: to,
        cc_list: cc,
        bcc_list: bcc,
        body_text: body_text.to_string(),
        body_html_raw: body_html.unwrap_or_default(),
        has_attachments: !attachment_records.is_empty(),
        is_read: true,
        is_starred: false,
        is_draft: false,
        date: now,
        remote_version: None,
        is_deleted: false,
        deleted_at: None,
        created_at: now,
        updated_at: now,
    };

    let payload = serde_json::json!({
        "provider_account_id": message.account_id,
        "remote_id": message.remote_id,
        "op": "send",
        "payload": {
            "local_finalize": "move_to_sent",
            "sent_folder_id": sent_folder_id,
        },
    });
    let op_id = match state.store.prepare_outgoing_send(
        &message,
        std::slice::from_ref(&outbox.id),
        &attachment_records,
        &payload.to_string(),
    ) {
        Ok(op_id) => op_id,
        Err(error) => {
            cleanup_local_attachment_records(&attachment_records);
            return Err(error);
        }
    };

    Ok(PreparedOutgoingSend {
        message,
        op_id,
        sent_folder_id,
        attachments: attachment_records,
    })
}

struct PreparedOutgoingSend {
    message: Message,
    op_id: String,
    sent_folder_id: Option<String>,
    attachments: Vec<pebble_core::Attachment>,
}

/// Send an OAuth message through the shared Gmail/Outlook provider.  The
/// provider implementations already own the wire format and attachment
/// handling; Web only supplies the decrypted access token and local paths.
async fn send_oauth_message(
    state: &AppStateRef,
    account: &Account,
    message: &Message,
    attachment_paths: &[String],
) -> Result<(), PebbleError> {
    let provider = crate::oauth::load_oauth_provider(state, account).await?;
    provider
        .send_message(&OutgoingMessage {
            to: message.to_list.clone(),
            cc: message.cc_list.clone(),
            bcc: message.bcc_list.clone(),
            subject: message.subject.clone(),
            body_text: message.body_text.clone(),
            body_html: (!message.body_html_raw.is_empty()).then_some(message.body_html_raw.clone()),
            in_reply_to: message.in_reply_to.clone(),
            attachment_paths: attachment_paths.to_vec(),
        })
        .await
}

/// 同步外发消息到搜索索引（与桌面端 refresh_search_document 等价）。
fn refresh_search_document(state: &AppStateRef, message_id: &str) -> Result<(), PebbleError> {
    let ids = vec![message_id.to_string()];
    state.store.add_search_pending(&ids, "index")?;
    match state.store.get_message(message_id)? {
        Some(message) if !message.is_deleted => {
            let folder_ids = state.store.get_message_folder_ids(message_id)?;
            if folder_ids.is_empty() {
                state.search.remove_message(message_id)?;
            } else {
                state.search.index_message(&message, &folder_ids)?;
            }
        }
        Some(_) | None => {
            state.search.remove_message(message_id)?;
        }
    }
    state.search.commit()?;
    state.store.clear_search_pending(&ids)?;
    Ok(())
}

fn emit_pending_ops_changed(state: &AppStateRef) {
    let _ = state.ws_broadcast.send(pending_ops_changed_event());
}

fn pending_ops_changed_event() -> String {
    serde_json::json!({
        "type": events::MAIL_PENDING_OPS_CHANGED,
        "payload": {},
    })
    .to_string()
}

/// 发送邮件（P0：IMAP/POP3 账户走 SMTP；Gmail/Outlook OAuth 后续批次）。
/// 暂存附件先复制到本地外发记录，再由 SMTP 发送；pending op 状态机与桌面端一致。
pub async fn send_email(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        to: Vec<String>,
        #[serde(default)]
        cc: Vec<String>,
        #[serde(default)]
        bcc: Vec<String>,
        #[serde(default)]
        subject: String,
        #[serde(default)]
        body_text: String,
        #[serde(default)]
        body_html: Option<String>,
        #[serde(default)]
        in_reply_to: Option<String>,
        #[serde(default)]
        attachment_paths: Option<Vec<String>>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid send_email args: {e}")))?;

    let state_for_account = state.clone();
    let account = run_blocking(move || {
        state_for_account
            .store
            .get_account(&args.account_id)?
            .ok_or_else(|| PebbleError::Internal(format!("Account not found: {}", args.account_id)))
    })
    .await?;

    let smtp_config = match account.provider {
        ProviderType::Imap | ProviderType::Pop3 => {
            Some(load_smtp_config(&state, &account.id).map_err(ApiError::from_pebble)?)
        }
        ProviderType::Gmail | ProviderType::Outlook => None,
    };
    let to = parse_recipients(args.to);
    let cc = parse_recipients(args.cc);
    let bcc = parse_recipients(args.bcc);

    let prepared = {
        let state_for_prepare = state.clone();
        let account_for_prepare = account.clone();
        run_blocking(move || {
            prepare_outgoing_send_locally(
                &state_for_prepare,
                &account_for_prepare,
                to,
                cc,
                bcc,
                &args.subject,
                &args.body_text,
                args.body_html,
                args.in_reply_to,
                &args.attachment_paths.clone().unwrap_or_default(),
            )
        })
        .await?
    };
    // The outgoing operation is visible to PendingOps immediately, before
    // the network request completes (matching the desktop event contract).
    emit_pending_ops_changed(&state);

    let durable_attachment_paths: Vec<String> = prepared
        .attachments
        .iter()
        .filter_map(|attachment| attachment.local_path.clone())
        .collect();
    let send_result = if let Some(smtp_config) = smtp_config {
        let sender = SmtpSender::new(
            smtp_config.host,
            smtp_config.port,
            smtp_config.username,
            smtp_config.password,
            smtp_config.security,
            smtp_config.accept_invalid_certs,
            smtp_config.proxy,
        );
        let to_addrs: Vec<String> = prepared
            .message
            .to_list
            .iter()
            .map(|a| a.address.clone())
            .collect();
        let cc_addrs: Vec<String> = prepared
            .message
            .cc_list
            .iter()
            .map(|a| a.address.clone())
            .collect();
        let bcc_addrs: Vec<String> = prepared
            .message
            .bcc_list
            .iter()
            .map(|a| a.address.clone())
            .collect();
        match tokio::time::timeout(
            Duration::from_secs(30),
            sender.send(
                &account.email,
                &to_addrs,
                &cc_addrs,
                &bcc_addrs,
                &prepared.message.subject,
                &prepared.message.body_text,
                (!prepared.message.body_html_raw.is_empty())
                    .then_some(prepared.message.body_html_raw.as_str()),
                prepared.message.in_reply_to.as_deref(),
                &durable_attachment_paths,
            ),
        )
        .await
        {
            Ok(inner) => inner,
            Err(_) => Err(PebbleError::Network(
                "SMTP send timed out after 30s".to_string(),
            )),
        }
    } else {
        tokio::time::timeout(
            Duration::from_secs(30),
            send_oauth_message(
                &state,
                &account,
                &prepared.message,
                &durable_attachment_paths,
            ),
        )
        .await
        .map_err(|_| PebbleError::Network("OAuth send timed out after 30s".to_string()))?
    };

    match send_result {
        Ok(()) => {
            let finalize_result = run_blocking({
                let state = state.clone();
                let message_id = prepared.message.id.clone();
                let op_id = prepared.op_id.clone();
                let sent_folder_id = prepared.sent_folder_id.clone();
                move || {
                    state.store.mark_pending_mail_op_remote_succeeded(&op_id)?;
                    state.store.complete_outgoing_send(
                        &message_id,
                        &op_id,
                        sent_folder_id.as_deref(),
                    )?;
                    if let Err(e) = refresh_search_document(&state, &message_id) {
                        tracing::warn!("Failed to index sent message {message_id}: {e}");
                    }
                    Ok(())
                }
            })
            .await;
            emit_pending_ops_changed(&state);
            finalize_result?;
            Ok(Value::Null)
        }
        Err(error) => {
            let op_id = prepared.op_id.clone();
            let message_id = prepared.message.id.clone();
            let attachments = prepared.attachments.clone();
            let state_for_cleanup = state.clone();
            let cleanup_result = run_blocking(move || {
                if let Err(transition) = state_for_cleanup
                    .store
                    .discard_prepared_outgoing_send(&message_id, &op_id)
                {
                    tracing::warn!("Failed to discard failed outgoing send: {transition}");
                }
                cleanup_local_attachment_records(&attachments);
                Ok::<(), PebbleError>(())
            })
            .await;
            emit_pending_ops_changed(&state);
            cleanup_result?;
            Err(ApiError::from_pebble(error))
        }
    }
}
