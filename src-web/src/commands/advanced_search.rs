use pebble_search::AdvancedSearchParams;
use serde_json::Value;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

/// 高级搜索（字段过滤）。顶层 command 参数由 Web transport 转为 snake_case，
/// 嵌套 AdvancedSearchQuery 保持与 Tauri 相同的 camelCase Serde 契约。
pub async fn advanced_search(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    // 与上游 Tauri 命令签名一致：advanced_search(query: AdvancedSearchQuery, limit)
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
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
    #[derive(serde::Deserialize)]
    struct Args {
        query: Query,
        limit: Option<usize>,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid advanced_search args: {e}")))?;
    let query = args.query;
    let limit = args.limit.unwrap_or(50);
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
