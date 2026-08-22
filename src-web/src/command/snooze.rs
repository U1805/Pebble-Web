use pebble_core::{now_timestamp, SnoozedMessage};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn snooze_message(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
        until: i64,
        return_to: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid snooze_message args: {e}")))?;
    let snooze = SnoozedMessage {
        message_id: args.message_id,
        snoozed_at: now_timestamp(),
        unsnoozed_at: args.until,
        return_to: args.return_to,
    };
    let store = state.store.clone();
    run_blocking(move || store.snooze_message(&snooze)).await?;
    Ok(Value::Null)
}

pub async fn unsnooze_message(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        message_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid unsnooze_message args: {e}")))?;
    let message_id = args.message_id;
    let store = state.store.clone();
    let return_to = run_blocking({
        let message_id = message_id.clone();
        move || Ok(store.get_snoozed_message(&message_id)?.map(|s| s.return_to))
    })
    .await?;
    let store = state.store.clone();
    let message_id_for_delete = message_id.clone();
    run_blocking(move || store.unsnooze_message(&message_id_for_delete)).await?;
    let _ = state.ws_broadcast.send(
        json!({
            "type": "mail:unsnoozed",
            "payload": { "message_id": message_id, "return_to": return_to },
        })
        .to_string(),
    );
    Ok(Value::Null)
}

pub async fn list_snoozed(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let snoozed = run_blocking(move || store.list_snoozed_messages()).await?;
    serde_json::to_value(snoozed).map_err(ApiError::from_serialize)
}
