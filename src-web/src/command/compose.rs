use pebble_core::{
    new_id, now_timestamp, Account, EmailAddress, Folder, FolderRole, FolderType, Message,
    PebbleError, ProviderType,
};
use pebble_mail::smtp::SmtpSender;
use pebble_mail::SmtpConfig;
use serde_json::Value;

use crate::command::accounts::StoredMailConfig;
use crate::command::run_blocking;
use crate::credentials;
use crate::error::ApiError;
use crate::state::AppStateRef;

// 与桌面端 compose.rs 对齐的本地外发文件夹状态（本批无附件路径，后续 stage 命令复用）。
enum LocalOutgoingState {
    Sent,
    Queued,
}

fn local_outgoing_folder_spec(state: &LocalOutgoingState) -> (&'static str, &'static str, Option<FolderRole>, i32) {
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
        .map(|address| EmailAddress { name: None, address })
        .collect()
}

/// 读取账户 SMTP 配置（auth_data 解密）。
/// proxy_mode 的全局代理语义（Inherit→全局设置）待网络命令接入后对齐，
/// 当前直接使用账户级 SMTP 配置中的 proxy。
fn load_smtp_config(
    state: &AppStateRef,
    account_id: &str,
) -> Result<SmtpConfig, PebbleError> {
    let decrypted = credentials::load_account_auth_data(&state.crypto, &state.store, account_id)
        .map_err(|e| PebbleError::Internal(e))?
        .ok_or_else(|| PebbleError::Internal(format!("No auth data found for account {account_id}")))?;
    let config: serde_json::Value = serde_json::from_slice(&decrypted)
        .map_err(|e| PebbleError::Internal(format!("Failed to parse decrypted config: {e}")))?;
    let stored: StoredMailConfig = serde_json::from_value(
        config
            .get("smtp")
            .cloned()
            .ok_or_else(|| PebbleError::Internal("No SMTP config in auth data".to_string()))?,
    )
    .map_err(|e| PebbleError::Internal(format!("Failed to deserialize SMTP config: {e}")))?;
    Ok(stored.into_smtp())
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
) -> Result<PreparedOutgoingSend, PebbleError> {
    let outbox = ensure_local_outgoing_folder(&state.store, &account.id, LocalOutgoingState::Queued)?;
    let sent_folder_id = Some(
        ensure_local_outgoing_folder(&state.store, &account.id, LocalOutgoingState::Sent)?.id,
    );

    let now = now_timestamp();
    let id = new_id();
    // 本批无附件：attachment 列表恒为空
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
        has_attachments: false,
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
    let op_id = state
        .store
        .prepare_outgoing_send(&message, std::slice::from_ref(&outbox.id), &[], &payload.to_string())?;

    Ok(PreparedOutgoingSend {
        message,
        op_id,
        sent_folder_id,
    })
}

struct PreparedOutgoingSend {
    message: Message,
    op_id: String,
    sent_folder_id: Option<String>,
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

/// 发送邮件（P0：IMAP/POP3 账户走 SMTP；Gmail/Outlook OAuth 后续批次）。
/// 无附件路径。pending op 远成功/失败状态机与桌面端一致。
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
    if let Some(paths) = &args.attachment_paths {
        if !paths.is_empty() {
            return Err(ApiError::BadRequest(
                "attachments not supported yet; use staged upload in a later milestone"
                    .to_string(),
            ));
        }
    }

    let state_for_account = state.clone();
    let account = run_blocking(move || {
        state_for_account
            .store
            .get_account(&args.account_id)?
            .ok_or_else(|| PebbleError::Internal(format!("Account not found: {}", args.account_id)))
    })
    .await?;

    match account.provider {
        ProviderType::Gmail | ProviderType::Outlook => {
            return Err(ApiError::BadRequest(
                "gmail/outlook providers not supported yet".to_string(),
            ));
        }
        ProviderType::Imap | ProviderType::Pop3 => {}
    }

    let smtp_config = load_smtp_config(&state, &account.id).map_err(ApiError::from_pebble)?;
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
            )
        })
        .await?
    };

    let sender = SmtpSender::new(
        smtp_config.host,
        smtp_config.port,
        smtp_config.username,
        smtp_config.password,
        smtp_config.security,
        smtp_config.accept_invalid_certs,
        smtp_config.proxy,
    );

    let to_addrs: Vec<String> = prepared.message.to_list.iter().map(|a| a.address.clone()).collect();
    let cc_addrs: Vec<String> = prepared.message.cc_list.iter().map(|a| a.address.clone()).collect();
    let bcc_addrs: Vec<String> = prepared.message.bcc_list.iter().map(|a| a.address.clone()).collect();

    let send_result = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
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
            &[],
        ),
    )
    .await
    {
        Ok(inner) => inner,
        Err(_) => Err(PebbleError::Network("SMTP send timed out after 30s".to_string())),
    };

    match send_result {
        Ok(()) => {
            // TODO: 事件推送（ws_broadcast MAIL_PENDING_OPS_CHANGED）接入阶段六
            run_blocking({
                let state = state.clone();
                let message_id = prepared.message.id.clone();
                let op_id = prepared.op_id.clone();
                let sent_folder_id = prepared.sent_folder_id.clone();
                move || {
                    state
                        .store
                        .mark_pending_mail_op_remote_succeeded(&op_id)?;
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
            .await?;
            Ok(Value::Null)
        }
        Err(error) => {
            let op_id = prepared.op_id.clone();
            let message_id = prepared.message.id.clone();
            run_blocking(move || {
                if let Err(transition) =
                    state.store.discard_prepared_outgoing_send(&message_id, &op_id)
                {
                    tracing::warn!("Failed to discard failed outgoing send: {transition}");
                }
                Ok::<(), PebbleError>(())
            })
            .await?;
            Err(ApiError::from_pebble(error))
        }
    }
}