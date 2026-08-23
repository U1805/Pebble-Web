use serde_json::{json, Value};

use crate::commands::indexing;
use crate::error::ApiError;
use crate::state::AppStateRef;

fn realtime_preference_interval(mode: &str) -> Result<u64, pebble_core::PebbleError> {
    match mode {
        "realtime" => Ok(3),
        "balanced" => Ok(15),
        "battery" => Ok(60),
        "manual" => Ok(0),
        other => Err(pebble_core::PebbleError::Validation(format!(
            "Invalid realtime preference: {other}"
        ))),
    }
}
/// Trigger an existing long-lived account worker. If no worker exists,
/// start a manual one-shot worker, matching the desktop command semantics.
pub async fn trigger_sync(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        reason: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid trigger_sync args: {e}")))?;
    state
        .sync_manager
        .trigger_account(&args.account_id, &args.reason)
        .await
        .map_err(ApiError::from_pebble)?;
    Ok(Value::Null)
}

/// Start one long-lived sync worker for the account.
pub async fn start_sync(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
        #[serde(default)]
        poll_interval_secs: Option<u64>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid start_sync args: {e}")))?;
    state
        .sync_manager
        .start_account(args.account_id.clone(), args.poll_interval_secs)
        .await
        .map_err(ApiError::from_pebble)?;
    Ok(json!(format!("Sync started for account {}", args.account_id)))
}

/// Stop and remove the account's long-lived sync worker.
pub async fn stop_sync(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        account_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid stop_sync args: {e}")))?;
    let _ = state.sync_manager.stop_account(&args.account_id).await;
    Ok(Value::Null)
}

/// Apply the same realtime preference intervals used by the desktop adapter.
pub async fn set_realtime_preference(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        mode: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid set_realtime_preference args: {e}")))?;
    let interval = realtime_preference_interval(&args.mode).map_err(ApiError::from_pebble)?;
    state
        .sync_manager
        .apply_realtime_preference(interval)
        .await
        .map_err(ApiError::from_pebble)?;
    Ok(Value::Null)
}

/// Rebuild the search index from all messages currently in the store.
pub async fn reindex_search(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let search = state.search.clone();
    let count = tokio::task::spawn_blocking(move || indexing::do_reindex(&store, &search))
        .await
        .map_err(|e| {
            ApiError::from_pebble(pebble_core::PebbleError::Internal(format!(
                "Reindex task failed: {e}"
            )))
        })?
        .map_err(ApiError::from_pebble)?;
    Ok(json!(count))
}
