use serde::Deserialize;
use serde_json::Value;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn get_message_labels(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_message_labels args: {e}")))?;
    let store = state.store.clone();
    let labels = run_blocking(move || store.get_message_labels(&args.message_id)).await?;
    serde_json::to_value(labels).map_err(ApiError::from_serialize)
}

pub async fn get_message_labels_batch(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_ids: Vec<String>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_message_labels_batch args: {e}")))?;
    let store = state.store.clone();
    let labels = run_blocking(move || store.get_message_labels_batch(&args.message_ids)).await?;
    serde_json::to_value(labels).map_err(ApiError::from_serialize)
}

pub async fn add_message_label(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
        label_name: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid add_message_label args: {e}")))?;
    let store = state.store.clone();
    run_blocking(move || store.add_label(&args.message_id, &args.label_name)).await?;
    Ok(Value::Null)
}

pub async fn remove_message_label(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
        label_name: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid remove_message_label args: {e}")))?;
    let store = state.store.clone();
    run_blocking(move || store.remove_label(&args.message_id, &args.label_name)).await?;
    Ok(Value::Null)
}

pub async fn list_labels(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let labels = run_blocking(move || store.list_labels()).await?;
    serde_json::to_value(labels).map_err(ApiError::from_serialize)
}
