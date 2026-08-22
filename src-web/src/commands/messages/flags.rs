use serde_json::{json, Value};

use crate::commands::messages::{refresh_search_documents, lifecycle::queue_pending};
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn update_message_flags(
    state: AppStateRef,
    message_id: String,
    is_read: Option<bool>,
    is_starred: Option<bool>,
) -> Result<Value, ApiError> {
    let exists = state
        .store
        .get_message(&message_id)
        .map_err(ApiError::from_store)?
        .is_some();
    if !exists {
        return Err(ApiError::NotFound(format!(
            "message not found: {message_id}"
        )));
    }
    state
        .store
        .update_message_flags(&message_id, is_read, is_starred)
        .map_err(ApiError::from_store)?;
    if let Some(msg) = state
        .store
        .get_message(&message_id)
        .map_err(ApiError::from_store)?
    {
        queue_pending(
            &state,
            &msg,
            "update_flags",
            json!({ "is_read": is_read, "is_starred": is_starred }),
        )
        .map_err(ApiError::from_pebble)?;
    }
    refresh_search_documents(&state, &[message_id.clone()]).map_err(ApiError::from_pebble)?;
    Ok(Value::Null)
}

pub async fn dispatch_command(state: AppStateRef, command: &str, args: Value) -> Result<Value, ApiError> {
    match command {
        "update_message_flags" => {
            #[derive(serde::Deserialize)]
            struct Args {
                message_id: String,
                #[serde(default)] is_read: Option<bool>,
                #[serde(default)] is_starred: Option<bool>,
            }
            let a: Args = serde_json::from_value(args)
                .map_err(|e| ApiError::BadRequest(format!("invalid update_message_flags args: {e}")))?;
            update_message_flags(state, a.message_id, a.is_read, a.is_starred).await
        }
        _ => Err(ApiError::NotFound(format!("unknown command: {command}"))),
    }
}
