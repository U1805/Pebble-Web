use pebble_core::traits::SearchHit;
use serde_json::Value;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 简单全文搜索（Tantivy 索引）。
pub async fn search_messages(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        query: String,
        #[serde(default)]
        limit: Option<usize>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid search_messages args: {e}")))?;
    if args.query.trim().is_empty() {
        return Ok(serde_json::to_value(Vec::<SearchHit>::new()).map_err(ApiError::from_serialize)?);
    }
    let search = state.search.clone();
    let limit = args.limit.unwrap_or(50);
    let hits = run_blocking(move || search.search(&args.query, limit)).await?;
    serde_json::to_value(hits).map_err(ApiError::from_serialize)
}
