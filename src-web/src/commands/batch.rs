use pebble_core::FolderRole;
use serde_json::{json, Value};

use crate::commands::messages::{
    lifecycle::{emit_folder_and_queue_changed, find_folder_by_role, find_message_context, queue_pending},
    refresh_search_documents,
};
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn batch_archive(state: AppStateRef, message_ids: Vec<String>) -> Result<Value, ApiError> {
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

pub async fn batch_delete(state: AppStateRef, message_ids: Vec<String>) -> Result<Value, ApiError> {
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

pub async fn batch_mark_read(
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

pub async fn batch_star(
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

pub async fn dispatch_command(state: AppStateRef, command: &str, args: Value) -> Result<Value, ApiError> {
    match command {
        "batch_archive" => {
            #[derive(serde::Deserialize)] struct Args { message_ids: Vec<String> }
            let a: Args = serde_json::from_value(args).map_err(|e| ApiError::BadRequest(format!("invalid batch_archive args: {e}")))?;
            batch_archive(state, a.message_ids).await
        }
        "batch_delete" => {
            #[derive(serde::Deserialize)] struct Args { message_ids: Vec<String> }
            let a: Args = serde_json::from_value(args).map_err(|e| ApiError::BadRequest(format!("invalid batch_delete args: {e}")))?;
            batch_delete(state, a.message_ids).await
        }
        "batch_mark_read" => {
            #[derive(serde::Deserialize)] struct Args { message_ids: Vec<String>, is_read: bool }
            let a: Args = serde_json::from_value(args).map_err(|e| ApiError::BadRequest(format!("invalid batch_mark_read args: {e}")))?;
            batch_mark_read(state, a.message_ids, a.is_read).await
        }
        "batch_star" => {
            #[derive(serde::Deserialize)] struct Args { message_ids: Vec<String>, starred: bool }
            let a: Args = serde_json::from_value(args).map_err(|e| ApiError::BadRequest(format!("invalid batch_star args: {e}")))?;
            batch_star(state, a.message_ids, a.starred).await
        }
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}
