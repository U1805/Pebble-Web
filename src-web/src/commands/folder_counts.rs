use serde::Deserialize;
use serde_json::Value;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn get_folder_unread_counts(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct Args {
        account_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid get_folder_unread_counts args: {e}")))?;
    let store = state.store.clone();
    let counts = run_blocking(move || store.get_folder_unread_counts(&args.account_id)).await?;
    serde_json::to_value(counts).map_err(ApiError::from_serialize)
}
