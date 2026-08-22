use pebble_core::traits::SearchHit;
use pebble_search::AdvancedSearchParams;
use serde_json::Value;

use crate::command::run_blocking;
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

/// 高级搜索（字段过滤）。入参由调用层将前端 AdvancedSearchQuery（camelCase）
/// 展开成顶层 snake_case 字段。
pub async fn advanced_search(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    // 与上游 Tauri 命令签名一致：advanced_search(query: AdvancedSearchQuery, limit)
    #[derive(serde::Deserialize)]
    struct Query {
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
    }
    let query: Query = serde_json::from_value(args.get("query").cloned().unwrap_or(Value::Null))
        .map_err(|e| ApiError::BadRequest(format!("invalid advanced_search args: {e}")))?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(50);
    let search = state.search.clone();
    let hits = run_blocking(move || {
        search.advanced_search(AdvancedSearchParams {
            text: query.text.as_deref(),
            from: query.from.as_deref(),
            to: query.to.as_deref(),
            subject: query.subject.as_deref(),
            date_from: query.date_from,
            date_to: query.date_to,
            has_attachment: query.has_attachment,
            folder_id: query.folder_id.as_deref(),
            limit,
        })
    })
    .await?;
    serde_json::to_value(hits).map_err(ApiError::from_serialize)
}
