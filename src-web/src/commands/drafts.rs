use crate::state::{AppState, AppStateRef};
use pebble_core::{traits::DraftProvider, DraftMessage, EmailAddress, PebbleError, ProviderType};
use tracing::warn;

use super::attachments::{
    cleanup_local_attachment_records, stage_local_attachment_records,
    validate_staged_attachment_paths,
};
use super::messages::provider_dispatch::ConnectedProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DraftProvenance {
    local_id: Option<String>,
    remote_id: Option<String>,
}

trait RemoteDraftOperations {
    async fn create_draft(&self, draft: &DraftMessage) -> Result<String, PebbleError>;
    async fn update_draft(&self, draft_id: &str, draft: &DraftMessage) -> Result<(), PebbleError>;
    async fn delete_draft(&self, draft_id: &str) -> Result<(), PebbleError>;
}

impl RemoteDraftOperations for ConnectedProvider {
    async fn create_draft(&self, draft: &DraftMessage) -> Result<String, PebbleError> {
        match self {
            Self::Gmail(provider) => provider.save_draft(draft).await,
            Self::Outlook(provider) => provider.save_draft(draft).await,
            Self::Imap(_) => Err(PebbleError::UnsupportedProvider(
                "IMAP remote drafts are not supported".to_string(),
            )),
        }
    }

    async fn update_draft(&self, draft_id: &str, draft: &DraftMessage) -> Result<(), PebbleError> {
        match self {
            Self::Gmail(provider) => provider.update_draft(draft_id, draft).await,
            Self::Outlook(provider) => provider.update_draft(draft_id, draft).await,
            Self::Imap(_) => Err(PebbleError::UnsupportedProvider(
                "IMAP remote drafts are not supported".to_string(),
            )),
        }
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), PebbleError> {
        match self {
            Self::Gmail(provider) => provider.delete_draft(draft_id).await,
            Self::Outlook(provider) => provider.delete_draft(draft_id).await,
            Self::Imap(_) => Ok(()),
        }
    }
}

fn requires_remote_draft_delete(provider_type: Option<ProviderType>) -> bool {
    matches!(
        provider_type,
        Some(ProviderType::Gmail | ProviderType::Outlook)
    )
}

fn resolve_draft_provenance(
    store: &pebble_store::Store,
    account_id: &str,
    existing_draft_id: Option<&str>,
) -> std::result::Result<DraftProvenance, PebbleError> {
    let Some(draft_id) = existing_draft_id else {
        return Ok(DraftProvenance {
            local_id: None,
            remote_id: None,
        });
    };

    let Some(existing) = store.get_message(draft_id)? else {
        return Ok(DraftProvenance {
            local_id: None,
            remote_id: Some(draft_id.to_string()),
        });
    };

    if existing.account_id != account_id || !existing.is_draft {
        return Err(PebbleError::Validation(
            "Existing draft does not belong to the selected account".to_string(),
        ));
    }

    Ok(DraftProvenance {
        local_id: Some(existing.id),
        remote_id: (!existing.remote_id.is_empty()).then_some(existing.remote_id),
    })
}

fn cleanup_unreferenced_local_attachment_paths(state: &AppState, local_paths: &[String]) {
    for path in local_paths {
        match state.store.is_attachment_local_path_referenced(path) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                warn!("Failed to check local attachment reference {path}: {error}");
                continue;
            }
        }
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!("Failed to delete local draft attachment {path}: {error}");
            }
        }
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

fn delete_local_draft(state: &AppState, draft_id: &str) -> Result<(), PebbleError> {
    let local_paths: Vec<String> = state
        .store
        .list_attachments_by_message(draft_id)?
        .into_iter()
        .filter_map(|attachment| attachment.local_path)
        .collect();
    state.store.hard_delete_messages(&[draft_id.to_string()])?;
    cleanup_unreferenced_local_attachment_paths(state, &local_paths);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn save_draft(
    state: AppStateRef,
    account_id: String,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    in_reply_to: Option<String>,
    attachment_paths: Option<Vec<String>>,
    existing_draft_id: Option<String>,
) -> std::result::Result<String, PebbleError> {
    let raw_attachment_paths = attachment_paths.unwrap_or_default();
    let attachment_paths = if raw_attachment_paths.is_empty() {
        raw_attachment_paths
    } else {
        validate_staged_attachment_paths(&state.attachments_dir, &raw_attachment_paths)?
    };
    let provenance = resolve_draft_provenance(
        state.store.as_ref(),
        &account_id,
        existing_draft_id.as_deref(),
    )?;
    let account = state
        .store
        .get_account(&account_id)?
        .ok_or_else(|| PebbleError::Validation("Account not found".into()))?;
    let sender = account.sender_identity();
    pebble_mail::sender::sender_mailbox(&sender)?;
    let draft = DraftMessage {
        from: Some(sender),
        id: provenance.remote_id.clone(),
        to: to
            .into_iter()
            .map(|a| EmailAddress {
                name: None,
                address: a,
            })
            .collect(),
        cc: cc
            .into_iter()
            .map(|a| EmailAddress {
                name: None,
                address: a,
            })
            .collect(),
        bcc: bcc
            .into_iter()
            .map(|a| EmailAddress {
                name: None,
                address: a,
            })
            .collect(),
        subject,
        body_text,
        body_html,
        in_reply_to,
        attachment_paths,
    };

    if matches!(
        account.provider,
        ProviderType::Gmail | ProviderType::Outlook
    ) {
        // Reuse the verified connection itself; a second connect could read different credentials.
        let prepared = super::compose::prepare_send_transport(&state, &account)
            .await
            .and_then(|(transport, sender)| {
                use super::compose::PreparedSendTransport;
                let remote = match transport {
                    PreparedSendTransport::Gmail(provider) => ConnectedProvider::Gmail(provider),
                    PreparedSendTransport::Outlook(provider) => {
                        ConnectedProvider::Outlook(provider)
                    }
                    PreparedSendTransport::Smtp(_) => {
                        return Err(PebbleError::UnsupportedProvider(
                            "Remote drafts require OAuth".into(),
                        ))
                    }
                };
                Ok((remote, sender))
            });
        save_prepared_oauth_draft(&state, &account_id, &draft, &provenance, prepared).await
    } else {
        save_draft_locally(
            &state,
            &account_id,
            &draft,
            provenance.local_id.as_deref(),
            provenance.remote_id.as_deref(),
        )
    }
}

async fn save_prepared_oauth_draft<R: RemoteDraftOperations>(
    state: &AppState,
    account_id: &str,
    draft: &DraftMessage,
    provenance: &DraftProvenance,
    prepared: Result<(R, EmailAddress), PebbleError>,
) -> Result<String, PebbleError> {
    match prepared {
        Ok((remote, sender)) => {
            let mut verified_draft = draft.clone();
            verified_draft.from = Some(sender);
            save_oauth_draft_with_fallback(state, account_id, &verified_draft, provenance, &remote)
                .await
        }
        Err(error) => {
            warn!("Draft identity verification failed; preserving local draft: {error}");
            save_draft_locally(
                state,
                account_id,
                draft,
                provenance.local_id.as_deref(),
                provenance.remote_id.as_deref(),
            )
        }
    }
}

async fn save_oauth_draft_with_fallback<R: RemoteDraftOperations>(
    state: &AppState,
    account_id: &str,
    draft: &DraftMessage,
    provenance: &DraftProvenance,
    remote: &R,
) -> Result<String, PebbleError> {
    let remote_result = if let Some(remote_id) = provenance.remote_id.as_deref() {
        remote
            .update_draft(remote_id, draft)
            .await
            .map(|()| remote_id.to_string())
    } else {
        remote.create_draft(draft).await
    };

    match remote_result {
        Ok(remote_id) => {
            if let Some(local_id) = provenance.local_id.as_deref() {
                if let Err(error) = delete_local_draft(state, local_id) {
                    warn!(
                        "Remote draft {remote_id} was saved, but local fallback {local_id} could not be deleted: {error}"
                    );
                }
            }
            Ok(remote_id)
        }
        Err(error) => {
            warn!("Remote draft save failed; preserving encrypted local fallback: {error}");
            save_draft_locally(
                state,
                account_id,
                draft,
                provenance.local_id.as_deref(),
                provenance.remote_id.as_deref(),
            )
        }
    }
}

fn save_draft_locally(
    state: &AppState,
    account_id: &str,
    draft: &DraftMessage,
    existing_local_id: Option<&str>,
    remote_draft_id: Option<&str>,
) -> std::result::Result<String, PebbleError> {
    let id = existing_local_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(pebble_core::new_id);
    let previous_local_paths: Vec<String> = state
        .store
        .list_attachments_by_message(&id)?
        .into_iter()
        .filter_map(|attachment| attachment.local_path)
        .collect();
    let drafts_folder = crate::patch::drafts::ensure_local_drafts_folder(&state.store, account_id)?;
    let attachment_records =
        stage_local_attachment_records(&state.attachments_dir, &id, &draft.attachment_paths)?;

    let msg = pebble_core::Message {
        id: id.clone(),
        account_id: account_id.to_string(),
        remote_id: remote_draft_id.unwrap_or_default().to_string(),
        message_id_header: None,
        in_reply_to: draft.in_reply_to.clone(),
        references_header: None,
        thread_id: None,
        subject: draft.subject.clone(),
        snippet: draft.body_text.chars().take(200).collect(),
        from_address: draft
            .from
            .as_ref()
            .map(|from| from.address.clone())
            .unwrap_or_default(),
        from_name: draft
            .from
            .as_ref()
            .and_then(|from| from.name.clone())
            .unwrap_or_default(),
        to_list: draft.to.clone(),
        cc_list: draft.cc.clone(),
        bcc_list: draft.bcc.clone(),
        body_text: draft.body_text.clone(),
        body_html_raw: draft.body_html.clone().unwrap_or_default(),
        has_attachments: !attachment_records.is_empty(),
        is_read: true,
        is_starred: false,
        is_draft: true,
        date: pebble_core::now_timestamp(),
        remote_version: None,
        is_deleted: false,
        deleted_at: None,
        created_at: pebble_core::now_timestamp(),
        updated_at: pebble_core::now_timestamp(),
    };
    let folder_ids = vec![drafts_folder.id];
    if let Err(error) =
        state
            .store
            .replace_message_with_attachments(&msg, &folder_ids, &attachment_records)
    {
        cleanup_local_attachment_records(&attachment_records);
        return Err(error);
    }
    cleanup_unreferenced_local_attachment_paths(state, &previous_local_paths);
    Ok(id)
}

pub async fn delete_draft(
    state: AppStateRef,
    account_id: String,
    draft_id: String,
) -> std::result::Result<(), PebbleError> {
    let provenance = resolve_draft_provenance(state.store.as_ref(), &account_id, Some(&draft_id))?;
    let provider_type = state.store.get_account(&account_id)?.map(|a| a.provider);

    if requires_remote_draft_delete(provider_type.clone()) {
        if let Some(remote_id) = provenance.remote_id.as_deref() {
            let provider = provider_type.as_ref().ok_or_else(|| {
                PebbleError::Internal(
                    "OAuth draft deletion requires a persisted account provider".to_string(),
                )
            })?;
            let conn = ConnectedProvider::connect(&state, &account_id, provider)
                .await
                .map_err(|error| {
                    PebbleError::Network(format!(
                        "Could not connect to delete remote draft; local fallback was retained: {error}"
                    ))
                })?;
            let delete_result = conn.delete_draft(remote_id).await;
            conn.disconnect().await;
            delete_result?;
        }
    }

    if let Some(local_id) = provenance.local_id.as_deref() {
        delete_local_draft(&state, local_id)?;
    }
    Ok(())
}

pub async fn dispatch_command(
    state: AppStateRef,
    command: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, crate::error::ApiError> {
    use crate::error::ApiError;
    match command {
        "save_draft" => {
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
                #[serde(default)]
                existing_draft_id: Option<String>,
            }
            let args: Args = serde_json::from_value(args).map_err(|error| {
                ApiError::BadRequest(format!("invalid save_draft args: {error}"))
            })?;
            let id = save_draft(
                state,
                args.account_id,
                args.to,
                args.cc,
                args.bcc,
                args.subject,
                args.body_text,
                args.body_html,
                args.in_reply_to,
                args.attachment_paths,
                args.existing_draft_id,
            )
            .await
            .map_err(ApiError::from_pebble)?;
            Ok(serde_json::json!(id))
        }
        "delete_draft" => {
            #[derive(serde::Deserialize)]
            struct Args {
                account_id: String,
                draft_id: String,
            }
            let args: Args = serde_json::from_value(args).map_err(|error| {
                ApiError::BadRequest(format!("invalid delete_draft args: {error}"))
            })?;
            delete_draft(state, args.account_id, args.draft_id)
                .await
                .map_err(ApiError::from_pebble)?;
            Ok(serde_json::Value::Null)
        }
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}
