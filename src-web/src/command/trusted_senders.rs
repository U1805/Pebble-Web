use pebble_core::{now_timestamp, TrustType, TrustedSender};
use serde::Deserialize;
use serde_json::Value;

use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn trust_sender(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
        email: String,
        trust_type: TrustType,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid trust_sender args: {e}")))?;
    let sender = TrustedSender {
        account_id: args.account_id,
        email: args.email,
        trust_type: args.trust_type,
        created_at: now_timestamp(),
    };
    let store = state.store.clone();
    run_blocking(move || store.trust_sender(&sender)).await?;
    Ok(Value::Null)
}

pub async fn list_trusted_senders(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid list_trusted_senders args: {e}")))?;
    let store = state.store.clone();
    let senders = run_blocking(move || store.list_trusted_senders(&args.account_id)).await?;
    serde_json::to_value(senders).map_err(ApiError::from_serialize)
}

pub async fn remove_trusted_sender(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
        email: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid remove_trusted_sender args: {e}")))?;
    let store = state.store.clone();
    run_blocking(move || store.remove_trusted_sender(&args.account_id, &args.email)).await?;
    Ok(Value::Null)
}
