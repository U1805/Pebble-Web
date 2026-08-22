use pebble_core::{new_id, now_timestamp, EmailAddress, FolderRole, Message, PebbleError};
use serde::Deserialize;
use serde_json::Value;

use crate::command::attachments::{
    cleanup_local_attachment_records, stage_local_attachment_records,
    validate_staged_attachment_paths,
};
use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

#[derive(Deserialize)]
struct SaveDraftArgs {
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
    #[serde(default)]
    existing_draft_id: Option<String>,
}

fn parse_recipients(addresses: &[String]) -> Vec<EmailAddress> {
    addresses
        .iter()
        .cloned()
        .map(|address| EmailAddress {
            name: None,
            address,
        })
        .collect()
}

fn resolve_draft_provenance(
    store: &pebble_store::Store,
    account_id: &str,
    existing_draft_id: Option<&str>,
) -> Result<(Option<String>, Option<String>), PebbleError> {
    let Some(draft_id) = existing_draft_id else {
        return Ok((None, None));
    };
    let Some(existing) = store.get_message(draft_id)? else {
        return Ok((None, Some(draft_id.to_string())));
    };
    if existing.account_id != account_id || !existing.is_draft {
        return Err(PebbleError::Validation(
            "Existing draft does not belong to the selected account".to_string(),
        ));
    }
    Ok((
        Some(existing.id),
        (!existing.remote_id.is_empty()).then_some(existing.remote_id),
    ))
}

fn save_draft_locally(state: &AppStateRef, args: &SaveDraftArgs) -> Result<String, PebbleError> {
    let account = state
        .store
        .get_account(&args.account_id)?
        .ok_or_else(|| PebbleError::Internal(format!("Account not found: {}", args.account_id)))?;
    let (local_id, remote_id) = resolve_draft_provenance(
        &state.store,
        &args.account_id,
        args.existing_draft_id.as_deref(),
    )?;
    let id = local_id.clone().unwrap_or_else(new_id);
    let previous_local_paths: Vec<String> = local_id
        .as_deref()
        .map(|draft_id| {
            state
                .store
                .list_attachments_by_message(draft_id)
                .map(|attachments| {
                    attachments
                        .into_iter()
                        .filter_map(|a| a.local_path)
                        .collect()
                })
        })
        .transpose()?
        .unwrap_or_default();

    let raw_paths = args.attachment_paths.clone().unwrap_or_default();
    let attachment_paths = validate_staged_attachment_paths(&state.attachments_dir, &raw_paths)?;
    let attachment_records =
        match stage_local_attachment_records(&state.attachments_dir, &id, &attachment_paths) {
            Ok(records) => records,
            Err(error) => return Err(error),
        };
    let now = now_timestamp();
    let message = Message {
        id: id.clone(),
        account_id: account.id.clone(),
        remote_id: remote_id.unwrap_or_default(),
        message_id_header: None,
        in_reply_to: args.in_reply_to.clone(),
        references_header: None,
        thread_id: None,
        subject: args.subject.clone(),
        snippet: args.body_text.chars().take(200).collect(),
        from_address: account.email,
        from_name: account.display_name,
        to_list: parse_recipients(&args.to),
        cc_list: parse_recipients(&args.cc),
        bcc_list: parse_recipients(&args.bcc),
        body_text: args.body_text.clone(),
        body_html_raw: args.body_html.clone().unwrap_or_default(),
        has_attachments: !attachment_records.is_empty(),
        is_read: true,
        is_starred: false,
        is_draft: true,
        date: now,
        remote_version: None,
        is_deleted: false,
        deleted_at: None,
        created_at: now,
        updated_at: now,
    };
    let folder_ids = state
        .store
        .find_folder_by_role(&account.id, FolderRole::Drafts)?
        .map(|folder| vec![folder.id])
        .unwrap_or_default();
    if let Err(error) =
        state
            .store
            .replace_message_with_attachments(&message, &folder_ids, &attachment_records)
    {
        cleanup_local_attachment_records(&attachment_records);
        return Err(error);
    }
    for path in previous_local_paths {
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(id)
}

fn delete_draft_locally(
    state: &AppStateRef,
    account_id: &str,
    draft_id: &str,
) -> Result<(), PebbleError> {
    let Some(existing) = state.store.get_message(draft_id)? else {
        return Ok(());
    };
    if existing.account_id != account_id || !existing.is_draft {
        return Err(PebbleError::Validation(
            "Draft does not belong to the selected account".to_string(),
        ));
    }
    let local_paths: Vec<String> = state
        .store
        .list_attachments_by_message(draft_id)?
        .into_iter()
        .filter_map(|attachment| attachment.local_path)
        .collect();
    state.store.hard_delete_messages(&[draft_id.to_string()])?;
    for path in local_paths {
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(())
}

pub async fn save_draft(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    let args: SaveDraftArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid save_draft args: {e}")))?;
    let id = run_blocking(move || save_draft_locally(&state, &args)).await?;
    Ok(Value::String(id))
}

pub async fn delete_draft(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
        draft_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid delete_draft args: {e}")))?;
    run_blocking(move || delete_draft_locally(&state, &args.account_id, &args.draft_id)).await?;
    Ok(Value::Null)
}
