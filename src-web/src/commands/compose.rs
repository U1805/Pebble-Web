use crate::commands::oauth::ensure_account_oauth_auth;
use pebble_core::traits::{MailTransport, OutgoingMessage};
use pebble_core::{
    new_id, now_timestamp, Account, EmailAddress, Folder, FolderRole, FolderType, Message,
    PebbleError, ProviderType,
};
use pebble_mail::smtp::SmtpSender;
use pebble_mail::{GmailProvider, OutlookProvider, SmtpConfig};
use serde_json::Value;

use crate::blocking::{run_blocking, run_blocking_core};
use crate::commands::accounts::StoredMailConfig;
use crate::commands::attachments::{
    cleanup_local_attachment_records, stage_local_attachment_records,
    validate_staged_attachment_paths,
};
use crate::commands::encrypted_store;
use crate::error::ApiError;
use crate::events;
use crate::state::{AppState, AppStateRef};

// 与桌面端 compose.rs 对齐的本地外发文件夹状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalOutgoingState {
    Sent,
    Queued,
}

fn local_outgoing_folder_spec(
    state: LocalOutgoingState,
) -> (&'static str, &'static str, Option<FolderRole>, i32) {
    match state {
        LocalOutgoingState::Sent => ("__local_sent__", "Sent", Some(FolderRole::Sent), 2),
        LocalOutgoingState::Queued => ("__local_outbox__", "Outbox", None, 3),
    }
}

pub(crate) fn ensure_local_outgoing_folder(
    store: &pebble_store::Store,
    account_id: &str,
    state: LocalOutgoingState,
) -> Result<Folder, PebbleError> {
    if state == LocalOutgoingState::Sent {
        if let Some(folder) = crate::patch::folders::find_preferred_folder_by_role(
            store,
            account_id,
            FolderRole::Sent,
        )? {
            return Ok(folder);
        }
    }
    let (remote_id, name, role, sort_order) = local_outgoing_folder_spec(state);
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
            address: address.trim().to_string(),
        })
        .filter(|address| !address.address.is_empty())
        .collect()
}

/// 读取账户 SMTP 配置（auth_data 解密）。
/// 加载账户 SMTP 配置并按代理模式解析生效代理（Inherit→账户或全局代理）。
pub(crate) fn load_smtp_config(
    state: &AppState,
    account_id: &str,
) -> Result<SmtpConfig, PebbleError> {
    let decrypted =
        encrypted_store::load_account_auth_data(&state.crypto, &state.store, account_id)?
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
    outgoing: &OutgoingMessage,
) -> Result<PreparedOutgoingSend, PebbleError> {
    let outbox =
        ensure_local_outgoing_folder(&state.store, &account.id, LocalOutgoingState::Queued)?;
    let delete_placeholder_after_send = matches!(
        account.provider,
        ProviderType::Gmail | ProviderType::Outlook
    );
    let sent_folder_id = if delete_placeholder_after_send {
        None
    } else {
        Some(ensure_local_outgoing_folder(&state.store, &account.id, LocalOutgoingState::Sent)?.id)
    };

    let now = now_timestamp();
    let id = new_id();
    let attachment_paths =
        validate_staged_attachment_paths(&state.attachments_dir, &outgoing.attachment_paths)?;
    let attachment_records =
        stage_local_attachment_records(&state.attachments_dir, &id, &attachment_paths)?;
    let mut message = Message {
        id: id.clone(),
        account_id: account.id.clone(),
        remote_id: format!("local-outbox-{id}"),
        message_id_header: Some(format!("<{id}@pebble.local>")),
        in_reply_to: outgoing.in_reply_to.clone(),
        references_header: outgoing.in_reply_to.clone(),
        thread_id: None,
        subject: outgoing.subject.clone(),
        snippet: outgoing.body_text.chars().take(200).collect(),
        from_address: outgoing.from.address.clone(),
        from_name: outgoing.from.name.clone().unwrap_or_default(),
        to_list: outgoing.to.clone(),
        cc_list: outgoing.cc.clone(),
        bcc_list: outgoing.bcc.clone(),
        body_text: outgoing.body_text.clone(),
        body_html_raw: outgoing.body_html.clone().unwrap_or_default(),
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
    crate::patch::outgoing::assign_thread_id(&state.store, &mut message)?;

    let payload = serde_json::json!({
        "provider_account_id": message.account_id,
        "remote_id": message.remote_id,
        "op": "send",
        "payload": {
            "local_finalize": if delete_placeholder_after_send {
                SEND_FINALIZE_DELETE_PLACEHOLDER
            } else {
                SEND_FINALIZE_MOVE_TO_SENT
            },
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

    // Keep the durable local Outbox state searchable before dispatch. This is
    // especially important for outcome-unknown sends, which intentionally stay
    // queued for manual review instead of being retried automatically.
    if let Err(error) = refresh_search_document(state, &message.id) {
        tracing::warn!(
            message_id = %message.id,
            "Failed to index prepared outgoing message: {error}"
        );
    }

    Ok(PreparedOutgoingSend {
        message,
        op_id,
        sent_folder_id,
        attachments: attachment_records,
        delete_placeholder_after_send,
    })
}

struct PreparedOutgoingSend {
    message: Message,
    op_id: String,
    sent_folder_id: Option<String>,
    attachments: Vec<pebble_core::Attachment>,
    delete_placeholder_after_send: bool,
}

pub(super) const SEND_FINALIZE_DELETE_PLACEHOLDER: &str = "delete_placeholder";
pub(super) const SEND_FINALIZE_MOVE_TO_SENT: &str = "move_to_sent";

pub(super) fn send_finalize_deletes_placeholder(payload: &serde_json::Value) -> bool {
    payload
        .get("local_finalize")
        .and_then(serde_json::Value::as_str)
        == Some(SEND_FINALIZE_DELETE_PLACEHOLDER)
}

pub(super) fn send_outcome_unknown_message(error: &PebbleError) -> String {
    format!(
        "Remote send outcome is unknown: {error}. Check Sent before dismissing or sending again."
    )
}

fn send_call_outcome_is_unknown(error: &PebbleError) -> bool {
    matches!(error, PebbleError::Network(_))
}

pub(crate) fn outgoing_message_from_stored(
    message: &Message,
    attachment_paths: Vec<String>,
) -> OutgoingMessage {
    OutgoingMessage {
        from: EmailAddress {
            name: (!message.from_name.is_empty()).then(|| message.from_name.clone()),
            address: message.from_address.clone(),
        },
        to: message.to_list.clone(),
        cc: message.cc_list.clone(),
        bcc: message.bcc_list.clone(),
        subject: message.subject.clone(),
        body_text: message.body_text.clone(),
        body_html: if message.body_html_raw.is_empty() {
            None
        } else {
            Some(message.body_html_raw.clone())
        },
        in_reply_to: message.in_reply_to.clone(),
        attachment_paths,
    }
}

async fn send_smtp_message(
    sender: &SmtpSender,
    outgoing: &OutgoingMessage,
) -> Result<(), PebbleError> {
    let to = outgoing
        .to
        .iter()
        .map(|address| address.address.clone())
        .collect::<Vec<_>>();
    let cc = outgoing
        .cc
        .iter()
        .map(|address| address.address.clone())
        .collect::<Vec<_>>();
    let bcc = outgoing
        .bcc
        .iter()
        .map(|address| address.address.clone())
        .collect::<Vec<_>>();
    sender
        .send(
            &outgoing.from,
            &to,
            &cc,
            &bcc,
            &outgoing.subject,
            &outgoing.body_text,
            outgoing.body_html.as_deref(),
            outgoing.in_reply_to.as_deref(),
            &outgoing.attachment_paths,
        )
        .await
}

pub(crate) enum PreparedSendTransport {
    Gmail(GmailProvider),
    Outlook(OutlookProvider),
    Smtp(SmtpSender),
}

impl PreparedSendTransport {
    pub(crate) async fn send(
        &self,
        outgoing: &OutgoingMessage,
    ) -> std::result::Result<(), PebbleError> {
        match self {
            Self::Gmail(provider) => provider.send_message(outgoing).await,
            Self::Outlook(provider) => provider.send_message(outgoing).await,
            Self::Smtp(sender) => send_smtp_message(sender, outgoing).await,
        }
    }
}

pub(crate) async fn prepare_send_transport(
    state: &AppState,
    account: &Account,
) -> std::result::Result<(PreparedSendTransport, EmailAddress), PebbleError> {
    let mut from = account.sender_identity();
    pebble_mail::sender::sender_mailbox(&from)?;
    match account.provider {
        ProviderType::Gmail => {
            let auth = ensure_account_oauth_auth(state, &account.id, "gmail").await?;
            let identity = super::oauth::fetch_mailbox_identity("gmail", &auth).await?;
            state.store.apply_verified_oauth_identity(
                &account.id,
                &account.email,
                &identity,
                false,
            )?;
            from.address = identity.email;
            Ok((
                PreparedSendTransport::Gmail(GmailProvider::new_with_proxy(
                    auth.tokens.access_token,
                    auth.proxy,
                )?),
                from,
            ))
        }
        ProviderType::Outlook => {
            let auth = ensure_account_oauth_auth(state, &account.id, "outlook").await?;
            let identity = super::oauth::fetch_mailbox_identity("outlook", &auth).await?;
            state.store.apply_verified_oauth_identity(
                &account.id,
                &account.email,
                &identity,
                false,
            )?;
            from = EmailAddress {
                name: identity.display_name,
                address: identity.email,
            };
            Ok((
                PreparedSendTransport::Outlook(OutlookProvider::new_with_proxy(
                    auth.tokens.access_token,
                    account.id.clone(),
                    auth.proxy,
                )?),
                from,
            ))
        }
        ProviderType::Imap | ProviderType::Pop3 => {
            let smtp_config = load_smtp_config(state, &account.id)?;
            Ok((
                PreparedSendTransport::Smtp(SmtpSender::new(
                    smtp_config.host,
                    smtp_config.port,
                    smtp_config.username,
                    smtp_config.password,
                    smtp_config.security,
                    smtp_config.accept_invalid_certs,
                    smtp_config.proxy,
                )),
                from,
            ))
        }
    }
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
        "payload": serde_json::Value::Null,
    })
    .to_string()
}

/// 发送邮件：验证发件身份后复用共享 SMTP/Gmail/Outlook transport。
/// 暂存附件先复制到本地外发记录，再由 SMTP 发送；pending op 状态机与桌面端一致。
pub async fn send_email(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        to: Vec<String>,
        cc: Vec<String>,
        bcc: Vec<String>,
        subject: String,
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

    // Resolve every provider credential/token before creating a durable
    // in-progress send. A token refresh or provider-construction failure is a
    // known pre-dispatch failure and must never be recorded as an
    // outcome-unknown send.
    let (transport, from) = prepare_send_transport(&state, &account).await?;
    let outgoing = OutgoingMessage {
        from,
        to: parse_recipients(args.to),
        cc: parse_recipients(args.cc),
        bcc: parse_recipients(args.bcc),
        subject: args.subject,
        body_text: args.body_text,
        body_html: args.body_html,
        in_reply_to: args.in_reply_to,
        attachment_paths: args.attachment_paths.unwrap_or_default(),
    };

    let prepared = {
        let state_for_prepare = state.clone();
        let account_for_prepare = account.clone();
        run_blocking(move || {
            prepare_outgoing_send_locally(&state_for_prepare, &account_for_prepare, &outgoing)
        })
        .await?
    };
    let durable_attachment_paths: Vec<String> = prepared
        .attachments
        .iter()
        .filter_map(|attachment| attachment.local_path.clone())
        .collect();
    let durable_outgoing =
        outgoing_message_from_stored(&prepared.message, durable_attachment_paths);
    let send_result = transport.send(&durable_outgoing).await;

    match send_result {
        Ok(()) => {
            let receipt_result = run_blocking_core({
                let state = state.clone();
                let op_id = prepared.op_id.clone();
                move || state.store.mark_pending_mail_op_remote_succeeded(&op_id)
            })
            .await;
            if let Err(receipt_error) = receipt_result {
                let transition = run_blocking_core({
                    let state = state.clone();
                    let op_id = prepared.op_id.clone();
                    let message = send_outcome_unknown_message(&receipt_error);
                    move || {
                        state
                            .store
                            .mark_pending_mail_op_outcome_unknown(&op_id, &message)
                    }
                })
                .await;
                emit_pending_ops_changed(&state);
                if let Err(error) = transition {
                    tracing::warn!(
                        "Send operation {} could not persist outcome-unknown state: {error}",
                        prepared.op_id
                    );
                }
                return Ok(Value::Null);
            }

            let finalize_result = run_blocking_core({
                let state = state.clone();
                let message_id = prepared.message.id.clone();
                let op_id = prepared.op_id.clone();
                let sent_folder_id = prepared.sent_folder_id.clone();
                move || {
                    state.store.complete_outgoing_send(
                        &message_id,
                        &op_id,
                        sent_folder_id.as_deref(),
                    )?;
                    if let Err(error) = refresh_search_document(&state, &message_id) {
                        tracing::warn!("Failed to index sent message {message_id}: {error}");
                    }
                    Ok(())
                }
            })
            .await;

            if let Err(finalize_error) = finalize_result {
                let transition = run_blocking_core({
                    let state = state.clone();
                    let op_id = prepared.op_id.clone();
                    let error = finalize_error.to_string();
                    move || state.store.mark_pending_mail_op_failed(&op_id, &error)
                })
                .await;
                if let Err(error) = transition {
                    tracing::warn!(
                        "Send operation {} could not persist local-finalize retry state: {error}",
                        prepared.op_id
                    );
                }
            } else if prepared.delete_placeholder_after_send {
                cleanup_local_attachment_records(&prepared.attachments);
            }
            emit_pending_ops_changed(&state);
            Ok(Value::Null)
        }
        Err(error) if send_call_outcome_is_unknown(&error) => {
            let transition = run_blocking_core({
                let state = state.clone();
                let op_id = prepared.op_id.clone();
                let message = send_outcome_unknown_message(&error);
                move || {
                    state
                        .store
                        .mark_pending_mail_op_outcome_unknown(&op_id, &message)
                }
            })
            .await;
            if let Err(transition_error) = transition {
                tracing::warn!(
                    "Send operation {} could not persist outcome-unknown state: {transition_error}",
                    prepared.op_id
                );
            }
            emit_pending_ops_changed(&state);
            Ok(Value::Null)
        }
        Err(error) => {
            let op_id = prepared.op_id.clone();
            let message_id = prepared.message.id.clone();
            let attachments = prepared.attachments.clone();
            let state_for_cleanup = state.clone();
            let cleanup_result = run_blocking_core(move || {
                state_for_cleanup
                    .store
                    .discard_prepared_outgoing_send(&message_id, &op_id)?;
                cleanup_local_attachment_records(&attachments);
                if let Err(index_error) = refresh_search_document(&state_for_cleanup, &message_id) {
                    tracing::warn!(
                        "Failed to remove discarded outgoing message {message_id} from search: {index_error}"
                    );
                }
                Ok::<(), PebbleError>(())
            })
            .await;
            emit_pending_ops_changed(&state);
            if let Err(cleanup_error) = cleanup_result {
                tracing::warn!(
                    "Known send failure cleanup also failed for {}: {cleanup_error}",
                    prepared.op_id
                );
            }
            Err(ApiError::from_pebble(error))
        }
    }
}
