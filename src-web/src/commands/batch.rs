use super::messages::provider_dispatch::{parse_imap_uid, ConnectedProvider};
use super::messages::{find_folder_by_role, find_message_folder, refresh_search_documents};
use crate::state::{AppState, AppStateRef};
use pebble_core::traits::{FolderProvider, LabelProvider};
use pebble_core::{FolderRole, Message, PebbleError, ProviderType};
use pebble_store::Store;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

/// Group messages by account_id and resolve their provider type.
/// Uses a batch query to avoid N+1 individual lookups.
fn group_by_account(
    store: &Store,
    message_ids: &[String],
) -> std::result::Result<HashMap<String, (ProviderType, Vec<Message>)>, PebbleError> {
    let messages = store.get_messages_batch(message_ids)?;
    let mut groups: HashMap<String, (ProviderType, Vec<Message>)> = HashMap::new();
    for msg in messages {
        let provider = store
            .get_account(&msg.account_id)?
            .map(|a| a.provider)
            .unwrap_or(ProviderType::Imap);
        groups
            .entry(msg.account_id.clone())
            .or_insert_with(|| (provider, Vec::new()))
            .1
            .push(msg);
    }
    Ok(groups)
}

fn queue_batch_pending_op(
    state: &AppState,
    message: &Message,
    op_type: &str,
    payload: serde_json::Value,
    error: &str,
) -> std::result::Result<(), PebbleError> {
    let payload = json!({
        "provider_account_id": message.account_id,
        "remote_id": message.remote_id,
        "op": op_type,
        "payload": payload,
    });
    let op_id = state.store.insert_pending_mail_op(
        &message.account_id,
        &message.id,
        op_type,
        &payload.to_string(),
    )?;
    state.store.mark_pending_mail_op_failed(&op_id, error)?;
    Ok(())
}

fn batch_local_commit_ids(
    requested_ids: &[String],
    remote_succeeded_ids: &[String],
    queued_for_local_commit_ids: &[String],
) -> Vec<String> {
    let mut commit_ids: HashSet<&str> = remote_succeeded_ids.iter().map(String::as_str).collect();
    commit_ids.extend(queued_for_local_commit_ids.iter().map(String::as_str));
    requested_ids
        .iter()
        .filter(|id| commit_ids.contains(id.as_str()))
        .cloned()
        .collect()
}

fn record_remote_success_after_remote_id_update(
    message_id: &str,
    update_result: std::result::Result<(), PebbleError>,
    remote_succeeded_ids: &mut Vec<String>,
) -> std::result::Result<(), PebbleError> {
    update_result?;
    remote_succeeded_ids.push(message_id.to_string());
    Ok(())
}

/// Shared preamble for every batch_* command: enforce the 1000-id cap, then
/// resolve the provider groupings off the Tokio runtime. Returns `Ok(None)`
/// for an empty input so the caller can short-circuit with a zero count.
async fn prepare_batch(
    store: std::sync::Arc<Store>,
    message_ids: &[String],
) -> std::result::Result<Option<HashMap<String, (ProviderType, Vec<Message>)>>, PebbleError> {
    if message_ids.is_empty() {
        return Ok(None);
    }
    if message_ids.len() > 1000 {
        return Err(PebbleError::Internal(
            "Batch size exceeds limit of 1000".into(),
        ));
    }
    let ids = message_ids.to_vec();
    let groups = tokio::task::spawn_blocking(move || group_by_account(&store, &ids))
        .await
        .map_err(|e| PebbleError::Internal(format!("Task join error: {e}")))??;
    Ok(Some(groups))
}

pub async fn batch_archive(
    state: AppStateRef,
    message_ids: Vec<String>,
) -> std::result::Result<u32, PebbleError> {
    let Some(groups) = prepare_batch(state.store.clone(), &message_ids).await? else {
        return Ok(0);
    };
    let mut success_count: u32 = 0;
    let mut archived_ids = Vec::new();

    // Validate every account before applying any mutations so a mixed-account
    // batch cannot be partially archived before an unsafe local IMAP target is
    // discovered.
    for (account_id, (provider_type, _)) in &groups {
        if let Ok(archive) = find_folder_by_role(&state, account_id, FolderRole::Archive) {
            crate::patch::archive::reject_unsafe_imap_local_archive(provider_type, &archive)?;
        }
    }

    for (account_id, (provider_type, messages)) in &groups {
        let archive_folder = find_folder_by_role(&state, account_id, FolderRole::Archive).ok();

        // Remote sync: connect once per account, operate, disconnect.
        // For Outlook/IMAP we need a usable (non-local) archive folder on the
        // server; skip the connection entirely when there isn't one.
        let has_remote_archive = archive_folder
            .as_ref()
            .is_some_and(|af| !af.remote_id.starts_with("__local_"));
        let needs_connection = !matches!(provider_type, ProviderType::Pop3)
            && (matches!(provider_type, ProviderType::Gmail) || has_remote_archive);

        // Track which messages succeeded remotely for this account group
        let mut remote_succeeded: Vec<String> = Vec::new();
        let mut queued_for_local_commit: Vec<String> = Vec::new();

        if needs_connection {
            match ConnectedProvider::connect(&state, account_id, provider_type).await {
                Ok(conn) => {
                    match &conn {
                        ConnectedProvider::Gmail(provider) => {
                            for msg in messages {
                                match provider
                                    .modify_labels(&msg.remote_id, &[], &["INBOX".to_string()])
                                    .await
                                {
                                    Ok(_) => remote_succeeded.push(msg.id.clone()),
                                    Err(e) => {
                                        warn!("Gmail batch archive failed for {}: {e}", msg.id);
                                        queue_batch_pending_op(
                                            &state,
                                            msg,
                                            "archive",
                                            json!({
                                                "add_labels": [],
                                                "remove_labels": ["INBOX"],
                                                "archive_folder_id": archive_folder.as_ref().map(|f| f.id.as_str()),
                                                "archive_folder_remote_id": archive_folder.as_ref().map(|f| f.remote_id.as_str()),
                                            }),
                                            &e.to_string(),
                                        )?;
                                    }
                                }
                            }
                        }
                        ConnectedProvider::Outlook(provider) => {
                            let Some(af) = archive_folder.as_ref() else {
                                continue;
                            };
                            for msg in messages {
                                match provider.move_message(&msg.remote_id, &af.remote_id).await {
                                    Ok(new_remote_id) => {
                                        if let Err(e) = record_remote_success_after_remote_id_update(
                                            &msg.id,
                                            state.store.update_remote_id(&msg.id, &new_remote_id),
                                            &mut remote_succeeded,
                                        ) {
                                            warn!(
                                                "Outlook batch archive applied remotely but failed to store new remote_id for {}: {e}",
                                                msg.id
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Outlook batch archive failed for {}: {e}", msg.id);
                                        queue_batch_pending_op(
                                            &state,
                                            msg,
                                            "archive",
                                            json!({
                                                "archive_folder_id": af.id,
                                                "archive_folder_remote_id": af.remote_id,
                                            }),
                                            &e.to_string(),
                                        )?;
                                    }
                                }
                            }
                        }
                        ConnectedProvider::Imap(imap) => {
                            let Some(af) = archive_folder.as_ref() else {
                                continue;
                            };
                            for msg in messages {
                                if let Ok(_uid) = parse_imap_uid(&msg.remote_id) {
                                    if let Ok(src) =
                                        find_message_folder(&state, &msg.id, account_id)
                                    {
                                        match crate::patch::imap_move::move_message_and_update_uid(
                                            imap,
                                            &state.store,
                                            msg,
                                            &src.remote_id,
                                            &af.remote_id,
                                        )
                                        .await
                                        {
                                            Ok(_) => remote_succeeded.push(msg.id.clone()),
                                            Err(e) => {
                                                warn!(
                                                    "IMAP batch archive failed for {}: {e}",
                                                    msg.id
                                                );
                                                queue_batch_pending_op(
                                                    &state,
                                                    msg,
                                                    "archive",
                                                    json!({
                                                        "source_folder_id": src.id,
                                                        "source_folder_remote_id": src.remote_id,
                                                        "archive_folder_id": af.id,
                                                        "archive_folder_remote_id": af.remote_id,
                                                    }),
                                                    &e.to_string(),
                                                )?;
                                            }
                                        }
                                    } else {
                                        queue_batch_pending_op(
                                            &state,
                                            msg,
                                            "archive",
                                            json!({
                                                "archive_folder_id": af.id,
                                                "archive_folder_remote_id": af.remote_id,
                                            }),
                                            "Source folder lookup failed",
                                        )?;
                                    }
                                } else {
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "archive",
                                        json!({
                                            "archive_folder_id": af.id,
                                            "archive_folder_remote_id": af.remote_id,
                                        }),
                                        "Invalid IMAP UID",
                                    )?;
                                }
                            }
                        }
                    }
                    conn.disconnect().await;
                }
                Err(e) => {
                    let error = e.to_string();
                    for msg in messages {
                        queue_batch_pending_op(
                            &state,
                            msg,
                            "archive",
                            json!({
                                "archive_folder_id": archive_folder.as_ref().map(|f| f.id.as_str()),
                                "archive_folder_remote_id": archive_folder.as_ref().map(|f| f.remote_id.as_str()),
                            }),
                            &error,
                        )?;
                        queued_for_local_commit.push(msg.id.clone());
                    }
                }
            }
        } else {
            // No remote target needed; all messages succeed locally
            for msg in messages {
                remote_succeeded.push(msg.id.clone());
            }
        }

        // Local store update: only for messages that succeeded remotely.
        // Build a lookup map so we can find each message's archive_folder logic
        let msg_map: HashMap<&str, &Message> =
            messages.iter().map(|m| (m.id.as_str(), m)).collect();
        let requested_for_account = messages
            .iter()
            .map(|msg| msg.id.clone())
            .collect::<Vec<_>>();
        let local_commit_ids = batch_local_commit_ids(
            &requested_for_account,
            &remote_succeeded,
            &queued_for_local_commit,
        );
        for id in &local_commit_ids {
            if let Some(msg) = msg_map.get(id.as_str()) {
                let result = match &archive_folder {
                    Some(af) => state.store.move_message_to_folder(&msg.id, &af.id),
                    None => state.store.soft_delete_message(&msg.id),
                };
                match result {
                    Ok(()) => {
                        success_count += 1;
                        archived_ids.push(msg.id.clone());
                    }
                    Err(e) => warn!("Failed to archive message {}: {e}", msg.id),
                }
            }
        }
    }

    // Update search index for archived messages in a single commit
    if let Err(e) = refresh_search_documents(&state, &archived_ids) {
        warn!("Failed to refresh search documents after batch archive: {e}");
    }

    info!(
        "Batch archive: {}/{} messages archived",
        success_count,
        message_ids.len()
    );
    Ok(success_count)
}

pub async fn batch_delete(
    state: AppStateRef,
    message_ids: Vec<String>,
) -> std::result::Result<u32, PebbleError> {
    let Some(groups) = prepare_batch(state.store.clone(), &message_ids).await? else {
        return Ok(0);
    };
    let messages: Vec<Message> = groups
        .values()
        .flat_map(|(_, messages)| messages.iter().cloned())
        .collect();
    let delete_plan =
        crate::patch::batch_delete::BatchDeletePlan::capture(&state.store, &messages)?;

    // Track which messages were successfully deleted remotely
    let mut deleted_ids: Vec<String> = Vec::new();
    let mut queued_for_local_commit_ids: Vec<String> = Vec::new();
    // Remote sync: connect once per account, operate, disconnect.
    for (account_id, (provider_type, messages)) in &groups {
        if matches!(provider_type, ProviderType::Pop3) {
            deleted_ids.extend(messages.iter().map(|msg| msg.id.clone()));
            continue;
        }

        match ConnectedProvider::connect(&state, account_id, provider_type).await {
            Ok(conn) => {
                match &conn {
                    ConnectedProvider::Gmail(provider) => {
                        for msg in messages {
                            let result = if delete_plan.is_permanent(&msg.id) {
                                provider.delete_message_permanently(&msg.remote_id).await
                            } else {
                                provider.trash_message(&msg.remote_id).await
                            };
                            match result {
                                Ok(_) => deleted_ids.push(msg.id.clone()),
                                Err(e) => {
                                    warn!("Gmail batch delete failed for {}: {e}", msg.id);
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        delete_plan.pending_op_type(&msg.id),
                                        delete_plan.pending_payload(&msg.id),
                                        &e.to_string(),
                                    )?;
                                }
                            }
                        }
                    }
                    ConnectedProvider::Outlook(provider) => {
                        for msg in messages {
                            if delete_plan.is_permanent(&msg.id) {
                                match provider.delete_message_permanently(&msg.remote_id).await {
                                    Ok(()) => deleted_ids.push(msg.id.clone()),
                                    Err(e) => {
                                        warn!(
                                            "Outlook batch permanent delete failed for {}: {e}",
                                            msg.id
                                        );
                                        queue_batch_pending_op(
                                            &state,
                                            msg,
                                            delete_plan.pending_op_type(&msg.id),
                                            delete_plan.pending_payload(&msg.id),
                                            &e.to_string(),
                                        )?;
                                    }
                                }
                            } else {
                                match provider.trash_message(&msg.remote_id).await {
                                    Ok(new_remote_id) => {
                                        if let Err(e) = record_remote_success_after_remote_id_update(
                                            &msg.id,
                                            state.store.update_remote_id(&msg.id, &new_remote_id),
                                            &mut deleted_ids,
                                        ) {
                                            warn!(
                                                "Outlook batch delete applied remotely but failed to store new remote_id for {}: {e}",
                                                msg.id
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Outlook batch delete failed for {}: {e}", msg.id);
                                        queue_batch_pending_op(
                                            &state,
                                            msg,
                                            delete_plan.pending_op_type(&msg.id),
                                            delete_plan.pending_payload(&msg.id),
                                            &e.to_string(),
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                    ConnectedProvider::Imap(imap) => {
                        if let Ok(trash_folder) =
                            find_folder_by_role(&state, account_id, FolderRole::Trash)
                        {
                            for msg in messages {
                                if let Ok(uid) = parse_imap_uid(&msg.remote_id) {
                                    if let Ok(src) =
                                        find_message_folder(&state, &msg.id, account_id)
                                    {
                                        if src.id != trash_folder.id {
                                            match crate::patch::imap_move::move_message_and_update_uid(
                                                imap,
                                                &state.store,
                                                msg,
                                                &src.remote_id,
                                                &trash_folder.remote_id,
                                            )
                                            .await
                                            {
                                                Ok(_) => deleted_ids.push(msg.id.clone()),
                                                Err(e) => {
                                                    warn!(
                                                        "IMAP batch delete failed for {}: {e}",
                                                        msg.id
                                                    );
                                                    queue_batch_pending_op(
                                                        &state,
                                                        msg,
                                                        "delete",
                                                        json!({
                                                            "source_folder_id": src.id,
                                                            "source_folder_remote_id": src.remote_id,
                                                            "trash_folder_id": trash_folder.id,
                                                            "trash_folder_remote_id": trash_folder.remote_id,
                                                        }),
                                                        &e.to_string(),
                                                    )?;
                                                }
                                            }
                                        } else {
                                            match imap.delete_message(&src.remote_id, uid).await {
                                                Ok(_) => deleted_ids.push(msg.id.clone()),
                                                Err(e) => {
                                                    warn!("IMAP batch permanent delete failed for {}: {e}", msg.id);
                                                    queue_batch_pending_op(
                                                        &state,
                                                        msg,
                                                        "delete_permanent",
                                                        json!({
                                                            "source_folder_id": src.id,
                                                            "source_folder_remote_id": src.remote_id,
                                                            "permanent": true,
                                                        }),
                                                        &e.to_string(),
                                                    )?;
                                                }
                                            }
                                        }
                                    } else {
                                        queue_batch_pending_op(
                                            &state,
                                            msg,
                                            "delete",
                                            json!({ "trash_folder_id": trash_folder.id }),
                                            "Source folder lookup failed",
                                        )?;
                                    }
                                } else {
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "delete",
                                        json!({ "trash_folder_id": trash_folder.id }),
                                        "Invalid IMAP UID",
                                    )?;
                                }
                            }
                        }
                    }
                }
                conn.disconnect().await;
            }
            Err(e) => {
                let error = e.to_string();
                for msg in messages {
                    queue_batch_pending_op(
                        &state,
                        msg,
                        delete_plan.pending_op_type(&msg.id),
                        delete_plan.pending_payload(&msg.id),
                        &error,
                    )?;
                    if !delete_plan.is_permanent(&msg.id) {
                        queued_for_local_commit_ids.push(msg.id.clone());
                    }
                }
            }
        }
    }

    // Apply locally after remote success or after a provider connection failure
    // that was queued for retry. Per-message remote failures after a successful
    // connection remain remote-only failures.
    let ids_to_delete =
        batch_local_commit_ids(&message_ids, &deleted_ids, &queued_for_local_commit_ids);

    let finalized = delete_plan.finalize(&state.store, &ids_to_delete)?;
    let success_count = ids_to_delete.len() as u32;

    refresh_search_documents(&state, &finalized.visible_ids)?;

    // Update search index only for messages that are no longer locally visible.
    let delete_ids = finalized.removed_ids;
    let _ = state.store.add_search_pending(&delete_ids, "remove");
    for id in &delete_ids {
        if let Err(e) = state.search.remove_message(id) {
            warn!("Failed to remove deleted message {id} from search index: {e}");
        }
    }
    if let Err(e) = state.search.commit() {
        warn!("Failed to commit search index after batch delete: {e}");
    }
    let _ = state.store.clear_search_pending(&delete_ids);

    info!(
        "Batch delete: {}/{} messages deleted",
        success_count,
        message_ids.len()
    );
    Ok(success_count)
}

pub async fn batch_mark_read(
    state: AppStateRef,
    message_ids: Vec<String>,
    is_read: bool,
) -> std::result::Result<u32, PebbleError> {
    let Some(groups) = prepare_batch(state.store.clone(), &message_ids).await? else {
        return Ok(0);
    };

    // Track which messages were successfully updated remotely
    let mut synced_ids: Vec<String> = Vec::new();
    let mut queued_for_local_commit_ids: Vec<String> = Vec::new();
    // Remote sync: connect once per account, operate, disconnect.
    for (account_id, (provider_type, messages)) in &groups {
        if matches!(provider_type, ProviderType::Pop3) {
            synced_ids.extend(messages.iter().map(|msg| msg.id.clone()));
            continue;
        }

        match ConnectedProvider::connect(&state, account_id, provider_type).await {
            Ok(conn) => {
                match &conn {
                    ConnectedProvider::Gmail(provider) => {
                        let (add, remove) = if is_read {
                            (vec![], vec!["UNREAD".to_string()])
                        } else {
                            (vec!["UNREAD".to_string()], vec![])
                        };
                        for msg in messages {
                            match provider.modify_labels(&msg.remote_id, &add, &remove).await {
                                Ok(_) => synced_ids.push(msg.id.clone()),
                                Err(e) => {
                                    warn!("Gmail batch mark_read failed for {}: {e}", msg.id);
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "update_flags",
                                        json!({ "is_read": is_read, "is_starred": null }),
                                        &e.to_string(),
                                    )?;
                                }
                            }
                        }
                    }
                    ConnectedProvider::Outlook(provider) => {
                        for msg in messages {
                            match provider.update_read_status(&msg.remote_id, is_read).await {
                                Ok(_) => synced_ids.push(msg.id.clone()),
                                Err(e) => {
                                    warn!("Outlook batch mark_read failed for {}: {e}", msg.id);
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "update_flags",
                                        json!({ "is_read": is_read, "is_starred": null }),
                                        &e.to_string(),
                                    )?;
                                }
                            }
                        }
                    }
                    ConnectedProvider::Imap(imap) => {
                        for msg in messages {
                            if let Ok(uid) = parse_imap_uid(&msg.remote_id) {
                                if let Ok(folder) = find_message_folder(&state, &msg.id, account_id)
                                {
                                    match imap
                                        .set_flags(&folder.remote_id, uid, Some(is_read), None)
                                        .await
                                    {
                                        Ok(_) => synced_ids.push(msg.id.clone()),
                                        Err(e) => {
                                            warn!(
                                                "IMAP batch mark_read failed for {}: {e}",
                                                msg.id
                                            );
                                            queue_batch_pending_op(
                                                &state,
                                                msg,
                                                "update_flags",
                                                json!({
                                                    "folder_remote_id": folder.remote_id,
                                                    "is_read": is_read,
                                                    "is_starred": null,
                                                }),
                                                &e.to_string(),
                                            )?;
                                        }
                                    }
                                } else {
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "update_flags",
                                        json!({ "is_read": is_read, "is_starred": null }),
                                        "Source folder lookup failed",
                                    )?;
                                }
                            } else {
                                queue_batch_pending_op(
                                    &state,
                                    msg,
                                    "update_flags",
                                    json!({ "is_read": is_read, "is_starred": null }),
                                    "Invalid IMAP UID",
                                )?;
                            }
                        }
                    }
                }
                conn.disconnect().await;
            }
            Err(e) => {
                let error = e.to_string();
                for msg in messages {
                    queue_batch_pending_op(
                        &state,
                        msg,
                        "update_flags",
                        json!({ "is_read": is_read, "is_starred": null }),
                        &error,
                    )?;
                    queued_for_local_commit_ids.push(msg.id.clone());
                }
            }
        }
    }

    let ids_to_update =
        batch_local_commit_ids(&message_ids, &synced_ids, &queued_for_local_commit_ids);

    // Local bulk flag update: only for messages that succeeded remotely (or all if offline).
    let changes: Vec<(String, Option<bool>, Option<bool>)> = ids_to_update
        .iter()
        .map(|id| (id.clone(), Some(is_read), None))
        .collect();
    state.store.bulk_update_flags(&changes)?;
    let success_count = ids_to_update.len() as u32;

    info!(
        "Batch mark_read({}): {}/{} messages updated",
        is_read,
        success_count,
        message_ids.len()
    );
    Ok(success_count)
}

pub async fn batch_star(
    state: AppStateRef,
    message_ids: Vec<String>,
    starred: bool,
) -> std::result::Result<u32, PebbleError> {
    let Some(groups) = prepare_batch(state.store.clone(), &message_ids).await? else {
        return Ok(0);
    };

    // Track which messages were successfully updated remotely
    let mut synced_ids: Vec<String> = Vec::new();
    let mut queued_for_local_commit_ids: Vec<String> = Vec::new();
    // Remote sync: connect once per account, operate, disconnect.
    for (account_id, (provider_type, messages)) in &groups {
        if matches!(provider_type, ProviderType::Pop3) {
            synced_ids.extend(messages.iter().map(|msg| msg.id.clone()));
            continue;
        }

        match ConnectedProvider::connect(&state, account_id, provider_type).await {
            Ok(conn) => {
                match &conn {
                    ConnectedProvider::Gmail(provider) => {
                        let (add, remove) = if starred {
                            (vec!["STARRED".to_string()], vec![])
                        } else {
                            (vec![], vec!["STARRED".to_string()])
                        };
                        for msg in messages {
                            match provider.modify_labels(&msg.remote_id, &add, &remove).await {
                                Ok(_) => synced_ids.push(msg.id.clone()),
                                Err(e) => {
                                    warn!("Gmail batch star failed for {}: {e}", msg.id);
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "update_flags",
                                        json!({ "is_read": null, "is_starred": starred }),
                                        &e.to_string(),
                                    )?;
                                }
                            }
                        }
                    }
                    ConnectedProvider::Outlook(provider) => {
                        for msg in messages {
                            match provider.update_flag_status(&msg.remote_id, starred).await {
                                Ok(_) => synced_ids.push(msg.id.clone()),
                                Err(e) => {
                                    warn!("Outlook batch star failed for {}: {e}", msg.id);
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "update_flags",
                                        json!({ "is_read": null, "is_starred": starred }),
                                        &e.to_string(),
                                    )?;
                                }
                            }
                        }
                    }
                    ConnectedProvider::Imap(imap) => {
                        for msg in messages {
                            if let Ok(uid) = parse_imap_uid(&msg.remote_id) {
                                if let Ok(folder) = find_message_folder(&state, &msg.id, account_id)
                                {
                                    match imap
                                        .set_flags(&folder.remote_id, uid, None, Some(starred))
                                        .await
                                    {
                                        Ok(_) => synced_ids.push(msg.id.clone()),
                                        Err(e) => {
                                            warn!("IMAP batch star failed for {}: {e}", msg.id);
                                            queue_batch_pending_op(
                                                &state,
                                                msg,
                                                "update_flags",
                                                json!({
                                                    "folder_remote_id": folder.remote_id,
                                                    "is_read": null,
                                                    "is_starred": starred,
                                                }),
                                                &e.to_string(),
                                            )?;
                                        }
                                    }
                                } else {
                                    queue_batch_pending_op(
                                        &state,
                                        msg,
                                        "update_flags",
                                        json!({ "is_read": null, "is_starred": starred }),
                                        "Source folder lookup failed",
                                    )?;
                                }
                            } else {
                                queue_batch_pending_op(
                                    &state,
                                    msg,
                                    "update_flags",
                                    json!({ "is_read": null, "is_starred": starred }),
                                    "Invalid IMAP UID",
                                )?;
                            }
                        }
                    }
                }
                conn.disconnect().await;
            }
            Err(e) => {
                let error = e.to_string();
                for msg in messages {
                    queue_batch_pending_op(
                        &state,
                        msg,
                        "update_flags",
                        json!({ "is_read": null, "is_starred": starred }),
                        &error,
                    )?;
                    queued_for_local_commit_ids.push(msg.id.clone());
                }
            }
        }
    }

    let ids_to_update =
        batch_local_commit_ids(&message_ids, &synced_ids, &queued_for_local_commit_ids);

    // Local bulk flag update: only for messages that succeeded remotely (or all if offline).
    let changes: Vec<(String, Option<bool>, Option<bool>)> = ids_to_update
        .iter()
        .map(|id| (id.clone(), None, Some(starred)))
        .collect();
    state.store.bulk_update_flags(&changes)?;
    let success_count = ids_to_update.len() as u32;

    info!(
        "Batch star({}): {}/{} messages updated",
        starred,
        success_count,
        message_ids.len()
    );
    Ok(success_count)
}

pub async fn dispatch_command(
    state: AppStateRef,
    command: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, crate::error::ApiError> {
    use crate::error::ApiError;

    match command {
        "batch_archive" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
            }
            let args: Args = serde_json::from_value(args).map_err(|error| {
                ApiError::BadRequest(format!("invalid batch_archive args: {error}"))
            })?;
            let count = batch_archive(state, args.message_ids)
                .await
                .map_err(ApiError::from_pebble)?;
            Ok(serde_json::json!(count))
        }
        "batch_delete" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
            }
            let args: Args = serde_json::from_value(args).map_err(|error| {
                ApiError::BadRequest(format!("invalid batch_delete args: {error}"))
            })?;
            let count = batch_delete(state, args.message_ids)
                .await
                .map_err(ApiError::from_pebble)?;
            Ok(serde_json::json!(count))
        }
        "batch_mark_read" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
                is_read: bool,
            }
            let args: Args = serde_json::from_value(args).map_err(|error| {
                ApiError::BadRequest(format!("invalid batch_mark_read args: {error}"))
            })?;
            let count = batch_mark_read(state, args.message_ids, args.is_read)
                .await
                .map_err(ApiError::from_pebble)?;
            Ok(serde_json::json!(count))
        }
        "batch_star" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_ids: Vec<String>,
                starred: bool,
            }
            let args: Args = serde_json::from_value(args).map_err(|error| {
                ApiError::BadRequest(format!("invalid batch_star args: {error}"))
            })?;
            let count = batch_star(state, args.message_ids, args.starred)
                .await
                .map_err(ApiError::from_pebble)?;
            Ok(serde_json::json!(count))
        }
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}
