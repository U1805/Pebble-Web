use pebble_core::traits::SearchHit;
use pebble_search::AdvancedSearchParams;
use serde_json::Value;

use crate::command::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 简单全文搜索（Tantivy 索引）。
pub async fn search_messages(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
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

/// 高级搜索（字段过滤）。字段命名与桌面端一致（camelCase 反序列化）。
pub async fn advanced_search(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        to: Option<String>,
        #[serde(default)]
        subject: Option<String>,
        #[serde(default)]
        date_from: Option<i64>,
        #[serde(default)]
        date_to: Option<i64>,
        #[serde(default)]
        has_attachment: Option<bool>,
        #[serde(default)]
        folder_id: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid advanced_search args: {e}")))?;
    let search = state.search.clone();
    let limit = args.limit.unwrap_or(50);
    let hits = run_blocking(move || {
        search.advanced_search(AdvancedSearchParams {
            text: args.text.as_deref(),
            from: args.from.as_deref(),
            to: args.to.as_deref(),
            subject: args.subject.as_deref(),
            date_from: args.date_from,
            date_to: args.date_to,
            has_attachment: args.has_attachment,
            folder_id: args.folder_id.as_deref(),
            limit,
        })
    })
    .await?;
    serde_json::to_value(hits).map_err(ApiError::from_serialize)
}