use serde_json::Value;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn list_messages(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        folder_id: String,
        #[serde(default)]
        folder_ids: Option<Vec<String>>,
        limit: u32,
        offset: u32,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_messages args: {e}")))?;
    let store = state.store.clone();
    let messages = run_blocking(move || match args.folder_ids {
        Some(ids) if !ids.is_empty() => {
            store.list_messages_by_folders(&ids, args.limit, args.offset)
        }
        _ => store.list_messages_by_folder(&args.folder_id, args.limit, args.offset),
    })
    .await?;
    serde_json::to_value(messages).map_err(ApiError::from_serialize)
}

pub async fn list_starred_messages(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        limit: u32,
        offset: u32,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_starred_messages args: {e}")))?;
    let store = state.store.clone();
    let messages = run_blocking(move || {
        store.list_starred_messages(&args.account_id, args.limit, args.offset)
    })
    .await?;
    serde_json::to_value(messages).map_err(ApiError::from_serialize)
}

pub async fn get_message(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_id: String,
    }
    let Args { message_id } = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_message args: {e}")))?;
    let store = state.store.clone();
    let message = run_blocking(move || store.get_message(&message_id)).await?;
    serde_json::to_value(message).map_err(ApiError::from_serialize)
}

pub async fn get_messages_batch(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_ids: Vec<String>,
    }
    let Args { message_ids } = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_messages_batch args: {e}")))?;
    let store = state.store.clone();
    let messages = run_blocking(move || store.get_messages_batch(&message_ids)).await?;
    serde_json::to_value(messages).map_err(ApiError::from_serialize)
}
